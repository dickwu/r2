//! What the native smoke test checks through the mounted drive besides its
//! own write, read, rename and delete: names beyond ASCII, case variants, an
//! existing file and a folder, each against what storage then holds.
use super::*;

/// Writes `name` through the mount and checks the property the server must
/// keep: no name is stored under a key the user did not write. Allowed: the
/// name is refused and storage gains no key; it is stored under exactly its
/// own key (a UTF-8 code page); or, only where `look_alike` names one, under
/// that best-fit ASCII look-alike (`č` sent as `c`), which the server cannot
/// tell from a real name. Whatever was stored must go again with a delete
/// through the mount, and no key may carry a `?` standing for a lost
/// character. The other known exception, code-page bytes that happen to spell
/// valid UTF-8 (GBK `说`, `CB B5`, is stored as `˵`), the probe names avoid.
/// The outcome is printed either way, as evidence in the CI log.
#[cfg_attr(not(windows), allow(dead_code))]
async fn never_stored_under_an_unexpected_key(
    target: &Path,
    objects: &std::sync::Mutex<HashMap<String, Object>>,
    name: &str,
    look_alike: Option<&str>,
) -> std::io::Result<()> {
    let keys = || -> std::collections::BTreeSet<String> {
        objects.lock().unwrap().keys().cloned().collect()
    };
    let before = keys();
    let write = tokio::fs::write(target.join(name), b"x").await;
    let added: Vec<String> = keys().difference(&before).cloned().collect();
    let outcome = match (&write, added.as_slice()) {
        (Err(error), []) => Ok(format!("refused ({error})")),
        (Ok(()), [key]) if key == name => Ok("stored under its own key".to_string()),
        (Ok(()), [key]) if look_alike == Some(key.as_str()) => {
            Ok(format!("stored under the look-alike {key:?}"))
        }
        (Ok(()), _) => Err(format!("stored as {added:?}")),
        (Err(error), _) => Err(format!("refused ({error}) yet stored as {added:?}")),
    };
    eprintln!(
        "Beyond-ASCII name {name:?}: {}",
        outcome.as_ref().unwrap_or_else(|problem| problem)
    );
    outcome.map_err(|problem| std::io::Error::other(format!("{name:?} was {problem}")))?;
    if let [key] = added.as_slice() {
        tokio::fs::remove_file(target.join(name))
            .await
            .map_err(doing(format!("deleting {name:?}")))?;
        if objects.lock().unwrap().contains_key(key) {
            return Err(std::io::Error::other(format!(
                "Deleting {name:?} through the mount left {key:?} behind"
            )));
        }
    }
    if let Some(key) = keys().into_iter().find(|key| key.contains('?')) {
        return Err(std::io::Error::other(format!(
            "A lost character reached storage as {key:?}"
        )));
    }
    Ok(())
}

/// Every beyond-ASCII probe, each run even after an earlier one failed so the
/// log records all their outcomes; the error names each one that failed.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) async fn beyond_ascii_names_keep_their_keys(
    target: &Path,
    objects: &std::sync::Mutex<HashMap<String, Object>>,
) -> std::io::Result<()> {
    let mut outcomes = Vec::new();
    for (name, look_alike) in [
        ("caf\u{e9}.txt", None),
        ("\u{4e2d}\u{6587}.txt", None),
        ("\u{010d}as.txt", Some("cas.txt")),
        ("\u{0142}.txt", Some("l.txt")),
    ] {
        outcomes
            .push(never_stored_under_an_unexpected_key(target, objects, name, look_alike).await);
    }
    all_passed(outcomes)
}

/// `Ok` when every outcome is; otherwise one error naming each failure.
pub(super) fn all_passed<E: std::fmt::Display>(
    outcomes: impl IntoIterator<Item = Result<(), E>>,
) -> std::io::Result<()> {
    let failed: Vec<String> = outcomes
        .into_iter()
        .filter_map(|outcome| outcome.err().map(|error| error.to_string()))
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(failed.join("; ")))
    }
}

/// How long the write/read/rename/delete cycle and each probe may take: far
/// longer than any took on the CI runners (a few hundred milliseconds), and
/// short enough that all of them, at most five limits or 25 s, fit the
/// operations' own 30 s bound.
const PROBE_LIMIT: Duration = Duration::from_secs(5);

