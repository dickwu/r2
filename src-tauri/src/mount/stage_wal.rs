use std::collections::{HashMap, VecDeque};
use std::io::{Error, ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex, OwnedRwLockReadGuard, RwLock};

use crate::providers::resources::{ByteLease, DiskLease, ResourceKind};

use super::{
    stage::PublicationGuard,
    stage::StageRecovery,
    stage::UploadSnapshot,
    stage::{sync_parent, write_json_atomic},
    stage_commit,
};

const MAGIC: &[u8; 4] = b"R2WL";
/// 3 added the watermark; nothing older ever shipped.
const VERSION: u16 = 3;
const HEADER_LEN: usize = 112;
const MAX_KEY_LEN: usize = 16 * 1024;
const MAX_DATA_NAME_LEN: usize = 1024;
const MAX_PAYLOAD_LEN: usize = 8 * 1024 * 1024;

/// Whole-file reads per WAL path. Keyed by path so tests running in parallel
/// on other staging folders cannot disturb a count.
#[cfg(test)]
static WAL_READS: OnceLock<std::sync::Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

#[cfg(test)]
fn note_wal_read(path: &Path) {
    *WAL_READS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default() += 1;
}

/// Data files whose restore replay fails, so tests can prove a stage that
/// could not be replayed is quarantined rather than restored with old bytes.
#[cfg(test)]
static FAILING_REPLAYS: OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    OnceLock::new();

#[cfg(test)]
pub fn fail_replay_of(data_path: &Path, fail: bool) {
    let mut failing = FAILING_REPLAYS
        .get_or_init(Default::default)
        .lock()
        .unwrap();
    if fail {
        failing.insert(data_path.to_path_buf());
    } else {
        failing.remove(data_path);
    }
}

