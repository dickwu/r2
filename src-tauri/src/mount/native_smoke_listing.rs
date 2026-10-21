//! `mount.exe`'s listing of the mounted drive, which shows the options that
//! took effect: the tool mounts even when it ignored some.
use super::*;

/// The properties `mount.exe` lists for `drive` (`Z:`): its row and the
/// indented lines under it, up to the next row, as `key=value` pairs; the
/// remote path and anything else without a `=` is skipped. `None` when the
/// drive is not listed.
fn listed_mount_properties(listing: &str, drive: &str) -> Option<BTreeMap<String, String>> {
    let continues_row = |line: &&str| line.starts_with(char::is_whitespace);
    let mut lines = listing.lines().skip_while(|line| {
        continues_row(line)
            || !line
                .split_whitespace()
                .next()
                .is_some_and(|local| local.eq_ignore_ascii_case(drive))
    });
    let row = lines.next()?;
    let properties = std::iter::once(row)
        .chain(lines.take_while(continues_row))
        .flat_map(|line| line.split(|c: char| c == ',' || c.is_whitespace()))
        .filter_map(|token| token.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    Some(properties)
}

/// What the mount asks for (`platform::WINDOWS_MOUNT_OPTIONS`), as
/// `mount.exe` lists it. On the windows-2022 runner only `casesensitive`,
/// `timeout` and `retry` differ from the client's defaults, so only they can
/// fail there; the other four restate those defaults, though Microsoft
/// documents locking as on by default elsewhere.
const REQUESTED_MOUNT_PROPERTIES: [(&str, &str); 7] = [
    ("UID", "-2"),     // anon
    ("locking", "no"), // nolock
    ("casesensitive", "yes"),
    ("timeout", "10"),
    ("retry", "10"),
    ("mount", "soft"), // mtype=soft
    ("lang", "ansi"),
];

/// How the listed properties differ from what the mount asks for, one entry
/// per property; empty when every option took effect. A drive that is not
/// listed is a failure too, never a clean result.
fn mount_option_mismatches(properties: Option<&BTreeMap<String, String>>) -> Vec<String> {
    let Some(properties) = properties else {
        return vec!["the drive is not listed".to_string()];
    };
    REQUESTED_MOUNT_PROPERTIES
        .iter()
        .filter_map(|&(key, wanted)| match properties.get(key) {
            Some(listed) if listed_value_matches(key, listed, wanted) => None,
            Some(listed) => Some(format!("{key}={listed} (wanted {wanted})")),
            None => Some(format!("no {key} (wanted {wanted})")),
        })
        .collect()
}

/// `timeout` is in seconds and may be listed as `10` or `10.0`; every other
/// value compares as text, ignoring ASCII case.
fn listed_value_matches(key: &str, listed: &str, wanted: &str) -> bool {
    if key == "timeout" {
        matches!(
            (listed.parse::<f64>(), wanted.parse::<f64>()),
            (Ok(listed), Ok(wanted)) if listed == wanted
        )
    } else {
        listed.eq_ignore_ascii_case(wanted)
    }
}

/// Checks `mount.exe`'s listing for `drive` against what the mount asks for:
/// the tool mounts even when it ignored some of the options. The drive's
/// properties are printed either way, as evidence in the CI log.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) async fn mount_options_took_effect(drive: &str) -> Result<(), String> {
    let program = windows_nfs_tool("mount.exe")?;
    let run = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(&program)
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let listing = match &run {
        Ok(Ok(output)) if output.status.success() => console_text(&output.stdout),
        _ => {
            return Err(format!(
                "mount.exe could not list the mounts: {}",
                render_tool_run(&run)
            ))
        }
    };
    let properties = listed_mount_properties(&listing, drive);
    match &properties {
        Some(properties) => eprintln!("mount.exe lists {drive} with {properties:?}"),
        None => eprintln!("mount.exe does not list {drive}: {listing:?}"),
    }
    let mismatches = mount_option_mismatches(properties.as_ref());
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "mount.exe's listing for {drive} differs from what the mount asked for: {}",
            mismatches.join(", ")
        ))
    }
}

/// `mount.exe`'s listing on the windows-2022 runner after a mount with
/// nfsserve's option recipe (`...,fileaccess=6,casesensitive,...`), verbatim.
const RECIPE_MOUNT_LISTING: &str = r"Local    Remote                                 Properties
-------------------------------------------------------------------------------
Z:       \\127.88.0.1\!                         UID=-2, GID=-2
                                                rsize=1048576, wsize=1048576
                                                mount=soft, timeout=0.8
                                                retry=1, locking=no
                                                fileaccess=755, lang=ANSI
                                                casesensitive=no
                                                sec=sys