/// Runs `probe` for at most `PROBE_LIMIT` and names it in any failure. A soft
/// mount can hold one call far longer than the whole test may wait, and an
/// unnamed timeout would not say which probe hung.
pub(super) async fn named(
    name: &str,
    probe: impl std::future::Future<Output = std::io::Result<()>>,
) -> Result<(), String> {
    match tokio::time::timeout(PROBE_LIMIT, probe).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("{name}: {error}")),
        Err(_) => Err(format!(
            "{name}: no answer within {} s",
            PROBE_LIMIT.as_secs()
        )),
    }
}

/// Names the step a raw I/O error from the mount came from, keeping its kind.
pub(super) fn doing(step: impl std::fmt::Display) -> impl FnOnce(std::io::Error) -> std::io::Error {
    move |error| std::io::Error::new(error.kind(), format!("{step}: {error}"))
}

/// Seeded before mounting, so the case probe's `case.txt` is written next to
/// a name that differs from it only in case.
pub(super) const CASE_SEED_KEY: &str = "Case.txt";
pub(super) const CASE_SEED_DATA: &[u8] = b"original case";

/// Seeded before mounting, so the overwrite probe rewrites a file the client
/// did not create. Windows' Client for NFS creates a file whatever the mode
/// bits, but opens an existing one for writing only if they let its anonymous
/// identity write (`permission_modes`).
pub(super) const EXISTING_SEED_KEY: &str = "existing.txt";
pub(super) const EXISTING_SEED_DATA: &[u8] = b"seeded bytes";

/// Publishes the staged bytes as an unmount's pre-drain would: they reach
/// storage only when published, and this test runs no flusher.
async fn publish_staged(fs: &S3NfsFs, written: &str) -> std::io::Result<()> {
    let unpublished = fs.drain(1, stage::UPLOAD_ATTEMPTS).await;
    if unpublished == 0 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "Publishing {written} left {unpublished} staged file(s) unpublished"
        )))
    }
}

/// Fails naming `expectation` unless storage holds exactly `wanted` under
/// `key` (`None`: no such key).
fn expect_stored(
    objects: &std::sync::Mutex<HashMap<String, Object>>,
    key: &str,
    wanted: Option<&[u8]>,
    expectation: &str,
) -> std::io::Result<()> {
    let held = objects
        .lock()
        .unwrap()
        .get(key)
        .map(|object| object.data.clone());
    if held.as_deref() == wanted {
        return Ok(());
    }
    let found = match held {
        Some(data) => format!("holds {key} as {:?}", String::from_utf8_lossy(&data)),
        None => format!("has no {key}"),
    };
    Err(std::io::Error::other(format!(
        "{expectation}, but storage {found}"
    )))
}

/// Writes `case.txt` through the mount next to the seeded `Case.txt` and
/// checks that the two stay two objects. A client that folds case would open
/// `Case.txt` for the write instead and truncate it.
pub(super) async fn case_variants_stay_apart(
    fs: &S3NfsFs,
    target: &Path,
    objects: &std::sync::Mutex<HashMap<String, Object>>,
) -> std::io::Result<()> {
    let lower: &[u8] = b"lower case";
    tokio::fs::write(target.join("case.txt"), lower)
        .await
        .map_err(doing("writing case.txt"))?;
    publish_staged(fs, "case.txt").await?;
    expect_stored(
        objects,
        "case.txt",
        Some(lower),
        "case.txt should be stored under its own key with the bytes written",
    )?;
    expect_stored(
        objects,
        CASE_SEED_KEY,
        Some(CASE_SEED_DATA),
        "Writing case.txt should leave Case.txt as seeded",
    )?;
    let seeded = tokio::fs::read(target.join(CASE_SEED_KEY))
        .await
        .map_err(doing("reading Case.txt"))?;
    if seeded != CASE_SEED_DATA {
        return Err(std::io::Error::other(
            "Case.txt should still read back as seeded after case.txt was written",
        ));
    }
    tokio::fs::remove_file(target.join("case.txt"))
        .await
        .map_err(doing("deleting case.txt"))?;
    expect_stored(
        objects,
        "case.txt",
        None,
        "Deleting case.txt should remove it from storage",
    )?;
    expect_stored(
        objects,
        CASE_SEED_KEY,
        Some(CASE_SEED_DATA),
        "Deleting case.txt should leave Case.txt as seeded",
    )
}