#[cfg(test)]
pub fn wal_read_count(path: &Path) -> u64 {
    WAL_READS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(path)
        .copied()
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalOp {
    Write,
    Truncate,
    /// The stage was deleted: every earlier record of its data file is dead,
    /// and recovery must neither restore it nor rebuild its manifest.
    Discard,
}

impl WalOp {
    fn as_u8(self) -> u8 {
        match self {
            Self::Write => 1,
            Self::Truncate => 2,
            Self::Discard => 3,
        }
    }

    fn from_u8(value: u8) -> std::io::Result<Self> {
        match value {
            1 => Ok(Self::Write),
            2 => Ok(Self::Truncate),
            3 => Ok(Self::Discard),
            _ => Err(Error::new(ErrorKind::InvalidData, "unknown WAL operation")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WalRecord {
    pub lsn: u64,
    pub generation: u64,
    pub op: WalOp,
    pub offset: u64,
    pub resulting_size: u64,
    pub mtime_secs: u32,
    pub dirty_at_ms: i64,
    pub data_name: String,
    pub key: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct WalSummary {
    pub record: WalRecord,
    pub next_lsn: u64,
    pub truncated_tail: bool,
}

/// A folder's WAL as recovery sees it.
#[derive(Debug, Clone, Default)]
pub struct WalRecoveryIndex {
    /// Live records before the torn tail: discards and the records they
    /// cover are left out.
    buckets: HashMap<String, WalBucket>,
    /// Data file name to the LSN of its newest discard record.
    discarded: HashMap<String, u64>,
    truncated_tail: bool,
    damage: Vec<Damage>,
    max_lsn: u64,
}

impl WalRecoveryIndex {
    /// Set when acknowledged records were damaged (see `DecodedWal`).
    /// Recovery sets the WAL aside and quarantines only the files the damage
    /// may affect (`is_affected`); everything else recovers normally.
    pub fn damage(&self) -> Option<String> {
        self.damage.first().map(|damage| {
            format!(
                "Staging WAL is damaged at byte {} inside acknowledged records; recovery keeps it for export and quarantines the files it may affect",
                damage.offset
            )
        })
    }

    /// Whether acknowledged changes of the stage at `data_path`, beyond its
    /// manifest's checkpoint and generation, may have been in damaged bytes.
    ///
    /// A stage is unaffected when, for every damaged stretch, its checkpoint
    /// covers every LSN the stretch could hold, or its own intact records
    /// continue past the stretch with no generation missing: each durable
    /// change takes the next generation, so a lost record leaves a gap.
    pub fn is_affected(&self, data_path: &Path, checkpoint_lsn: u64, generation: u64) -> bool {
        if self.damage.is_empty() {
            return false;
        }
        let Ok(name) = data_name(data_path) else {
            return true;
        };
        if self.discarded.contains_key(&name) {
            return false;
        }
        let chain: Vec<&WalRecord> = self
            .buckets
            .get(&name)
            .map(|bucket| {
                bucket
                    .records
                    .iter()
                    .filter(|record| record.lsn > checkpoint_lsn && record.generation > generation)
                    .collect()
            })
            .unwrap_or_default();
        if !generations_unbroken(&chain, generation) {
            return true;
        }
        let last = chain.last().map(|record| record.lsn);
        self.damage.iter().any(|damage| match damage.lsn_after {
            Some(after) => {
                checkpoint_lsn.saturating_add(1) < after && last.is_none_or(|lsn| lsn < after)
            }
            None => true,
        })
    }

    /// Whether the stage was deleted. Data file names are unique per stage
    /// (`Stage::create` never reuses one), so a discard settles it for good.
    pub fn is_discarded(&self, data_path: &Path) -> bool {
        data_name(data_path).is_ok_and(|name| self.discarded.contains_key(&name))
    }

    pub fn summary_for_after(
        &self,
        data_path: &Path,
        checkpoint_lsn: u64,
        generation_floor: u64,
    ) -> std::io::Result<Option<WalSummary>> {
        let Some(bucket) = self.buckets.get(&data_name(data_path)?) else {
            return Ok(None);
        };
        let Some(record) = bucket
            .records
            .iter()
            .rev()
            .find(|record| record.lsn > checkpoint_lsn && record.generation > generation_floor)
            .cloned()
        else {
            return Ok(None);
        };
        Ok(Some(WalSummary {
            next_lsn: record.lsn.saturating_add(1),
            record,
            truncated_tail: self.truncated_tail,
        }))
    }

    /// Every data file with records in the WAL, with the key its newest
    /// record carries.
    pub fn stages(&self) -> impl Iterator<Item = (&str, &str)> {
        self.buckets.iter().filter_map(|(name, bucket)| {
            bucket
                .records
                .last()
                .map(|record| (name.as_str(), record.key.as_str()))
        })
    }

    pub fn uncheckpointed_bytes_for_after(
        &self,
        data_path: &Path,
        checkpoint_lsn: u64,
    ) -> std::io::Result<u64> {
        let Some(bucket) = self.buckets.get(&data_name(data_path)?) else {
            return Ok(0);
        };
        bucket
            .records
            .iter()
            .filter(|record| record.lsn > checkpoint_lsn)
            .map(estimated_record_len)
            .try_fold(0u64, |total, next| {
                next.map(|next| total.saturating_add(next))
            })
    }
}

#[derive(Debug, Clone, Default)]
struct WalBucket {
    records: Vec<WalRecord>,
    bytes: u64,
}

/// Whether a stage's records above its manifest — in LSN order — take every
/// generation after `generation` without skipping one. Retries of a failed
/// commit may repeat a generation; only a skipped one means a lost record.
fn generations_unbroken(chain: &[&WalRecord], generation: u64) -> bool {
    let mut expected = generation.saturating_add(1);
    for record in chain {
        if record.generation > expected {
            return false;
        }
        expected = expected.max(record.generation.saturating_add(1));
    }
    true
}

/// The LSN of every discard still in force: one that is the newest record of
/// its data file. A later record of the same file means that removal never
/// completed — its commit failed and the stage lived on — so the discard is
/// void, even if a rewrite made its bytes durable afterwards.
fn discards(records: &[WalRecord]) -> HashMap<String, u64> {
    let mut newest = HashMap::<&str, &WalRecord>::new();
    for record in records {
        let entry = newest.entry(record.data_name.as_str()).or_insert(record);
        if record.lsn > entry.lsn {
            *entry = record;
        }
    }
    newest
        .into_iter()
        .filter(|(_, record)| record.op == WalOp::Discard)
        .map(|(name, record)| (name.to_string(), record.lsn))
        .collect()
}

/// Whether a record belongs to a stage that was deleted after writing it.
fn is_dead(record: &WalRecord, discarded: &HashMap<String, u64>) -> bool {
    discarded
        .get(&record.data_name)
        .is_some_and(|&lsn| record.lsn < lsn)
}

pub fn wal_path(data_path: &Path) -> PathBuf {
    root_wal_path(data_path.parent().unwrap_or_else(|| Path::new(".")))
}

/// The WAL every stage in `root` shares.
pub fn root_wal_path(root: &Path) -> PathBuf {
    root.join(".stage.wal")
}

fn highwater_path(path: &Path) -> PathBuf {
    path.with_extension("wal.highwater")
}

#[derive(Debug)]
struct AppendState {
    next_lsn: u64,
    tail_valid: bool,
    /// Length of the WAL as this process last wrote or checked it.
    file_len: u64,
    /// Prefix an fsync has proven: every record appended carries it as its
    /// watermark (see `DecodedWal`).
    durable_len: u64,
    /// LSN and end offset of each appended record whose commit has not
    /// returned yet, in file order.
    pending: VecDeque<(u64, u64)>,
    /// Bytes of records no stage needs any more (checkpointed, or of deleted
    /// stages) still in the file until the next compaction. The WAL owns
    /// them in the resource accounting from the moment their stage lets go.
    reclaimable: ByteLease,
    compaction_scheduled: bool,
    /// Bumped whenever the file is cut or replaced, so a compaction that ran
    /// unlocked can tell its snapshot of the file is still the WAL.
    layout: u64,
    compacting: bool,
}

/// Dead bytes a WAL holds before it is compacted. Compaction also waits until
/// they are at least as many as the live bytes, so each O(WAL) rewrite is
/// paid for by at least that many bytes appended since the last one.
const COMPACT_MIN_RECLAIMABLE: u64 = 64 * 1024 * 1024;

#[cfg(test)]
static COMPACTION_FLOORS: OnceLock<std::sync::Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

/// Lowers the compaction threshold of one WAL, so tests need not write 64 MiB.
#[cfg(test)]
pub fn set_compaction_floor(path: &Path, bytes: u64) {
    COMPACTION_FLOORS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), bytes);
}

fn compaction_floor(path: &Path) -> u64 {
    #[cfg(test)]
    if let Some(bytes) = COMPACTION_FLOORS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(path)
    {
        return *bytes;
    }
    #[cfg(not(test))]
    let _ = path;
    COMPACT_MIN_RECLAIMABLE
}

impl AppendState {
    fn new(next_lsn: u64) -> Self {
        Self {
            next_lsn,
            tail_valid: false,
            file_len: 0,
            durable_len: 0,
            pending: VecDeque::new(),
            reclaimable: ByteLease::new(ResourceKind::Wal, 0),
            compaction_scheduled: false,
            layout: 0,
            compacting: false,
        }
    }

    fn compaction_due(&self, path: &Path) -> bool {
        let reclaimable = self.reclaimable.bytes();
        let live = self.file_len.saturating_sub(reclaimable);
        reclaimable > 0 && reclaimable >= compaction_floor(path).max(live)
    }

    /// The WAL was just cut, rewritten or checked, and `durable_len` of its
    /// `len` bytes are proven.
    fn relaid(&mut self, next_lsn: u64, len: u64, durable_len: u64) {
        self.next_lsn = self.next_lsn.max(next_lsn);
        self.tail_valid = true;
        self.file_len = len;
        self.durable_len = durable_len.min(len);
        self.pending.clear();
        self.layout = self.layout.wrapping_add(1);
    }
}

/// Hands `bytes` of a stage's records to the WAL's own account once no stage
/// needs them — checkpointed, or of a deleted stage — and schedules a
/// compaction once enough have piled up. It never compacts inline: callers
/// may hold the mount's stage locks, and compaction rewrites the whole WAL.
pub async fn note_reclaimable(path: &Path, bytes: u64) {
    let mut states = append_states().lock().await;
    let state = states
        .entry(path.to_path_buf())
        .or_insert_with(|| AppendState::new(1));
    state
        .reclaimable
        .resize(state.reclaimable.bytes().saturating_add(bytes));
    if state.compaction_scheduled || !state.compaction_due(path) {
        return;
    }
    state.compaction_scheduled = true;
    let path = path.to_path_buf();
    tokio::spawn(async move {
        if let Err(error) = compact_if_due(&path).await {
            log::warn!("mount: compacting the staging WAL failed: {}", error);
        }
    });
}

/// The operations of a data file's intact records in the WAL, in file order.
#[cfg(test)]
pub async fn ops_for(path: &Path, data_name: &str) -> Vec<WalOp> {
    let bytes = tokio::fs::read(path).await.unwrap_or_default();
    decode_records(&bytes)
        .records
        .into_iter()
        .filter(|record| record.data_name == data_name)
        .map(|record| record.op)
        .collect()
}

/// Compaction now, whatever the threshold says.
#[cfg(test)]
pub async fn compact_now(path: &Path) -> std::io::Result<bool> {
    compact(path).await
}

/// Compacts the WAL at `path` if its dead bytes have reached the threshold;
/// the background task `note_reclaimable` schedules runs this.
pub async fn compact_if_due(path: &Path) -> std::io::Result<bool> {
    {
        let mut states = append_states().lock().await;
        let Some(state) = states.get_mut(path) else {
            return Ok(false);
        };
        state.compaction_scheduled = false;
        if !state.compaction_due(path) {
            return Ok(false);
        }
    }
    compact(path).await
}

/// Raises the watermark once the commit of `lsn` has returned: its fsync
/// proved every byte up to the end of that record.
pub async fn note_committed(path: &Path, lsn: u64) {
    let mut states = append_states().lock().await;
    let Some(state) = states.get_mut(path) else {
        return;
    };
    while let Some(&(pending_lsn, end)) = state.pending.front() {
        if pending_lsn > lsn {
            break;
        }
        state.durable_len = state.durable_len.max(end);
        state.pending.pop_front();
    }
}

static APPEND_STATES: OnceLock<Mutex<HashMap<PathBuf, AppendState>>> = OnceLock::new();

fn append_states() -> &'static Mutex<HashMap<PathBuf, AppendState>> {
    APPEND_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One per WAL. An append holds the read side from before it writes until the
/// commit that acknowledges it has returned; a rewrite holds the write side.
static APPEND_GATES: OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<RwLock<()>>>>> =
    OnceLock::new();

/// Admission for one WAL append; drop it once the append's commit returned.
pub struct WalAppendGuard {
    _gate: OwnedRwLockReadGuard<()>,
}

/// Admits one append to the WAL at `path`.
///
/// After a failed fsync the commit worker refuses every acknowledgement of
/// this WAL. The first append to arrive then rewrites it (see
/// `rewrite_after_failed_sync`) while holding the gate exclusively, so no
/// record written to the old file can be acknowledged by an fsync of the new
/// one. Until a rewrite succeeds every append is refused with the error.
pub async fn begin_append(path: &Path) -> std::io::Result<WalAppendGuard> {
    let gate = APPEND_GATES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .entry(path.to_path_buf())
        .or_default()
        .clone();
    loop {
        let admitted = gate.clone().read_owned().await;
        let Some(failure) = stage_commit::poisoned(path) else {
            return Ok(WalAppendGuard { _gate: admitted });
        };
        drop(admitted);
        let _exclusive = gate.clone().write_owned().await;
        if stage_commit::poisoned(path).is_some() {
            rewrite_after_failed_sync(path).await.map_err(|error| {
                Error::other(format!(
                    "staging WAL fsync failed ({failure}) and rewriting it failed: {error}"
                ))
            })?;
        }
    }
}

/// Makes a WAL whose fsync failed trustworthy again: its valid records go to
/// a new file, which is fsynced, renamed over the old one and made durable
/// with a directory fsync; every data file with records in it is fsynced
/// again. Only then does the commit worker acknowledge this WAL again.
async fn rewrite_after_failed_sync(path: &Path) -> std::io::Result<()> {
    let mut states = append_states().lock().await;
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    note_wal_read(path);
    let decoded = decode_records(&bytes);
    // The unacknowledged tail goes; proven damage stays byte for byte, and so
    // does everything after it (a later record may be its only evidence).
    let keep = if decoded.damage.is_empty() {
        decoded.cut
    } else {
        decoded.len
    };
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "WAL path has no parent"))?;
    if !bytes.is_empty() {
        let temporary = path.with_extension("wal.tmp");
        let mut output = File::create(&temporary).await?;
        output.write_all(&bytes[..keep]).await?;
        output.flush().await?;
        stage_commit::injected_sync_failure(&temporary)?;
        output.sync_all().await?;
        stage_commit::record_file_sync_bytes(keep as u64);
        drop(output);
        tokio::fs::rename(&temporary, path).await?;
    }
    sync_parent(path).await?;
    let names: std::collections::BTreeSet<&str> = decoded
        .records
        .iter()
        .map(|record| record.data_name.as_str())
        .collect();
    for name in names {
        let data = parent.join(name);
        match OpenOptions::from(stage_commit::sync_open_options())
            .open(&data)
            .await
        {
            Ok(file) => {
                stage_commit::injected_sync_failure(&data)?;
                file.sync_all().await?;
                stage_commit::record_file_sync_bytes(file.metadata().await?.len());
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let next_lsn = decoded.max_lsn.saturating_add(1);
    states
        .entry(path.to_path_buf())
        .or_insert_with(|| AppendState::new(next_lsn))
        .relaid(next_lsn, keep as u64, keep as u64);
    stage_commit::clear_poison(path);
    Ok(())
}

/// What a process restart does to the in-memory append state; tests that
/// damage a WAL on disk call it before the next append, as a restart would.
#[cfg(test)]
pub async fn forget_append_state(path: &Path) {
    append_states().lock().await.remove(path);
}

pub fn data_name(data_path: &Path) -> std::io::Result<String> {
    data_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "stage data path has no file name"))
}

pub fn estimated_record_len(record: &WalRecord) -> std::io::Result<u64> {
    if record.data_name.len() > MAX_DATA_NAME_LEN {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "WAL data filename is too large",
        ));
    }
    if record.key.len() > MAX_KEY_LEN {
        return Err(Error::new(ErrorKind::InvalidInput, "WAL key is too large"));
    }
    if record.payload.len() > MAX_PAYLOAD_LEN {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "WAL payload is too large",
        ));
    }
    Ok((HEADER_LEN + record.data_name.len() + record.key.len() + record.payload.len()) as u64)
}

