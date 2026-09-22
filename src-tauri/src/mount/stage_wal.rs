use std::collections::HashMap;
use std::io::{Error, ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::providers::resources::DiskLease;

use super::{
    stage::PublicationGuard,
    stage::StageRecovery,
    stage::UploadSnapshot,
    stage::{sync_parent, write_json_atomic},
    stage_commit,
};

const MAGIC: &[u8; 4] = b"R2WL";
const VERSION: u16 = 2;
const HEADER_LEN: usize = 104;
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
}

impl WalOp {
    fn as_u8(self) -> u8 {
        match self {
            Self::Write => 1,
            Self::Truncate => 2,
        }
    }

    fn from_u8(value: u8) -> std::io::Result<Self> {
        match value {
            1 => Ok(Self::Write),
            2 => Ok(Self::Truncate),
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

#[derive(Debug, Clone, Default)]
pub struct WalRecoveryIndex {
    buckets: HashMap<String, WalBucket>,
    truncated_tail: bool,
    corruption: Option<String>,
}

impl WalRecoveryIndex {
    /// Set when damage sits in front of intact records: nothing in the
    /// folder can be trusted to be complete, so recovery quarantines it all.
    pub fn corruption(&self) -> Option<&str> {
        self.corruption.as_deref()
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

#[derive(Debug, Clone, Default)]
struct RootWal {
    buckets: HashMap<String, WalBucket>,
    truncated_tail: bool,
    corruption: Option<String>,
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

#[derive(Debug, Clone, Copy)]
struct AppendState {
    next_lsn: u64,
    tail_valid: bool,
}

static APPEND_STATES: OnceLock<Mutex<HashMap<PathBuf, AppendState>>> = OnceLock::new();

fn append_states() -> &'static Mutex<HashMap<PathBuf, AppendState>> {
    APPEND_STATES.get_or_init(|| Mutex::new(HashMap::new()))
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
    let state = states.entry(path.to_path_buf()).or_insert(AppendState {
        next_lsn: 1,
        tail_valid: false,
    });
    if !state.tail_valid {
        let next_lsn = repair_tail_and_next_lsn(path).await?;
        state.next_lsn = state.next_lsn.max(next_lsn);
        state.tail_valid = true;
    }

    let assigned_lsn = state.next_lsn.max(record.lsn);
    state.next_lsn = assigned_lsn.saturating_add(1);
    state.tail_valid = false;

    let mut assigned = record.clone();
    assigned.lsn = assigned_lsn;
    let bytes = encode_record(&assigned)?;
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
    decoded.refuse_damage()?;
    let truncated_tail = decoded.tail == WalTail::Torn;
    let mut last = None;
    for record in decoded.records.into_iter().filter(|record| {
        record.data_name == data_name
            && record.lsn > checkpoint_lsn
            && record.generation > generation_floor
    }) {
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
    let next_lsn = repair_tail_and_next_lsn(path).await?;
    let state = states.entry(path.to_path_buf()).or_insert(AppendState {
        next_lsn,
        tail_valid: true,
    });
    state.next_lsn = state.next_lsn.max(next_lsn);
    state.tail_valid = true;
    Ok(())
}

async fn repair_tail_and_next_lsn(path: &Path) -> std::io::Result<u64> {
    let highwater = read_highwater(path).await?;
    let mut file = match OpenOptions::new().read(true).write(true).open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(highwater.unwrap_or(1)),
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    note_wal_read(path);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let decoded = decode_records(&bytes);
    // Damage in front of intact records is never cut: those records may have
    // been acknowledged. Appending behind it would only bury more of them.
    decoded.refuse_damage()?;
    if decoded.tail == WalTail::Torn {
        file.set_len(decoded.valid_len as u64).await?;
        file.sync_all().await?;
        stage_commit::record_file_sync_bytes(decoded.valid_len as u64);
        sync_parent(path).await?;
    }
    Ok(highwater.unwrap_or(1).max(
        decoded
            .records
            .iter()
            .map(|record| record.lsn)
            .max()
            .unwrap_or(0)
            .saturating_add(1),
    ))
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
    let wal = read_root_wal(root).await.map_err(|e| e.to_string())?;
    Ok(WalRecoveryIndex {
        buckets: wal.buckets,
        truncated_tail: wal.truncated_tail,
        corruption: wal.corruption,
    })
}

pub async fn replay_all(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut errors = Vec::new();
    let wal = read_root_wal(root).await.map_err(|e| e.to_string())?;
    if let Some(damage) = wal.corruption {
        // Nothing is applied from a WAL that may be missing acknowledged
        // records; recovery_entries quarantines the whole folder instead.
        return Ok(vec![(root_wal_path(root), damage)]);
    }
    for (name, bucket) in wal.buckets {
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
        match replay_bucket(
            &data_path,
            bucket,
            checkpoint_lsn,
            generation_floor,
            wal.truncated_tail,
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
                if let Err(error) = write_json_atomic(&manifest_path, &recovery).await {
                    errors.push((error_key, error.to_string()));
                }
            }
            Ok(None) => {}
            Err(error) => errors.push((error_key, error.to_string())),
        }
    }
    Ok(errors)
}

/// Where a stage's replay error is reported. `restore_stages` looks the error
/// of every `replay_pending` record up under `<data>.write.json` — the name a
/// legacy JSON intent for the same data file has — so a WAL replay failure is
/// reported the same way instead of being dropped.
pub fn replay_error_key(data_path: &Path) -> PathBuf {
    data_path.with_extension("write.json")
}

async fn read_root_wal(root: &Path) -> std::io::Result<RootWal> {
    let path = root_wal_path(root);
    let mut file = match File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(RootWal::default()),
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    note_wal_read(&path);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let decoded = decode_records(&bytes);
    let corruption = decoded.damage();
    let truncated_tail = decoded.tail == WalTail::Torn;
    let mut buckets = HashMap::<String, WalBucket>::new();
    for record in decoded.records {
        let len = estimated_record_len(&record)?;
        let bucket = buckets.entry(record.data_name.clone()).or_default();
        bucket.bytes = bucket.bytes.saturating_add(len);
        bucket.records.push(record);
    }
    for bucket in buckets.values_mut() {
        bucket.records.sort_by_key(|record| record.lsn);
    }
    Ok(RootWal {
        buckets,
        truncated_tail,
        corruption,
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
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
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

pub async fn checkpoint(data_path: &Path, checkpoint_lsn: u64) -> std::io::Result<()> {
    let path = wal_path(data_path);
    let mut states = append_states().lock().await;
    let mut file = match File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            states.insert(
                path,
                AppendState {
                    next_lsn: 1,
                    tail_valid: true,
                },
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    note_wal_read(&path);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let data_name = data_name(data_path)?;
    let decoded = decode_records(&bytes);
    decoded.refuse_damage()?;
    let retained: Vec<_> = decoded
        .records
        .into_iter()
        .filter(|record| record.data_name != data_name || record.lsn > checkpoint_lsn)
        .collect();
    let retained_next_lsn = retained
        .iter()
        .map(|record| record.lsn)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let next_lsn = states
        .get(&path)
        .map(|state| state.next_lsn)
        .unwrap_or(1)
        .max(retained_next_lsn)
        .max(checkpoint_lsn.saturating_add(1));
    persist_highwater(&path, next_lsn).await?;
    if retained.is_empty() {
        drop(file);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        sync_parent(&path).await?;
        states.insert(
            path,
            AppendState {
                next_lsn,
                tail_valid: true,
            },
        );
        return Ok(());
    }
    let retained_bytes: u64 = retained
        .iter()
        .map(estimated_record_len)
        .try_fold(0u64, |total, next| {
            next.map(|next| total.saturating_add(next))
        })?;
    let wal_parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "WAL path has no parent"))?;
    let compaction_growth = DiskLease::reserve(wal_parent, retained_bytes, || {
        super::available_space(wal_parent)
    })?;
    let temporary = path.with_extension("wal.tmp");
    let mut output = File::create(&temporary).await?;
    for record in retained {
        output.write_all(&encode_record(&record)?).await?;
    }
    output.sync_all().await?;
    stage_commit::record_file_sync_bytes(retained_bytes);
    drop(output);
    tokio::fs::rename(&temporary, &path).await?;
    drop(compaction_growth);
    sync_parent(&path).await?;
    states.insert(
        path,
        AppendState {
            next_lsn,
            tail_valid: true,
        },
    );
    Ok(())
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
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(data_path)
        .await?;
    apply_record_to_open_file(&mut file, record).await?;
    file.flush().await?;
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
    }
    Ok(())
}

fn encode_record(record: &WalRecord) -> std::io::Result<Vec<u8>> {
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
    if record.op == WalOp::Truncate && !record.payload.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "truncate WAL record cannot have a payload",
        ));
    }
    let data_name = record.data_name.as_bytes();
    let key = record.key.as_bytes();
    let mut checksum_input = Vec::with_capacity(key.len() + record.payload.len() + 49);
    checksum_input.extend_from_slice(&record.lsn.to_le_bytes());
    checksum_input.extend_from_slice(&record.generation.to_le_bytes());
    checksum_input.push(record.op.as_u8());
    checksum_input.extend_from_slice(&record.offset.to_le_bytes());
    checksum_input.extend_from_slice(&record.resulting_size.to_le_bytes());
    checksum_input.extend_from_slice(&record.mtime_secs.to_le_bytes());
    checksum_input.extend_from_slice(&record.dirty_at_ms.to_le_bytes());
    checksum_input.extend_from_slice(&(data_name.len() as u32).to_le_bytes());
    checksum_input.extend_from_slice(&(key.len() as u32).to_le_bytes());
    checksum_input.extend_from_slice(&(record.payload.len() as u64).to_le_bytes());
    checksum_input.extend_from_slice(data_name);
    checksum_input.extend_from_slice(key);
    checksum_input.extend_from_slice(&record.payload);
    let checksum = Sha256::digest(&checksum_input);

    let mut bytes =
        Vec::with_capacity(HEADER_LEN + data_name.len() + key.len() + record.payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.push(record.op.as_u8());
    bytes.push(0);
    bytes.extend_from_slice(&record.lsn.to_le_bytes());
    bytes.extend_from_slice(&record.generation.to_le_bytes());
    bytes.extend_from_slice(&record.offset.to_le_bytes());
    bytes.extend_from_slice(&record.resulting_size.to_le_bytes());
    bytes.extend_from_slice(&record.mtime_secs.to_le_bytes());
    bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(record.payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&(data_name.len() as u32).to_le_bytes());
    bytes.extend_from_slice(checksum.as_slice());
    bytes.extend_from_slice(&record.dirty_at_ms.to_le_bytes());
    bytes.resize(HEADER_LEN, 0);
    bytes.extend_from_slice(data_name);
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(&record.payload);
    Ok(bytes)
}

/// What follows the last record that decoded cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalTail {
    /// Every byte belongs to a valid record.
    Clean,
    /// Invalid bytes run to the end of the file. Group commit fsyncs the WAL
    /// in file order, so no record at or behind the first invalid byte was
    /// ever acknowledged: it is cut back to the last valid record.
    Torn,
    /// Invalid bytes are followed by an intact newer record, or the first bad
    /// record is one this build cannot read. What lies behind the damage may
    /// have been acknowledged, so nothing is cut, replayed or rewritten.
    Damaged,
}

struct DecodedWal {
    records: Vec<WalRecord>,
    tail: WalTail,
    /// Length of the prefix made of valid records.
    valid_len: usize,
}

impl DecodedWal {
    fn damage(&self) -> Option<String> {
        (self.tail == WalTail::Damaged).then(|| {
            format!(
                "Staging WAL is damaged at byte {} with intact records after it; nothing in this folder is replayed or uploaded until it is exported and reviewed",
                self.valid_len
            )
        })
    }

    fn refuse_damage(&self) -> std::io::Result<()> {
        match self.damage() {
            Some(message) => Err(Error::new(ErrorKind::InvalidData, message)),
            None => Ok(()),
        }
    }
}

enum RecordAt {
    Valid(WalRecord, usize),
    /// Right magic, unknown version: a real record this build cannot read.
    Unsupported,
    Invalid,
}

fn decode_records(bytes: &[u8]) -> DecodedWal {
    let mut records: Vec<WalRecord> = Vec::new();
    let mut offset = 0usize;
    let mut tail = WalTail::Clean;
    while offset < bytes.len() {
        match decode_record_at(bytes, offset) {
            RecordAt::Valid(record, len) => {
                records.push(record);
                offset += len;
            }
            RecordAt::Unsupported => {
                tail = WalTail::Damaged;
                break;
            }
            RecordAt::Invalid => {
                let last_lsn = records.last().map(|record| record.lsn);
                tail = if newer_record_after(bytes, offset + 1, last_lsn) {
                    WalTail::Damaged
                } else {
                    WalTail::Torn
                };
                break;
            }
        }
    }
    DecodedWal {
        records,
        tail,
        valid_len: offset,
    }
}

/// Whether an intact record newer than `last_lsn` starts at or after `from`.
/// Appends assign LSNs in file order, so an intact record with an older LSN
/// can only be payload bytes that happen to contain a WAL record.
fn newer_record_after(bytes: &[u8], from: usize, last_lsn: Option<u64>) -> bool {
    let mut start = from;
    while let Some(found) = bytes
        .get(start..)
        .and_then(|rest| rest.windows(MAGIC.len()).position(|window| window == MAGIC))
    {
        let candidate = start + found;
        if let RecordAt::Valid(record, _) = decode_record_at(bytes, candidate) {
            if last_lsn.is_none_or(|lsn| record.lsn > lsn) {
                return true;
            }
        }
        start = candidate + 1;
    }
    false
}

fn decode_record_at(bytes: &[u8], offset: usize) -> RecordAt {
    let rest = &bytes[offset..];
    if rest.len() < HEADER_LEN {
        return RecordAt::Invalid;
    }
    let header = &rest[..HEADER_LEN];
    if &header[0..4] != MAGIC {
        return RecordAt::Invalid;
    }
    if u16::from_le_bytes([header[4], header[5]]) != VERSION {
        return RecordAt::Unsupported;
    }
    let Ok(op) = WalOp::from_u8(header[6]) else {
        return RecordAt::Invalid;
    };
    let key_len = read_u32(header, 44) as usize;
    let payload_len = read_u64(header, 48) as usize;
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
    let (Ok(data_name), Ok(key)) = (
        String::from_utf8(rest[HEADER_LEN..key_start].to_vec()),
        String::from_utf8(rest[key_start..payload_start].to_vec()),
    ) else {
        return RecordAt::Invalid;
    };
    let record = WalRecord {
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
    };
    match encode_record(&record) {
        Ok(encoded) if encoded[60..92] == header[60..92] => RecordAt::Valid(record, total),
        _ => RecordAt::Invalid,
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
        assert_eq!(decoded.tail, WalTail::Clean);
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
    async fn checkpoint_retains_other_stage_records_in_shared_wal() {
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
        checkpoint(&first, 1).await.unwrap();
        let summary = replay_file(&second, 0).await.unwrap().unwrap();
        assert_eq!(summary.record.key, "second");
        assert_eq!(tokio::fs::read(&second).await.unwrap(), b"two");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn checkpoint_highwater_prevents_lsn_reuse_after_restart() {
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
        checkpoint(&data, 64).await.unwrap();
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
        let tails: [(&str, Vec<u8>); 5] = [
            ("zero-fill", vec![0u8; 4096]),
            ("bad-checksum", bad_checksum),
            ("bad-op", bad_op),
            ("garbage", garbage),
            ("short", encode_record(&third).unwrap()[..50].to_vec()),
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
    async fn damage_followed_by_a_valid_record_is_reported_and_never_cut() {
        let (root, data, wal, _) = two_acknowledged_records("mid-file").await;
        append_record(&wal, &write_record(3, 6, b"ghi"))
            .await
            .unwrap();
        let mut bytes = tokio::fs::read(&wal).await.unwrap();
        // Flip a payload byte of the middle record: the third stays intact.
        let first_len = encode_record(&write_record(1, 0, b"abc")).unwrap().len();
        let second_payload = first_len + HEADER_LEN + "record.data".len() + "key".len();
        bytes[second_payload] ^= 0xff;
        tokio::fs::write(&wal, &bytes).await.unwrap();
        forget_append_state(&wal).await;

        let errors = replay_all(&root)
            .await
            .expect("mid-file damage is reported, not a failed recovery");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, wal);
        assert!(errors[0].1.contains("damaged"), "{}", errors[0].1);
        assert!(
            tokio::fs::read(&data).await.unwrap().is_empty(),
            "nothing is replayed from a damaged WAL"
        );
        assert!(repair_tail(&wal).await.is_err());
        assert!(replay_file(&data, 0).await.is_err());
        assert!(checkpoint(&data, 1).await.is_err());
        assert_eq!(
            tokio::fs::read(&wal).await.unwrap(),
            bytes,
            "records after the damage may be acknowledged; keep every byte"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