/// Overwrites the seeded `existing.txt` through the mount with fewer bytes
/// than it held, so a write that kept the old length would leave a stale tail.
pub(super) async fn an_existing_file_takes_new_bytes(
    fs: &S3NfsFs,
    target: &Path,
    objects: &std::sync::Mutex<HashMap<String, Object>>,
) -> std::io::Result<()> {
    let replaced: &[u8] = b"new bytes";
    tokio::fs::write(target.join(EXISTING_SEED_KEY), replaced)
        .await
        .map_err(doing("overwriting existing.txt"))?;
    publish_staged(fs, EXISTING_SEED_KEY).await?;
    expect_stored(
        objects,
        EXISTING_SEED_KEY,
        Some(replaced),
        "Overwriting existing.txt should store exactly the new bytes",
    )?;
    let read_back = tokio::fs::read(target.join(EXISTING_SEED_KEY))
        .await
        .map_err(doing("reading existing.txt"))?;
    if read_back != replaced {
        return Err(std::io::Error::other(
            "existing.txt should read back as exactly the new bytes",
        ));
    }
    Ok(())
}

/// Makes a folder through the mount, writes a file into it, moves the file
/// out to the root and removes the folder, checking storage after each step.
/// A folder is stored as its zero-byte `name/` marker, without which an empty
/// one would not exist at all. The moved file is read back and the removed
/// folder looked up through the mount too, so a move that lost track of the
/// file or a listing that still served the folder would fail here.
pub(super) async fn a_folder_takes_a_file_and_goes_again(
    fs: &S3NfsFs,
    target: &Path,
    objects: &std::sync::Mutex<HashMap<String, Object>>,
) -> std::io::Result<()> {
    let filed: &[u8] = b"filed bytes";
    let folder = target.join("folder");
    tokio::fs::create_dir(&folder)
        .await
        .map_err(doing("making folder"))?;
    expect_stored(
        objects,
        "folder/",
        Some(b"".as_slice()),
        "Making folder should store its folder/ marker",
    )?;
    tokio::fs::write(folder.join("inside.txt"), filed)
        .await
        .map_err(doing("writing folder/inside.txt"))?;
    publish_staged(fs, "folder/inside.txt").await?;
    expect_stored(
        objects,
        "folder/inside.txt",
        Some(filed),
        "folder/inside.txt should be stored with the bytes written",
    )?;
    tokio::fs::rename(folder.join("inside.txt"), target.join("moved.txt"))
        .await
        .map_err(doing("moving folder/inside.txt to moved.txt"))?;
    expect_stored(
        objects,
        "moved.txt",
        Some(filed),
        "Moving inside.txt out of folder should store moved.txt with its bytes",
    )?;
    expect_stored(
        objects,
        "folder/inside.txt",
        None,
        "Moving inside.txt out of folder should remove folder/inside.txt",
    )?;
    let moved = tokio::fs::read(target.join("moved.txt"))
        .await
        .map_err(doing("reading moved.txt"))?;
    if moved != filed {
        return Err(std::io::Error::other(
            "moved.txt should read back as the bytes written into folder",
        ));
    }
    tokio::fs::remove_dir(&folder)
        .await
        .map_err(doing("removing folder"))?;
    expect_stored(
        objects,
        "folder/",
        None,
        "Removing folder should delete its marker",
    )?;
    match tokio::fs::metadata(&folder).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(std::io::Error::other(
                "folder should no longer be found through the mount once removed",
            ))
        }
        Err(error) => return Err(doing("looking up the removed folder")(error)),
    }
    tokio::fs::remove_file(target.join("moved.txt"))
        .await
        .map_err(doing("deleting moved.txt"))?;
    expect_stored(
        objects,
        "moved.txt",
        None,
        "Deleting moved.txt should remove it from storage",
    )
}

#[tokio::test(start_paused = true)]
async fn a_probe_that_never_answers_is_named_when_its_time_runs_out() {
    let outcome = named("folder probe", std::future::pending()).await;
    assert_eq!(
        outcome,
        Err("folder probe: no answer within 5 s".to_string())
    );
}

#[tokio::test]
async fn a_failing_probe_names_itself_and_the_step_and_keeps_the_kind() {
    let error = doing("making folder")(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let message = named("folder probe", async { Err(error) })
        .await
        .unwrap_err();
    assert!(
        message.starts_with("folder probe: making folder: "),
        "{message}"
    );
}

#[test]
fn every_failure_is_reported_not_just_the_first() {
    let outcomes: [Result<(), &str>; 3] = [Err("first"), Ok(()), Err("third")];
    assert_eq!(
        all_passed(outcomes).unwrap_err().to_string(),
        "first; third"
    );
    assert!(all_passed::<&str>([]).is_ok());
}