#[cfg(test)]
pub async fn append_record(path: &Path, record: &WalRecord) -> std::io::Result<u64> {
    append_record_unchecked(path, record).await
}

pub async fn append_record_unchecked(path: &Path, record: &WalRecord) -> std::io::Result<u64> {
    let mut states = append_states().lock().await;
    let state = states
        .entry(path.to_path_buf())
        .or_insert_with(|| AppendState::new(1));
    if !state.tail_valid {
        let repaired = repair_tail_and_next_lsn(path).await?;
        state.relaid(repaired.next_lsn, repaired.len, repaired.durable_len);
    }

    let assigned_lsn = state.next_lsn.max(record.lsn);
    state.next_lsn = assigned_lsn.saturating_add(1);
    state.tail_valid = false;

    let mut assigned = record.clone();
    assigned.lsn = assigned_lsn;
    let bytes = encode_stamped(&assigned, state.durable_len)?;
    let result = async {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        file.write_all(&bytes).await?;
        file.flush().await
    }
    .await;
    match result {
        Ok(()) => {
            state.tail_valid = true;
            state.file_len = state.file_len.saturating_add(bytes.len() as u64);
            state.pending.push_back((assigned_lsn, state.file_len));
            Ok(assigned_lsn)
        }
        Err(error) => Err(error),
    }
}

pub async fn uncheckpointed_bytes_for(
    data_path: &Path,
    checkpoint_lsn: u64,
) -> std::io::Result<u64> {
    let wal = read_root_wal(data_path.parent().unwrap_or_else(|| Path::new("."))).await?;
    let name = data_name(data_path)?;
    wal.buckets
        .get(&name)
        .map(|bucket| {
            bucket
                .records
                .iter()
                .filter(|record| record.lsn > checkpoint_lsn)
                .map(estimated_record_len)
                .try_fold(0u64, |total, next| {
                    next.map(|next| total.saturating_add(next))
                })
        })
        .unwrap_or(Ok(0))
}

#[cfg(test)]
async fn replay_file(data_path: &Path, checkpoint_lsn: u64) -> std::io::Result<Option<WalSummary>> {
    replay_file_after_generation(data_path, checkpoint_lsn, 0).await
}

pub async fn replay_file_after_generation(
    data_path: &Path,
    checkpoint_lsn: u64,
    generation_floor: u64,
) -> std::io::Result<Option<WalSummary>> {
    let path = wal_path(data_path);
    let mut file = match File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    note_wal_read(&path);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let data_name = data_name(data_path)?;
    let decoded = decode_records(&bytes);
    let truncated_tail = decoded.torn();
    let records: Vec<WalRecord> = decoded
        .records
        .into_iter()
        .filter(|record| {
            record.data_name == data_name
                && record.op != WalOp::Discard
                && record.lsn > checkpoint_lsn
                && record.generation > generation_floor
        })
        .collect();
    if !decoded.damage.is_empty()
        && !generations_unbroken(&records.iter().collect::<Vec<_>>(), generation_floor)
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "A record of this file is in a damaged part of the staging WAL",
        ));
    }
    let mut last = None;
    for record in records {
        apply_record(data_path, &record).await?;
        last = Some(record);
    }
    let Some(record) = last else {
        return Ok(None);
    };
    Ok(Some(WalSummary {
        next_lsn: record.lsn.saturating_add(1),
        record,
        truncated_tail,
    }))
}

pub async fn repair_tail(path: &Path) -> std::io::Result<()> {
    let mut states = append_states().lock().await;
    // Once one append in this process has seen or repaired the tail, every
    // later append keeps it valid; rereading the WAL again is pure cost.
    if states.get(path).is_some_and(|state| state.tail_valid) {
        return Ok(());
    }
    let repaired = repair_tail_and_next_lsn(path).await?;
    states
        .entry(path.to_path_buf())
        .or_insert_with(|| AppendState::new(repaired.next_lsn))
        .relaid(repaired.next_lsn, repaired.len, repaired.durable_len);
    Ok(())
}

struct Repaired {
    next_lsn: u64,
    len: u64,
    durable_len: u64,
}

async fn repair_tail_and_next_lsn(path: &Path) -> std::io::Result<Repaired> {
    let highwater = read_highwater(path).await?;
    let mut file = match OpenOptions::from(stage_commit::sync_open_options())
        .open(path)
        .await
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Repaired {
                next_lsn: highwater.unwrap_or(1),
                len: 0,
                durable_len: 0,
            })
        }
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    note_wal_read(path);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let decoded = decode_records(&bytes);
    // Not even a record that was cut may have its LSN handed out again.
    let next_lsn = highwater
        .unwrap_or(1)
        .max(decoded.max_lsn.saturating_add(1));
    // Proven damage is never cut, nor is anything after it: a later record
    // may be the only evidence of it. New records simply follow.
    if decoded.torn() && decoded.damage.is_empty() {
        file.set_len(decoded.cut as u64).await?;
        let synced = async {
            file.sync_all().await?;
            stage_commit::record_file_sync_bytes(decoded.cut as u64);
            sync_parent(path).await
        }
        .await;
        if let Err(error) = synced {
            // The cut may not be durable: after power loss the dead tail could
            // come back in front of records appended behind it.
            stage_commit::poison(path, &error);
            return Err(error);
        }
        return Ok(Repaired {
            next_lsn,
            len: decoded.cut as u64,
            durable_len: decoded.cut as u64,
        });
    }
    Ok(Repaired {
        next_lsn,
        len: decoded.len as u64,
        durable_len: decoded.proven_len as u64,
    })
}