";

fn property_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn the_mount_listing_yields_each_drives_own_properties() {
    let recipe = property_map(&[
        ("UID", "-2"),
        ("GID", "-2"),
        ("rsize", "1048576"),
        ("wsize", "1048576"),
        ("mount", "soft"),
        ("timeout", "0.8"),
        ("retry", "1"),
        ("locking", "no"),
        ("fileaccess", "755"),
        ("lang", "ANSI"),
        ("casesensitive", "no"),
        ("sec", "sys"),
    ]);
    assert_eq!(
        listed_mount_properties(RECIPE_MOUNT_LISTING, "Z:"),
        Some(recipe.clone())
    );
    assert_eq!(listed_mount_properties(RECIPE_MOUNT_LISTING, "Y:"), None);

    // Another drive's rows right above and right below must stay out of Z:'s
    // block, and Z:'s out of theirs.
    let other = r"Y:       \\10.0.0.5\share                       UID=1000, GID=1000
                                                casesensitive=yes, timeout=60
                                                retry=2
";
    let (header, rows) = RECIPE_MOUNT_LISTING.split_at(RECIPE_MOUNT_LISTING.find("Z:").unwrap());
    let listing = format!("{header}{other}{rows}{}", other.replace("Y:", "X:"));
    let neighbour = property_map(&[
        ("UID", "1000"),
        ("GID", "1000"),
        ("casesensitive", "yes"),
        ("timeout", "60"),
        ("retry", "2"),
    ]);
    assert_eq!(listed_mount_properties(&listing, "Z:"), Some(recipe));
    assert_eq!(
        listed_mount_properties(&listing, "Y:"),
        Some(neighbour.clone())
    );
    // Drive letters match whatever their case.
    assert_eq!(listed_mount_properties(&listing, "x:"), Some(neighbour));
    assert_eq!(listed_mount_properties(&listing, "W:"), None);
}

#[test]
fn the_listing_check_wants_what_the_mount_asks_for() {
    // How `mount.exe` lists each option it applied.
    fn listed_as(option: &'static str) -> (&'static str, &'static str) {
        match option.split_once('=') {
            None if option == "anon" => ("UID", "-2"),
            None if option == "nolock" => ("locking", "no"),
            Some(("mtype", value)) => ("mount", value),
            Some(pair) => pair,
            None => panic!("no listing known for the option {option:?}"),
        }
    }
    let asked: std::collections::BTreeSet<_> = crate::mount::platform::WINDOWS_MOUNT_OPTIONS
        .into_iter()
        .map(listed_as)
        .collect();
    let checked: std::collections::BTreeSet<_> = REQUESTED_MOUNT_PROPERTIES.into_iter().collect();
    assert_eq!(checked, asked);
}

#[test]
fn the_mount_listing_shows_which_requested_options_took_effect() {
    // nfsserve's recipe, one comma list, showed nothing but this runner's
    // defaults.
    let recipe = listed_mount_properties(RECIPE_MOUNT_LISTING, "Z:");
    assert_eq!(
        mount_option_mismatches(recipe.as_ref()),
        [
            "casesensitive=no (wanted yes)",
            "timeout=0.8 (wanted 10)",
            "retry=1 (wanted 10)",
        ]
    );
    // What the mount asks for now, whichever way the timeout is printed.
    for timeout in ["10", "10.0"] {
        let listing = RECIPE_MOUNT_LISTING
            .replace("timeout=0.8", &format!("timeout={timeout}"))
            .replace("retry=1", "retry=10")
            .replace("casesensitive=no", "casesensitive=yes");
        let mismatches = mount_option_mismatches(listed_mount_properties(&listing, "Z:").as_ref());
        assert!(mismatches.is_empty(), "timeout={timeout}: {mismatches:?}");
    }
    // A drive the listing lacks fails; it is not a listing with no mismatch.
    let absent = listed_mount_properties(RECIPE_MOUNT_LISTING, "Y:");
    assert_eq!(
        mount_option_mismatches(absent.as_ref()),
        ["the drive is not listed"]
    );
    // So does a property the listing lacks.
    let without_retry = RECIPE_MOUNT_LISTING.replace("retry=1, ", "");
    assert_eq!(
        mount_option_mismatches(listed_mount_properties(&without_retry, "Z:").as_ref()),
        [
            "casesensitive=no (wanted yes)",
            "timeout=0.8 (wanted 10)",
            "no retry (wanted 10)",
        ]
    );
}