async fn read_highwater(path: &Path) -> std::io::Result<Option<u64>> {
    match tokio::fs::read_to_string(highwater_path(path)).await {
        Ok(text) => text
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn persist_highwater(path: &Path, next_lsn: u64) -> std::io::Result<()> {
    let highwater = highwater_path(path);
    let temporary = highwater.with_extension("highwater.tmp");
    let mut file = File::create(&temporary).await?;
    file.write_all(next_lsn.to_string().as_bytes()).await?;
    file.write_all(
        b"
",
    )
    .await?;
    file.sync_all().await?;
    stage_commit::record_file_sync_bytes(next_lsn.to_string().len() as u64 + 1);
    drop(file);
    tokio::fs::rename(&temporary, &highwater).await?;
    sync_parent(&highwater).await
}

pub async fn recovery_index(root: &Path) -> Result<WalRecoveryIndex, String> {
    read_root_wal(root).await.map_err(|e| e.to_string())
}

/// Prefix of the name a damaged WAL is kept under once recovery set it aside.
pub const DAMAGED_WAL_PREFIX: &str = ".stage.wal.damaged-";

/// Restore-time replay of the whole folder.
///
/// Damaged acknowledged records never block the folder: the stages the
/// damage may affect are quarantined in their manifests, every other stage
/// is replayed as usual, and the WAL is renamed aside (kept for export) so
/// new writes start a fresh one — unless a replay failed. Then the WAL is
/// the only copy of that stage's acknowledged records and stays where it is:
/// `recovery_entries` reports the stage as `replay_pending` from it, restore
/// quarantines it with its replay error, and the next restore tries again.
pub async fn replay_all(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut errors: Vec<(PathBuf, String)> = Vec::new();
    let index = read_root_wal(root).await.map_err(|e| e.to_string())?;
    let set_aside = (!index.damage.is_empty()).then(|| {
        root.join(format!(
            "{DAMAGED_WAL_PREFIX}{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    });
    let affected = match &set_aside {
        Some(target) => quarantine_affected(root, &index, target).await?,
        None => std::collections::HashSet::new(),
    };
    let truncated_tail = index.truncated_tail;
    let max_lsn = index.max_lsn;
    let mut dead_bytes = 0u64;
    for (name, bucket) in index.buckets {
        if affected.contains(&name) {
            continue;
        }
        let data_path = root.join(&name);
        let manifest_path = data_path.with_extension("stage.json");
        let error_key = replay_error_key(&data_path);
        let existing = match read_manifest(&manifest_path).await {
            Ok(record) => record,
            Err(error) => {
                errors.push((error_key, error.to_string()));
                continue;
            }
        };
        let checkpoint_lsn = existing.as_ref().map_or(0, |record| record.checkpoint_lsn);
        let generation_floor = existing.as_ref().map_or(0, |record| record.generation);
        let bucket_bytes = bucket.bytes;
        let checkpointed_bytes: u64 = bucket
            .records
            .iter()
            .filter(|record| record.lsn <= checkpoint_lsn)
            .map(|record| estimated_record_len(record).unwrap_or(0))
            .sum();
        match replay_bucket(
            &data_path,
            bucket,
            checkpoint_lsn,
            generation_floor,
            truncated_tail,
        )
        .await
        {
            Ok(Some(summary)) => {
                let recovery = match merge_summary(data_path.clone(), existing, summary) {
                    Ok(recovery) => recovery,
                    Err(error) => {
                        errors.push((error_key, error.to_string()));
                        continue;
                    }
                };
                match write_json_atomic(&manifest_path, &recovery).await {
                    // The manifest now checkpoints every record of the file.
                    Ok(()) => dead_bytes = dead_bytes.saturating_add(bucket_bytes),
                    Err(error) => errors.push((error_key, error.to_string())),
                }
            }
            Ok(None) => dead_bytes = dead_bytes.saturating_add(checkpointed_bytes),
            Err(error) => errors.push((error_key, error.to_string())),
        }
    }
    if set_aside.is_none() && dead_bytes > 0 {
        note_reclaimable(&root_wal_path(root), dead_bytes).await;
    }
    // Every error above is a stage whose records were not folded into its
    // manifest; they stay only in this WAL, so it must not be set aside yet.
    let replayed_all = errors.is_empty();
    if let Some(target) = set_aside {
        if replayed_all {
            set_aside_wal(root, &target, max_lsn)
                .await
                .map_err(|e| e.to_string())?;
            errors.push((
                target,
                "Damaged staging WAL kept for export and review".into(),
            ));
        }
    }
    Ok(errors)
}

/// Marks every stage the damage may affect as unreadable in its manifest —
/// creating one for a stage known only from the WAL whose data file is still
/// there — so the quarantine outlives the WAL, which is about to be set
/// aside. Returns their data file names.
async fn quarantine_affected(
    root: &Path,
    index: &WalRecoveryIndex,
    set_aside: &Path,
) -> Result<std::collections::HashSet<String>, String> {
    let reason = format!(
        "Acknowledged changes to this file may be in a damaged part of the staging WAL, kept as {} for export and review",
        set_aside
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    let mut names: std::collections::BTreeSet<String> = index.buckets.keys().cloned().collect();
    let mut dir = tokio::fs::read_dir(root).await.map_err(|e| e.to_string())?;
    while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
        if !entry.file_name().to_string_lossy().ends_with(".stage.json") {
            continue;
        }
        if let Ok(Some(manifest)) = read_manifest(&entry.path()).await {
            if let Some(name) = manifest.path.file_name() {
                names.insert(name.to_string_lossy().into_owned());
            }
        }
    }
    let mut affected = std::collections::HashSet::new();
    for name in names {
        let data_path = root.join(&name);
        let manifest_path = data_path.with_extension("stage.json");
        let manifest = read_manifest(&manifest_path).await.ok().flatten();
        let (checkpoint_lsn, generation) = manifest
            .as_ref()
            .map_or((0, 0), |record| (record.checkpoint_lsn, record.generation));
        if !index.is_affected(&data_path, checkpoint_lsn, generation) {
            continue;
        }
        affected.insert(name.clone());
        let quarantined = match manifest {
            Some(record) => Some(StageRecovery {
                dirty: true,
                state: "unreadable".into(),
                error: Some(reason.clone()),
                ..record
            }),
            None => match tokio::fs::metadata(&data_path).await {
                Ok(metadata) => {
                    let last = index
                        .buckets
                        .get(&name)
                        .and_then(|bucket| bucket.records.last());
                    Some(StageRecovery {
                        key: last.map(|record| record.key.clone()).unwrap_or_default(),
                        size: metadata.len(),
                        mtime_secs: last.map_or(0, |record| record.mtime_secs),
                        dirty: true,
                        state: "unreadable".into(),
                        error: Some(reason.clone()),
                        first_dirty_at: last.map(|record| record.dirty_at_ms),
                        ..recovery_placeholder(&data_path)
                    })
                }
                Err(_) => None,
            },
        };
        if let Some(record) = quarantined {
            write_json_atomic(&manifest_path, &record)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(affected)
}

fn recovery_placeholder(path: &Path) -> StageRecovery {
    StageRecovery {
        key: String::new(),
        size: 0,
        mtime_secs: 0,
        generation: 0,
        dirty: false,
        state: String::new(),
        error: None,
        path: path.to_path_buf(),
        snapshot: None,
        publication_guard: None,
        checkpoint_lsn: 0,
        first_dirty_at: None,
        wal_bytes: None,
    }
}

/// Renames the damaged WAL aside and lets the next append start a fresh one,
/// its LSNs above every LSN the damaged copy holds.
async fn set_aside_wal(root: &Path, target: &Path, max_lsn: u64) -> std::io::Result<()> {
    let wal = root_wal_path(root);
    let mut states = append_states().lock().await;
    let next_lsn = read_highwater(&wal)
        .await?
        .unwrap_or(1)
        .max(max_lsn.saturating_add(1));
    persist_highwater(&wal, next_lsn).await?;
    tokio::fs::rename(&wal, target).await?;
    sync_parent(&wal).await?;
    states.remove(&wal);
    stage_commit::clear_poison(&wal);
    Ok(())
}

/// Where a stage's replay error is reported. `restore_stages` looks the error
/// of every `replay_pending` record up under `<data>.write.json` — the name a
/// legacy JSON intent for the same data file has — so a WAL replay failure is
/// reported the same way instead of being dropped.
pub fn replay_error_key(data_path: &Path) -> PathBuf {
    data_path.with_extension("write.json")
}

async fn read_root_wal(root: &Path) -> std::io::Result<WalRecoveryIndex> {
    let path = root_wal_path(root);
    let mut file = match File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(WalRecoveryIndex::default()),
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    note_wal_read(&path);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let decoded = decode_records(&bytes);
    let truncated_tail = decoded.torn();
    let discarded = discards(&decoded.records);
    let mut buckets = HashMap::<String, WalBucket>::new();
    for record in decoded.records {
        if record.op == WalOp::Discard || is_dead(&record, &discarded) {
            continue;
        }
        let len = estimated_record_len(&record)?;
        let bucket = buckets.entry(record.data_name.clone()).or_default();
        bucket.bytes = bucket.bytes.saturating_add(len);
        bucket.records.push(record);
    }
    for bucket in buckets.values_mut() {
        bucket.records.sort_by_key(|record| record.lsn);
    }
    Ok(WalRecoveryIndex {
        buckets,
        discarded,
        truncated_tail,
        damage: decoded.damage,
        max_lsn: decoded.max_lsn,
    })
}

async fn replay_bucket(
    data_path: &Path,
    bucket: WalBucket,
    checkpoint_lsn: u64,
    generation_floor: u64,
    truncated_tail: bool,
) -> std::io::Result<Option<WalSummary>> {
    let mut records: Vec<_> = bucket
        .records
        .into_iter()
        .filter(|record| record.lsn > checkpoint_lsn && record.generation > generation_floor)
        .collect();
    if records.is_empty() {
        return Ok(None);
    }
    #[cfg(test)]
    if FAILING_REPLAYS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .contains(data_path)
    {
        return Err(Error::other("injected replay failure"));
    }
    let mut file = OpenOptions::from(stage_commit::sync_open_options())
        .open(data_path)
        .await?;
    for record in &records {
        apply_record_to_open_file(&mut file, record).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    if let Some(record) = records.last() {
        stage_commit::record_file_sync_bytes(record.resulting_size);
    }
    let record = records.pop().expect("records checked non-empty");
    Ok(Some(WalSummary {
        next_lsn: record.lsn.saturating_add(1),
        record,
        truncated_tail,
    }))
}

async fn read_manifest(path: &Path) -> std::io::Result<Option<StageRecovery>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(std::io::Error::other),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Who still needs a data file's records, judged by what is on disk.
enum RecordOwner {
    /// A manifest: records at or below its checkpoint are in the data file.
    Manifest { checkpoint_lsn: u64 },
    /// No manifest but the data file: records still wait for replay.
    DataOnly,
    /// Neither: the stage was deleted and its records can go.
    Gone,
    /// An unreadable manifest: keep everything.
    Unknown,
}

/// Rewrites the WAL without the records no stage can need any more: those at
/// or below their stage's durable checkpoint, those of deleted stages, and
/// the discards of deletions whose files are all gone. What may go is judged
/// from the manifests and data files on disk, so it holds whatever state the
/// live stages are in. A discard alone drops nothing: while a file of its
/// stage remains the removal may not have completed, the stage may live on,
/// and its next write voids the discard. A WAL with proven damage is left for
/// recovery to set aside: dropping records around the damage could drop its
/// only evidence. A WAL whose fsync failed is left for the rewrite that makes
/// it trustworthy again (`rewrite_after_failed_sync`).
///
/// The O(WAL) part runs without the append lock — the WAL only grows between
/// layout changes, so its first bytes stay put — and appends keep flowing.
/// The lock is taken only to copy over what was appended meanwhile and swap
/// the files; if anything cut or replaced the WAL in between, it gives up.
async fn compact(path: &Path) -> std::io::Result<bool> {
    let root = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "WAL path has no parent"))?;
    let (layout, len) = {
        let mut states = append_states().lock().await;
        if stage_commit::poisoned(path).is_some() {
            return Ok(false);
        }
        match states.get_mut(path) {
            Some(state) if state.tail_valid && !state.compacting => {
                state.compacting = true;
                (state.layout, state.file_len)
            }
            _ => return Ok(false),
        }
    };
    let temporary = path.with_extension("wal.compact");
    let result = compact_unlocked(path, root, &temporary, layout, len).await;
    let _ = tokio::fs::remove_file(&temporary).await;
    if let Some(state) = append_states().lock().await.get_mut(path) {
        state.compacting = false;
    }
    result
}

async fn compact_unlocked(
    path: &Path,
    root: &Path,
    temporary: &Path,
    layout: u64,
    len: u64,
) -> std::io::Result<bool> {
    let mut bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let Ok(len) = usize::try_from(len) else {
        return Ok(false);
    };
    if bytes.len() < len {
        return Ok(false);
    }
    bytes.truncate(len);
    #[cfg(test)]
    note_wal_read(path);
    let decoded = decode_records(&bytes);
    if !decoded.damage.is_empty() || decoded.torn() {
        return Ok(false);
    }
    let discarded = discards(&decoded.records);
    let mut owners = HashMap::<String, RecordOwner>::new();
    for record in &decoded.records {
        if owners.contains_key(&record.data_name) {
            continue;
        }
        let data = root.join(&record.data_name);
        let owner = match read_manifest(&data.with_extension("stage.json")).await {
            Ok(Some(manifest)) => RecordOwner::Manifest {
                checkpoint_lsn: manifest.checkpoint_lsn,
            },
            Ok(None) => match tokio::fs::symlink_metadata(&data).await {
                Ok(_) => RecordOwner::DataOnly,
                Err(error) if error.kind() == ErrorKind::NotFound => RecordOwner::Gone,
                Err(_) => RecordOwner::Unknown,
            },
            Err(_) => RecordOwner::Unknown,
        };
        owners.insert(record.data_name.clone(), owner);
    }
    let total = decoded.records.len();
    let past_every_record = decoded.max_lsn.saturating_add(1);
    let retained: Vec<_> = decoded
        .records
        .into_iter()
        .filter(|record| {
            if record.op == WalOp::Discard && discarded.get(&record.data_name) != Some(&record.lsn)
            {
                // A void discard: its removal never completed.
                return false;
            }
            // Records under a discard in force go only with their stage's
            // files, never on the discard's word alone.
            match owners.get(&record.data_name) {
                Some(RecordOwner::Gone) => false,
                // An unfinished deletion keeps its discard.
                Some(RecordOwner::Manifest { checkpoint_lsn }) => {
                    record.op == WalOp::Discard || record.lsn > *checkpoint_lsn
                }
                _ => true,
            }
        })
        .collect();
    if retained.len() == total {
        // The estimate that scheduled this was stale; nothing can go.
        if let Some(state) = append_states().lock().await.get_mut(path) {
            state.reclaimable.resize(0);
        }
        return Ok(false);
    }
    let prefix_len: u64 = retained
        .iter()
        .map(estimated_record_len)
        .try_fold(0u64, |total, next| {
            next.map(|next| total.saturating_add(next))
        })?;
    let growth = DiskLease::reserve(root, prefix_len, || super::available_space(root))?;
    let mut output = File::create(temporary).await?;
    for record in &retained {
        // The new prefix is fsynced before it can become the WAL, so every
        // record in it may vouch for all of it.
        output
            .write_all(&encode_stamped(record, prefix_len)?)
            .await?;
    }
    output.flush().await?;
    output.sync_all().await?;
    stage_commit::record_file_sync_bytes(prefix_len);

    let mut states = append_states().lock().await;
    let Some(state) = states.get_mut(path) else {
        return Ok(false);
    };
    if state.layout != layout || !state.tail_valid || stage_commit::poisoned(path).is_some() {
        return Ok(false);
    }
    // Records appended while the prefix was being rewritten, carried over as
    // they are (their watermarks restamped for the new file).
    let appended_len = state.file_len.saturating_sub(len as u64);
    let mut appended = vec![0u8; usize::try_from(appended_len).unwrap_or(usize::MAX)];
    if appended_len > 0 {
        let mut wal = File::open(path).await?;
        wal.seek(SeekFrom::Start(len as u64)).await?;
        wal.read_exact(&mut appended).await?;
    }
    let tail = decode_records(&appended);
    if tail.torn() || !tail.damage.is_empty() {
        return Ok(false);
    }
    let final_len = prefix_len.saturating_add(appended_len);
    for record in &tail.records {
        output
            .write_all(&encode_stamped(record, final_len)?)
            .await?;
    }
    output.flush().await?;
    output.sync_all().await?;
    stage_commit::record_file_sync_bytes(appended_len);
    drop(output);
    // The highwater must stay past every LSN ever written, including the
    // records being dropped, so no LSN is handed out twice.
    let next_lsn = state
        .next_lsn
        .max(past_every_record)
        .max(tail.max_lsn.saturating_add(1));
    persist_highwater(path, next_lsn).await?;
    if final_len == 0 {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    } else {
        tokio::fs::rename(temporary, path).await?;
    }
    sync_replaced_entry(path).await?;
    drop(growth);
    state.relaid(next_lsn, final_len, final_len);
    state.reclaimable.resize(0);
    Ok(true)
}

/// Directory fsync after the WAL's name was pointed at a new file or removed.
/// If it fails, appends to the new file could vanish with the entry after
/// power loss, so the WAL refuses acknowledgements until it is rewritten.
async fn sync_replaced_entry(path: &Path) -> std::io::Result<()> {
    let result = sync_parent(path).await;
    if let Err(error) = &result {
        stage_commit::poison(path, error);
    }
    result
}

fn recovery_from_summary(path: PathBuf, summary: WalSummary) -> StageRecovery {
    StageRecovery {
        key: summary.record.key,
        size: summary.record.resulting_size,
        mtime_secs: summary.record.mtime_secs,
        generation: summary.record.generation,
        dirty: true,
        state: if summary.truncated_tail {
            "waiting_torn_tail".into()
        } else {
            "waiting".into()
        },
        error: None,
        path,
        snapshot: None::<UploadSnapshot>,
        publication_guard: None::<PublicationGuard>,
        checkpoint_lsn: summary.record.lsn,
        first_dirty_at: Some(summary.record.dirty_at_ms),
        wal_bytes: None,
    }
}

fn merge_summary(
    path: PathBuf,
    existing: Option<StageRecovery>,
    summary: WalSummary,
) -> std::io::Result<StageRecovery> {
    let mut record =
        existing.unwrap_or_else(|| recovery_from_summary(path.clone(), summary.clone()));
    if record.key != summary.record.key {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "WAL key does not match durable stage manifest",
        ));
    }
    record.path = path;
    record.size = summary.record.resulting_size;
    record.mtime_secs = summary.record.mtime_secs;
    record.generation = summary.record.generation;
    record.dirty = true;
    record.checkpoint_lsn = summary.record.lsn;
    if record.first_dirty_at.is_none() {
        record.first_dirty_at = Some(summary.record.dirty_at_ms);
    }
    if summary.truncated_tail && record.error.is_none() {
        record.error = Some("WAL ended with an incomplete unacknowledged record".into());
    }
    Ok(record)
}

async fn apply_record(data_path: &Path, record: &WalRecord) -> std::io::Result<()> {
    let mut file = OpenOptions::from(stage_commit::sync_open_options())
        .open(data_path)
        .await?;
    apply_record_to_open_file(&mut file, record).await?;
    file.flush().await?;
    stage_commit::injected_sync_failure(data_path)?;
    file.sync_all().await?;
    stage_commit::record_file_sync_bytes(record.resulting_size);
    Ok(())
}

async fn apply_record_to_open_file(file: &mut File, record: &WalRecord) -> std::io::Result<()> {
    match record.op {
        WalOp::Write => {
            file.seek(SeekFrom::Start(record.offset)).await?;
            file.write_all(&record.payload).await?;
            file.set_len(record.resulting_size).await?;
        }
        WalOp::Truncate => file.set_len(record.resulting_size).await?,
        // Never reaches a data file: replay skips discards and what they cover.
        WalOp::Discard => {}
    }
    Ok(())
}

/// A record that claims no durable prefix, as tests build them.
#[cfg(test)]
fn encode_record(record: &WalRecord) -> std::io::Result<Vec<u8>> {
    encode_stamped(record, 0)
}

/// Encodes a record carrying `watermark`: the length of the WAL prefix an
/// fsync had proven when it was written (see `decode_records`).
fn encode_stamped(record: &WalRecord, watermark: u64) -> std::io::Result<Vec<u8>> {
    if record.op != WalOp::Write && !record.payload.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "only a write WAL record can have a payload",
        ));
    }
    encode_raw(record, record.op.as_u8(), watermark)
}

/// A valid record with an operation byte this build does not define, as a
/// newer build would write one.
#[cfg(test)]
fn encode_with_op(record: &WalRecord, op: u8) -> Vec<u8> {
    encode_raw(record, op, 0).unwrap()
}

/// Record layout, format 3, little endian: 0..4 magic, 4..6 version, 6 op,
/// 7 reserved, 8..16 LSN, 16..24 generation, 24..32 offset, 32..40 resulting
/// size, 40..44 mtime, 44..48 key length, 48..56 payload length, 56..60 data
/// name length, 60..92 SHA-256, 92..100 dirty-since ms, 100..108 watermark,
/// 108..112 reserved; then data name, key and payload. The checksum covers
/// the whole record with its own field zeroed.
fn encode_raw(record: &WalRecord, op: u8, watermark: u64) -> std::io::Result<Vec<u8>> {
    if record.data_name.len() > MAX_DATA_NAME_LEN {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "WAL data filename is too large",
        ));
    }
    if record.key.len() > MAX_KEY_LEN {
        return Err(Error::new(ErrorKind::InvalidInput, "WAL key is too large"));
    }
    if record.payload.len() > MAX_PAYLOAD_LEN {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "WAL payload is too large",
        ));
    }
    let data_name = record.data_name.as_bytes();
    let key = record.key.as_bytes();
    let mut bytes =
        Vec::with_capacity(HEADER_LEN + data_name.len() + key.len() + record.payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.push(op);
    bytes.push(0);
    bytes.extend_from_slice(&record.lsn.to_le_bytes());
    bytes.extend_from_slice(&record.generation.to_le_bytes());
    bytes.extend_from_slice(&record.offset.to_le_bytes());
    bytes.extend_from_slice(&record.resulting_size.to_le_bytes());
    bytes.extend_from_slice(&record.mtime_secs.to_le_bytes());
    bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(record.payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&(data_name.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0u8; 32]);
    bytes.extend_from_slice(&record.dirty_at_ms.to_le_bytes());
    bytes.extend_from_slice(&watermark.to_le_bytes());
    bytes.resize(HEADER_LEN, 0);
    bytes.extend_from_slice(data_name);
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(&record.payload);
    let checksum = record_checksum(&bytes[..HEADER_LEN], &bytes[HEADER_LEN..]);
    bytes[60..92].copy_from_slice(&checksum);
    Ok(bytes)
}

fn record_checksum(header: &[u8], body: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(&header[..60]);
    hasher.update([0u8; 32]);
    hasher.update(&header[92..HEADER_LEN]);
    hasher.update(body);
    hasher.finalize().into()
}

/// Bytes that are not an intact record but were proven durable when an
/// intact record was written, or an intact record this build cannot read.
/// Neither is ever cut. Whatever records these bytes held had LSNs below
/// `lsn_after`, the first intact record after them (if there is one).
#[derive(Debug, Clone)]
pub struct Damage {
    offset: usize,
    lsn_after: Option<u64>,
}

/// A WAL as read back from disk.
///
/// Every record carries a watermark: the prefix an fsync had proven when it
/// was appended. Group commit fsyncs in file order, but an unsynced batch can
/// reach the disk out of order after a power cut — a hole, then intact
/// records. Invalid bytes that a watermark covers were durable before they
/// went bad: real damage, kept, reported and never cut. Invalid bytes no
/// watermark covers are treated as the torn tail and cut at `cut`, together
/// with every record after them: a power cut cannot lose bytes an fsync had
/// covered, so if anything after them had been acknowledged, the bytes
/// themselves were durable too and damaged later. The one case that looks
/// identical is exactly that — media damage inside the final group commit
/// before the crash, with no record appended after that commit returned to
/// carry its watermark — and it is cut as a torn tail.
struct DecodedWal {
    /// Intact records before `cut`, in file order.
    records: Vec<WalRecord>,
    /// Where the unacknowledged tail starts; the file length if it has none.
    cut: usize,
    len: usize,
    damage: Vec<Damage>,
    /// Highest LSN of any intact record, including those behind `cut`.
    max_lsn: u64,
    /// Longest prefix any watermark proves durable.
    proven_len: usize,
}

impl DecodedWal {
    fn torn(&self) -> bool {
        self.cut < self.len
    }
}

enum RecordAt {
    Valid {
        record: WalRecord,
        len: usize,
        watermark: u64,
    },
    /// An intact record from a newer build: a later format version, or the
    /// current one with an operation this build does not define.
    Unsupported,
    Invalid,
}

fn decode_records(bytes: &[u8]) -> DecodedWal {
    // Every intact record, resynchronising after each invalid stretch. LSNs
    // rise in file order, so an intact record with an older LSN can only be
    // payload bytes that happen to hold a WAL record.
    let mut intact: Vec<(usize, WalRecord, u64)> = Vec::new();
    let mut gaps: Vec<(usize, bool)> = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let last_lsn = intact.last().map(|(_, record, _)| record.lsn);
        match decode_record_at(bytes, offset) {
            RecordAt::Valid {
                record,
                len,
                watermark,
            } if last_lsn.is_none_or(|lsn| record.lsn > lsn) => {
                intact.push((offset, record, watermark));
                offset += len;
            }
            other => {
                gaps.push((offset, matches!(other, RecordAt::Unsupported)));
                offset = next_record_after(bytes, offset + 1, last_lsn).unwrap_or(bytes.len());
            }
        }
    }
    let proven_len = intact
        .iter()
        .map(|(_, _, watermark)| usize::try_from(*watermark).unwrap_or(usize::MAX))
        .max()
        .unwrap_or(0)
        .min(bytes.len());
    let mut cut = bytes.len();
    let mut damage = Vec::new();
    for &(gap, unsupported) in &gaps {
        if !unsupported && gap >= proven_len {
            cut = gap;
            break;
        }
        damage.push(Damage {
            offset: gap,
            lsn_after: intact
                .iter()
                .find(|(offset, _, _)| *offset > gap)
                .map(|(_, record, _)| record.lsn),
        });
    }
    let max_lsn = intact
        .iter()
        .map(|(_, record, _)| record.lsn)
        .max()
        .unwrap_or(0);
    DecodedWal {
        records: intact
            .into_iter()
            .filter(|(offset, _, _)| *offset < cut)
            .map(|(_, record, _)| record)
            .collect(),
        cut,
        len: bytes.len(),
        damage,
        max_lsn,
        proven_len,
    }
}

/// The next offset at or after `from` where an intact record newer than
/// `last_lsn` starts.
fn next_record_after(bytes: &[u8], from: usize, last_lsn: Option<u64>) -> Option<usize> {
    let mut start = from;
    while let Some(found) = bytes
        .get(start..)
        .and_then(|rest| rest.windows(MAGIC.len()).position(|window| window == MAGIC))
    {
        let candidate = start + found;
        if let RecordAt::Valid { record, .. } = decode_record_at(bytes, candidate) {
            if last_lsn.is_none_or(|lsn| record.lsn > lsn) {
                return Some(candidate);
            }
        }
        start = candidate + 1;
    }
    None
}

fn decode_record_at(bytes: &[u8], offset: usize) -> RecordAt {
    let rest = &bytes[offset..];
    if rest.len() < HEADER_LEN || &rest[0..4] != MAGIC {
        return RecordAt::Invalid;
    }
    let header = &rest[..HEADER_LEN];
    let key_len = read_u32(header, 44) as usize;
    let payload_len = usize::try_from(read_u64(header, 48)).unwrap_or(usize::MAX);
    let data_name_len = read_u32(header, 56) as usize;
    if key_len > MAX_KEY_LEN || data_name_len > MAX_DATA_NAME_LEN || payload_len > MAX_PAYLOAD_LEN {
        return RecordAt::Invalid;
    }
    let key_start = HEADER_LEN + data_name_len;
    let payload_start = key_start + key_len;
    let total = payload_start + payload_len;
    if rest.len() < total {
        return RecordAt::Invalid;
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != VERSION {
        // A header torn just after its magic reads as version 0 or noise;
        // only a plausible later version is a record from a newer build.
        return if version > VERSION && version <= VERSION + 64 {
            RecordAt::Unsupported
        } else {
            RecordAt::Invalid
        };
    }
    if record_checksum(header, &rest[HEADER_LEN..total]) != header[60..92] {
        return RecordAt::Invalid;
    }
    // Intact from here on: anything this build cannot interpret was written
    // by a newer one and must be refused, never cut as a torn tail.
    let Ok(op) = WalOp::from_u8(header[6]) else {
        return RecordAt::Unsupported;
    };
    let (Ok(data_name), Ok(key)) = (
        String::from_utf8(rest[HEADER_LEN..key_start].to_vec()),
        String::from_utf8(rest[key_start..payload_start].to_vec()),
    ) else {
        return RecordAt::Unsupported;
    };
    RecordAt::Valid {
        record: WalRecord {
            lsn: read_u64(header, 8),
            generation: read_u64(header, 16),
            op,
            offset: read_u64(header, 24),
            resulting_size: read_u64(header, 32),
            mtime_secs: read_u32(header, 40),
            dirty_at_ms: read_i64(header, 92),
            data_name,
            key,
            payload: rest[payload_start..total].to_vec(),
        },
        len: total,
        watermark: read_u64(header, 100),
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_uses_raw_payload_bytes_not_json_arrays() {
        let record = WalRecord {
            lsn: 1,
            generation: 1,
            op: WalOp::Write,
            offset: 4,
            resulting_size: 7,
            mtime_secs: 9,
            dirty_at_ms: 11,
            data_name: "record.data".into(),
            key: "k".into(),
            payload: vec![0, 1, b'[', b']', 255],
        };
        let encoded = encode_record(&record).unwrap();
        assert_eq!(
            &encoded[HEADER_LEN + record.data_name.len() + record.key.len()..],
            record.payload.as_slice()
        );
        assert!(!encoded.windows(5).any(|window| window == b"[0,1,"));
        let decoded = decode_records(&encoded);
        assert!(!decoded.torn() && decoded.damage.is_empty());
        assert_eq!(decoded.records[0].payload, record.payload);
    }

    #[tokio::test]
    async fn recovery_ignores_a_torn_tail_after_acknowledged_records() {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-torn-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let data = root.join("record.data");
        File::create(&data).await.unwrap();
        let wal = wal_path(&data);
        append_record(
            &wal,
            &WalRecord {
                lsn: 1,
                generation: 1,
                op: WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: "record.data".into(),
                key: "key".into(),
                payload: b"abc".to_vec(),
            },
        )
        .await
        .unwrap();
        let mut file = OpenOptions::new().append(true).open(&wal).await.unwrap();
        file.write_all(
            &encode_record(&WalRecord {
                lsn: 2,
                generation: 2,
                op: WalOp::Write,
                offset: 3,
                resulting_size: 6,
                mtime_secs: 2,
                dirty_at_ms: 2,
                data_name: "record.data".into(),
                key: "key".into(),
                payload: b"def".to_vec(),
            })
            .unwrap()[..20],
        )
        .await
        .unwrap();
        drop(file);
        let summary = replay_file(&data, 0).await.unwrap().unwrap();
        assert!(summary.truncated_tail);
        assert_eq!(summary.record.lsn, 1);
        assert_eq!(tokio::fs::read(&data).await.unwrap(), b"abc");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn compaction_keeps_the_records_other_stages_still_need() {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-retain-other-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let first = root.join("first.data");
        let second = root.join("second.data");
        File::create(&first).await.unwrap();
        File::create(&second).await.unwrap();
        let wal = wal_path(&first);
        append_record(
            &wal,
            &WalRecord {
                lsn: 1,
                generation: 1,
                op: WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: "first.data".into(),
                key: "first".into(),
                payload: b"one".to_vec(),
            },
        )
        .await
        .unwrap();
        append_record(
            &wal,
            &WalRecord {
                lsn: 1,
                generation: 1,
                op: WalOp::Write,
                offset: 0,
                resulting_size: 3,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: "second.data".into(),
                key: "second".into(),
                payload: b"two".to_vec(),
            },
        )
        .await
        .unwrap();
        // first's manifest checkpoints its record; second has only the WAL.
        manifest_at(&first, "first", 1).await;
        assert!(compact_now(&wal).await.unwrap());
        let decoded = decode_records(&tokio::fs::read(&wal).await.unwrap());
        assert_eq!(decoded.records.len(), 1);
        assert_eq!(decoded.records[0].data_name, "second.data");
        let summary = replay_file(&second, 0).await.unwrap().unwrap();
        assert_eq!(summary.record.key, "second");
        assert_eq!(tokio::fs::read(&second).await.unwrap(), b"two");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn records_appended_while_a_compaction_runs_are_never_lost() {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-compact-concurrent-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let dead = root.join("dead.data");
        let live = root.join("live.data");
        File::create(&dead).await.unwrap();
        File::create(&live).await.unwrap();
        let wal = wal_path(&live);
        let record = |name: &str, payload: u8| WalRecord {
            lsn: 0,
            generation: 1,
            op: WalOp::Write,
            offset: 0,
            resulting_size: 4096,
            mtime_secs: 1,
            dirty_at_ms: 1,
            data_name: name.into(),
            key: name.into(),
            payload: vec![payload; 4096],
        };
        for index in 0..200u8 {
            append_record(&wal, &record("dead.data", index))
                .await
                .unwrap();
        }
        let mut expected = Vec::new();
        for index in 0..10u8 {
            expected.push(
                append_record(&wal, &record("live.data", index))
                    .await
                    .unwrap(),
            );
        }
        manifest_at(&dead, "dead.data", u64::MAX).await;

        let compaction = tokio::spawn({
            let wal = wal.clone();
            async move { compact_now(&wal).await.unwrap() }
        });
        for index in 10..60u8 {
            expected.push(
                append_record(&wal, &record("live.data", index))
                    .await
                    .unwrap(),
            );
            tokio::task::yield_now().await;
        }
        assert!(compaction.await.unwrap(), "the compaction was not needed");

        let decoded = decode_records(&tokio::fs::read(&wal).await.unwrap());
        assert!(!decoded.torn() && decoded.damage.is_empty());
        assert!(decoded
            .records
            .iter()
            .all(|record| record.data_name == "live.data"));
        let lsns: Vec<u64> = decoded.records.iter().map(|record| record.lsn).collect();
        assert_eq!(lsns, expected, "a record appended meanwhile was lost");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    async fn manifest_at(data: &Path, key: &str, checkpoint_lsn: u64) {
        write_json_atomic(
            &data.with_extension("stage.json"),
            &StageRecovery {
                key: key.into(),
                checkpoint_lsn,
                generation: checkpoint_lsn,
                ..recovery_placeholder(data)
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn compaction_highwater_prevents_lsn_reuse_after_restart() {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-highwater-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let data = root.join("record.data");
        File::create(&data).await.unwrap();
        let wal = wal_path(&data);
        let record = WalRecord {
            lsn: 64,
            generation: 64,
            op: WalOp::Write,
            offset: 0,
            resulting_size: 3,
            mtime_secs: 1,
            dirty_at_ms: 1,
            data_name: "record.data".into(),
            key: "key".into(),
            payload: b"abc".to_vec(),
        };
        assert_eq!(append_record(&wal, &record).await.unwrap(), 64);
        manifest_at(&data, "key", 64).await;
        assert!(compact_now(&wal).await.unwrap());
        assert!(!wal.exists(), "nothing was left to keep");
        forget_append_state(&wal).await;
        let next = WalRecord {
            lsn: 1,
            generation: 65,
            op: WalOp::Write,
            offset: 0,
            resulting_size: 3,
            mtime_secs: 2,
            dirty_at_ms: 2,
            data_name: "record.data".into(),
            key: "key".into(),
            payload: b"def".to_vec(),
        };
        assert_eq!(append_record(&wal, &next).await.unwrap(), 65);
        let summary = replay_file(&data, 64).await.unwrap().unwrap();
        assert_eq!(summary.record.lsn, 65);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn overlapping_writes_and_truncate_replay_in_lsn_order() {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-overlap-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let data = root.join("record.data");
        File::create(&data).await.unwrap();
        let wal = wal_path(&data);
        for record in [
            WalRecord {
                lsn: 1,
                generation: 1,
                op: WalOp::Write,
                offset: 0,
                resulting_size: 6,
                mtime_secs: 1,
                dirty_at_ms: 1,
                data_name: "record.data".into(),
                key: "key".into(),
                payload: b"abcdef".to_vec(),
            },
            WalRecord {
                lsn: 2,
                generation: 2,
                op: WalOp::Write,
                offset: 2,
                resulting_size: 6,
                mtime_secs: 2,
                dirty_at_ms: 1,
                data_name: "record.data".into(),
                key: "key".into(),
                payload: b"XY".to_vec(),
            },
            WalRecord {
                lsn: 3,
                generation: 3,
                op: WalOp::Truncate,
                offset: 0,
                resulting_size: 4,
                mtime_secs: 3,
                dirty_at_ms: 1,
                data_name: "record.data".into(),
                key: "key".into(),
                payload: Vec::new(),
            },
        ] {
            append_record(&wal, &record).await.unwrap();
        }
        let summary = replay_file(&data, 0).await.unwrap().unwrap();
        assert_eq!(summary.record.lsn, 3);
        assert_eq!(tokio::fs::read(&data).await.unwrap(), b"abXY");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    fn write_record(lsn: u64, offset: u64, payload: &[u8]) -> WalRecord {
        WalRecord {
            lsn,
            generation: lsn,
            op: WalOp::Write,
            offset,
            resulting_size: offset + payload.len() as u64,
            mtime_secs: 1,
            dirty_at_ms: 1,
            data_name: "record.data".into(),
            key: "key".into(),
            payload: payload.to_vec(),
        }
    }

    async fn two_acknowledged_records(label: &str) -> (PathBuf, PathBuf, PathBuf, u64) {
        let root = std::env::temp_dir().join(format!(
            "r2-wal-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let data = root.join("record.data");
        File::create(&data).await.unwrap();
        let wal = wal_path(&data);
        append_record(&wal, &write_record(1, 0, b"abc"))
            .await
            .unwrap();
        append_record(&wal, &write_record(2, 3, b"def"))
            .await
            .unwrap();
        let acknowledged_len = tokio::fs::metadata(&wal).await.unwrap().len();
        (root, data, wal, acknowledged_len)
    }

    async fn append_raw(wal: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new().append(true).open(wal).await.unwrap();
        file.write_all(bytes).await.unwrap();
        file.sync_all().await.unwrap();
    }

    #[tokio::test]
    async fn every_kind_of_invalid_tail_is_cut_back_to_the_last_valid_record() {
        let third = write_record(3, 6, b"ghi");
        let mut bad_checksum = encode_record(&third).unwrap();
        *bad_checksum.last_mut().unwrap() ^= 0xff;
        let mut bad_op = encode_record(&third).unwrap();
        bad_op[6] = 9;
        let garbage: Vec<u8> = (0..777u32).map(|i| (i * 31 + 7) as u8).collect();
        // A header torn right after its magic bytes is not a newer format.
        let mut torn_after_magic = MAGIC.to_vec();
        torn_after_magic.resize(HEADER_LEN + 64, 0);
        let tails: [(&str, Vec<u8>); 6] = [
            ("zero-fill", vec![0u8; 4096]),
            ("bad-checksum", bad_checksum),
            ("bad-op", bad_op),
            ("garbage", garbage),
            ("short", encode_record(&third).unwrap()[..50].to_vec()),
            ("torn-after-magic", torn_after_magic),
        ];
        for (label, tail) in tails {
            let (root, data, wal, acknowledged_len) = two_acknowledged_records(label).await;
            append_raw(&wal, &tail).await;
            // Power loss: the process that wrote the WAL is gone.
            forget_append_state(&wal).await;

            let index = recovery_index(&root).await.unwrap_or_else(|error| {
                panic!("{label}: an invalid tail must not fail recovery: {error}")
            });
            let summary = index.summary_for_after(&data, 0, 0).unwrap().unwrap();
            assert_eq!(summary.record.lsn, 2, "{label}");
            assert!(replay_all(&root).await.unwrap().is_empty(), "{label}");
            assert_eq!(tokio::fs::read(&data).await.unwrap(), b"abcdef", "{label}");

            repair_tail(&wal).await.unwrap();
            assert_eq!(
                tokio::fs::metadata(&wal).await.unwrap().len(),
                acknowledged_len,
                "{label}: the invalid tail was never acknowledged and must go"
            );
            assert_eq!(
                append_record(&wal, &write_record(1, 6, b"ghi"))
                    .await
                    .unwrap(),
                3,
                "{label}"
            );
            let summary = replay_file(&data, 2).await.unwrap().unwrap();
            assert_eq!(summary.record.lsn, 3, "{label}");
            assert_eq!(tokio::fs::read(&data).await.unwrap(), b"abcdefghi");
            tokio::fs::remove_dir_all(root).await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_hole_before_unacknowledged_records_is_cut_like_a_torn_tail() {
        // No commit ever acknowledged these appends: the batch was still in
        // flight, and unsynced bytes can reach the disk out of order.
        let (root, data, wal, _) = two_acknowledged_records("power-loss-hole").await;
        append_record(&wal, &write_record(3, 6, b"ghi"))
            .await
            .unwrap();
        let mut bytes = tokio::fs::read(&wal).await.unwrap();
        let first_len = encode_record(&write_record(1, 0, b"abc")).unwrap().len();
        let second_len = encode_record(&write_record(2, 3, b"def")).unwrap().len();
        // The middle record never landed; the one after it did.
        bytes[first_len..first_len + second_len].fill(0);
        tokio::fs::write(&wal, &bytes).await.unwrap();
        forget_append_state(&wal).await;

        let errors = replay_all(&root).await.unwrap();
        assert!(
            errors.is_empty(),
            "a power-loss hole is not damage: {errors:?}"
        );
        assert_eq!(tokio::fs::read(&data).await.unwrap(), b"abc");
        repair_tail(&wal).await.unwrap();
        assert_eq!(
            tokio::fs::metadata(&wal).await.unwrap().len(),
            first_len as u64,
            "the hole and everything behind it were never acknowledged"
        );
        // No LSN is handed out twice, not even one that was cut.
        assert_eq!(
            append_record(&wal, &write_record(1, 3, b"DEF"))
                .await
                .unwrap(),
            4
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_valid_record_from_a_newer_build_is_refused_never_cut() {
        let (root, _data, wal, acknowledged_len) = two_acknowledged_records("newer-op").await;
        append_raw(&wal, &encode_with_op(&write_record(3, 6, b"ghi"), 9)).await;
        let bytes = tokio::fs::read(&wal).await.unwrap();
        assert!(acknowledged_len < bytes.len() as u64);
        forget_append_state(&wal).await;

        repair_tail(&wal).await.unwrap();
        assert_eq!(
            tokio::fs::read(&wal).await.unwrap(),
            bytes,
            "an intact record this build cannot read is never cut"
        );
        assert!(recovery_index(&root).await.unwrap().damage().is_some());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
