//! Per-OS NFS client command construction.
//!
//! The mount itself is performed by the operating system's built-in NFSv3
//! client talking to the localhost server started by [`crate::mount::manager`].
//! Every command is built here as a pure `argv` vector so the exact flags are
//! unit-testable for all three platforms regardless of the build host.

use std::net::Ipv4Addr;

/// Operating systems with a built-in NFSv3 client we can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountPlatform {
    MacOs,
    Linux,
    Windows,
}

impl MountPlatform {
    /// The platform this binary was compiled for.
    pub const CURRENT: MountPlatform = if cfg!(target_os = "macos") {
        MountPlatform::MacOs
    } else if cfg!(windows) {
        MountPlatform::Windows
    } else {
        MountPlatform::Linux
    };
}

/// NFS client options. Editing refreshes attributes every ten seconds;
/// read-only browsing uses thirty seconds to bound external-change staleness.
///
/// The transfer sizes are per-OS maxima: Linux negotiates up to the server's
/// 1 MiB `rtmax`/`wtmax`, while the macOS client caps a transfer at 128 KiB.
/// macOS compensates with `readahead=128` — the most Read RPCs it will
/// pipeline ahead of a sequential reader — which is what keeps a high-latency
/// S3 read path busy; the server coalesces those into larger object fetches.
/// Linux read-ahead is kernel-managed and has no mount option.
///
/// A read-only mount is marked `ro` so the client greys out the affordances
/// that would fail, instead of letting the user try and collect an I/O error.
fn unix_options(platform: MountPlatform, port: u16, read_only: bool) -> String {
    let access = if read_only { "ro," } else { "" };
    let attribute_ttl = if read_only { 30 } else { 10 };
    let sizes = match platform {
        MountPlatform::MacOs => "rsize=131072,wsize=131072,readahead=128",
        _ => "rsize=1048576,wsize=1048576",
    };
    format!(
        "{access}vers=3,tcp,{sizes},actimeo={attribute_ttl},port={port},mountport={port}",
        access = access,
        sizes = sizes,
        port = port
    )
}

/// Command that mounts the local NFS server at `server_ip`:`port` on `target`.
///
/// `target` is a directory path on macOS/Linux and a drive specifier such as
/// `Z:` on Windows.
pub fn mount_argv(
    platform: MountPlatform,
    server_ip: &str,
    port: u16,
    target: &str,
    read_only: bool,
) -> Vec<String> {
    match platform {
        MountPlatform::MacOs => vec![
            "/sbin/mount_nfs".to_string(),
            "-o".to_string(),
            format!("nolocks,{}", unix_options(platform, port, read_only)),
            format!("{}:/", server_ip),
            target.to_string(),
        ],
        MountPlatform::Linux => vec![
            "mount.nfs".to_string(),
            "-o".to_string(),
            format!(
                "user,noacl,nolock,{}",
                unix_options(platform, port, read_only)
            ),
            format!("{}:/", server_ip),
            target.to_string(),
        ],
        // The Windows NFS client has no `port=` option — it resolves the
        // server through the portmapper on port 111 of `server_ip`, which the
        // manager binds for exactly this reason. The UNC hides the read-only
        // flag too: the client has no `ro`, so the server enforces it alone.
        // The share name is `!`, the spelling Microsoft documents for an
        // exported root "/": with an empty share (`\\ip\`) or `ip:/` the
        // client maps the server itself, so every path under the drive is
        // looked up as an export of its own (MNT "/<first component>") and
        // fails with error 53 while the mount command still exits 0.
        MountPlatform::Windows => {
            let mut argv = vec!["mount.exe".to_string()];
            for option in WINDOWS_MOUNT_OPTIONS {
                argv.extend(["-o".to_string(), option.to_string()]);
            }
            argv.extend([format!(r"\\{}\!", server_ip), target.to_string()]);
            argv
        }
    }
}

/// `mount.exe`'s options, each passed with an `-o` of its own: the tool reads
/// the value of a `key=value` option to the end of its argument, commas
/// included (`-o retry=2,timeout=5` is an "Invalid retry value"), so a comma
/// list applies at most its first option, and mounts all the same. That is
/// how nfsserve's recipe (`-o anon,nolock,mtype=soft,…`) left lookups
/// case-insensitive, a 0.8 s RPC timeout and one retry. `casesensitive=yes`
/// keeps `report.pdf` from resolving to `Report.pdf`; the bare
/// `casesensitive` Microsoft documents is an "Invalid command line argument".
/// `timeout` is capped at 10 s (60 lists as 10.0), so `retry=10`, the most it
/// takes, gives a RENAME (a copy, then a delete) the longest window a soft
/// mount has. `anon`, `nolock`, `mtype=soft` and `lang=ansi` match the CI
/// runner's defaults, which Microsoft documents differently for locking.
/// `fileaccess` (the server reads no requested mode) and `rsize`/`wsize`
/// (documented only up to 32 KB; the client's default is 1 MB) are left out.
/// The native smoke test asserts the listing `mount.exe` prints.
pub const WINDOWS_MOUNT_OPTIONS: [&str; 7] = [
    "anon",
    "nolock",
    "casesensitive=yes",
    "timeout=10",
    "retry=10",
    "mtype=soft",
    "lang=ansi",
];

/// How many loopback addresses a Windows mount's server can listen on: 254
/// hosts (never .0 or .255) in each of the 256 subnets of 127.88.0.0/16.
pub const WINDOWS_SERVER_IPS: u64 = 256 * 254;

/// The loopback address a Windows mount's server listens on, named by
/// `seed`: 127.88.x.y, never ending in .0 or .255. Seeds
/// 0..`WINDOWS_SERVER_IPS` name each address once, in order, and larger ones
/// wrap. The manager steps through the seeds from a random start rather than
/// drawing a fresh one per mount, so no address repeats within a run (see
/// `manager::bind_nfs_server`).
pub fn windows_server_ip(seed: u64) -> Ipv4Addr {
    const HOSTS: u64 = 254;
    let index = seed % WINDOWS_SERVER_IPS;
    Ipv4Addr::new(127, 88, (index / HOSTS) as u8, 1 + (index % HOSTS) as u8)
}

/// Unmount commands to try in order; the first one that exits 0 wins.
pub fn umount_argvs(platform: MountPlatform, target: &str) -> Vec<Vec<String>> {
    match platform {
        MountPlatform::MacOs => vec![
            vec!["umount".to_string(), target.to_string()],
            vec![
                "diskutil".to_string(),
                "unmount".to_string(),
                target.to_string(),
            ],
            vec!["umount".to_string(), "-f".to_string(), target.to_string()],
        ],
        MountPlatform::Linux => vec![
            vec!["umount".to_string(), target.to_string()],
            vec!["umount".to_string(), "-l".to_string(), target.to_string()],
        ],
        MountPlatform::Windows => vec![
            vec!["umount".to_string(), target.to_string()],
            vec!["umount".to_string(), "-f".to_string(), target.to_string()],
        ],
    }
}

/// Interactive unmount must flush the OS client's dirty pages. Force/lazy
/// detach are reserved for process-exit best effort, never reported as a safe
/// completed upload by the interactive path.
pub fn interactive_umount_argvs(platform: MountPlatform, target: &str) -> Vec<Vec<String>> {
    umount_argvs(platform, target)
        .into_iter()
        .filter(|argv| !argv.iter().any(|arg| arg == "-f" || arg == "-l"))
        .collect()
}

/// Copy-pasteable mount command shown to the user when the automatic mount
/// fails. Linux distributions commonly require root for `mount.nfs`, so the
/// suggestion is prefixed with `sudo` there.
pub fn manual_mount_command(
    platform: MountPlatform,
    server_ip: &str,
    port: u16,
    target: &str,
    read_only: bool,
) -> String {
    let argv = mount_argv(platform, server_ip, port, target, read_only);
    let rendered = argv
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");

    match platform {
        MountPlatform::Linux => format!("sudo {}", rendered),
        _ => rendered,
    }
}

/// POSIX single-quote escaping so a path with spaces survives copy-paste.
fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | ',' | '=' | '@')
        });
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Normalizes a Windows mount target to the bare `X:` drive specifier the NFS
/// client expects, rejecting anything that is not a drive letter.
///
/// Compiled on all platforms during tests so the parsing rules stay covered on
/// the macOS/Linux CI runners too.
#[cfg(any(windows, test))]
pub fn normalize_drive_spec(target: &str) -> Option<String> {
    let trimmed = target.trim().trim_end_matches(['/', '\\']);
    let mut chars = trimmed.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next() != Some(':') || chars.next().is_some() {
        return None;
    }
    Some(format!("{}:", letter.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_mounts_refresh_metadata_more_often_than_browsing_mounts() {
        for platform in [MountPlatform::MacOs, MountPlatform::Linux] {
            assert!(unix_options(platform, 1, false).contains("actimeo=10,"));
            assert!(unix_options(platform, 1, true).contains("actimeo=30,"));
        }
    }

    #[test]
    fn interactive_unmount_never_forces_or_lazily_detaches_dirty_clients() {
        for platform in [
            MountPlatform::MacOs,
            MountPlatform::Linux,
            MountPlatform::Windows,
        ] {
            for argv in interactive_umount_argvs(platform, "/mnt/photos") {
                assert!(!argv.iter().any(|arg| arg == "-f" || arg == "-l"));
            }
        }
    }

    #[test]
    fn macos_mount_uses_nolocks_and_both_ports() {
        let argv = mount_argv(
            MountPlatform::MacOs,
            "127.0.0.1",
            51234,
            "/Users/me/CloudMounts/photos",
            false,
        );
        assert_eq!(
            argv,
            vec![
                "/sbin/mount_nfs",
                "-o",
                "nolocks,vers=3,tcp,rsize=131072,wsize=131072,readahead=128,actimeo=10,port=51234,mountport=51234",
                "127.0.0.1:/",
                "/Users/me/CloudMounts/photos",
            ]
        );
    }

    #[test]
    fn linux_mount_requests_user_mount() {
        let argv = mount_argv(MountPlatform::Linux, "127.0.0.1", 9, "/mnt/photos", false);
        assert_eq!(argv[0], "mount.nfs");
        assert!(argv[2].starts_with("user,noacl,nolock,"));
        assert!(argv[2].ends_with("port=9,mountport=9"));
        assert_eq!(argv[3], "127.0.0.1:/");
        assert_eq!(argv[4], "/mnt/photos");
    }

    #[test]
    fn a_read_only_mount_is_marked_ro_for_the_client() {
        // The client greys out what it knows it cannot do; without `ro` the user
        // gets an I/O error at the end of a copy instead.
        let macos = mount_argv(MountPlatform::MacOs, "127.0.0.1", 1, "/tmp/m", true);
        assert_eq!(
            macos[2],
            "nolocks,ro,vers=3,tcp,rsize=131072,wsize=131072,readahead=128,actimeo=30,port=1,mountport=1"
        );

        let linux = mount_argv(MountPlatform::Linux, "127.0.0.1", 1, "/tmp/m", true);
        assert_eq!(
            linux[2],
            "user,noacl,nolock,ro,vers=3,tcp,rsize=1048576,wsize=1048576,actimeo=30,port=1,mountport=1"
        );

        // A writable mount says nothing about access at all.
        assert!(
            !mount_argv(MountPlatform::MacOs, "127.0.0.1", 1, "/tmp/m", false)[2].contains("ro,")
        );
        assert!(
            !mount_argv(MountPlatform::Linux, "127.0.0.1", 1, "/tmp/m", false)[2].contains(",ro,")
        );
    }

    #[test]
    fn writes_are_sized_like_reads() {
        // Without wsize the client falls back to a much smaller write size and
        // a copy turns into many more round trips than it needs. Each platform
        // asks for its own maximum: 128 KiB on macOS, 1 MiB on Linux (the
        // server's rtmax/wtmax).
        for read_only in [true, false] {
            let macos = mount_argv(MountPlatform::MacOs, "127.0.0.1", 1, "/tmp/m", read_only);
            assert!(macos[2].contains("rsize=131072"), "{}", macos[2]);
            assert!(macos[2].contains("wsize=131072"), "{}", macos[2]);

            let linux = mount_argv(MountPlatform::Linux, "127.0.0.1", 1, "/tmp/m", read_only);
            assert!(linux[2].contains("rsize=1048576"), "{}", linux[2]);
            assert!(linux[2].contains("wsize=1048576"), "{}", linux[2]);
        }
    }

    #[test]
    fn macos_pipelines_reads_against_the_high_latency_backend() {
        // readahead=128 is the macOS client's maximum; the default of 16 leaves
        // a sequential read mostly waiting on S3 round trips.
        let argv = mount_argv(MountPlatform::MacOs, "127.0.0.1", 1, "/tmp/m", false);
        assert!(argv[2].contains("readahead=128"), "{}", argv[2]);
        // Linux has no such mount option; the kernel manages read-ahead.
        let linux = mount_argv(MountPlatform::Linux, "127.0.0.1", 1, "/tmp/m", false);
        assert!(!linux[2].contains("readahead"), "{}", linux[2]);
    }

    #[test]
    fn windows_mount_targets_the_server_unc_share() {
        // The UNC host is the per-mount loopback IP whose port 111 the server
        // bound; the portmapper there answers with its own port. The share
        // name is `!`, the documented spelling for the exported root "/":
        // `\\ip\` and `ip:/` map the server itself, so every path under the
        // drive is then looked up as an export of its own. Every option has
        // an `-o` of its own: in a comma list `mount.exe` applies at most the
        // first.
        let argv = mount_argv(MountPlatform::Windows, "127.88.41.7", 111, "Z:", false);
        assert_eq!(
            argv,
            vec![
                "mount.exe",
                "-o",
                "anon",
                "-o",
                "nolock",
                "-o",
                "casesensitive=yes",
                "-o",
                "timeout=10",
                "-o",
                "retry=10",
                "-o",
                "mtype=soft",
                "-o",
                "lang=ansi",
                r"\\127.88.41.7\!",
                "Z:",
            ]
        );
    }

    #[test]
    fn every_seed_names_a_usable_windows_loopback_address() {
        let domain = 0..WINDOWS_SERVER_IPS;
        // Each of 127.88.0.1 ..= 127.88.255.254 once, none ending in .0 or
        // .255, and the same seed always the same address.
        let addresses: Vec<Ipv4Addr> = domain.clone().map(windows_server_ip).collect();
        let distinct: std::collections::BTreeSet<&Ipv4Addr> = addresses.iter().collect();
        assert_eq!(distinct.len(), addresses.len());
        assert_eq!(addresses[0], Ipv4Addr::new(127, 88, 0, 1));
        assert_eq!(addresses[1], Ipv4Addr::new(127, 88, 0, 2));
        assert_eq!(addresses[254], Ipv4Addr::new(127, 88, 1, 1));
        assert_eq!(
            addresses[addresses.len() - 1],
            Ipv4Addr::new(127, 88, 255, 254)
        );
        for seed in domain.chain([u64::MAX, u64::MAX / 3, 1 << 40]) {
            let address = windows_server_ip(seed);
            let [a, b, _, host] = address.octets();
            assert_eq!((a, b), (127, 88), "{seed}");
            assert!((1..=254).contains(&host), "{seed}: {address}");
            assert_eq!(windows_server_ip(seed), address);
        }
        // Larger seeds wrap onto the same addresses.
        assert_eq!(windows_server_ip(WINDOWS_SERVER_IPS), addresses[0]);
    }

    #[test]
    fn umount_falls_back_after_the_plain_attempt() {
        let macos = umount_argvs(MountPlatform::MacOs, "/tmp/m");
        assert_eq!(macos[0], vec!["umount", "/tmp/m"]);
        assert_eq!(macos[1], vec!["diskutil", "unmount", "/tmp/m"]);

        let linux = umount_argvs(MountPlatform::Linux, "/tmp/m");
        assert_eq!(linux[0], vec!["umount", "/tmp/m"]);
        assert_eq!(linux[1], vec!["umount", "-l", "/tmp/m"]);
    }

    #[test]
    fn manual_command_is_sudo_prefixed_only_on_linux() {
        let linux =
            manual_mount_command(MountPlatform::Linux, "127.0.0.1", 42, "/mnt/photos", false);
        assert!(linux.starts_with("sudo mount.nfs -o "));

        let macos = manual_mount_command(MountPlatform::MacOs, "127.0.0.1", 42, "/tmp/m", false);
        assert!(macos.starts_with("/sbin/mount_nfs -o "));
        assert!(!macos.contains("sudo"));
    }

    #[test]
    fn the_manual_command_mounts_the_mode_that_was_asked_for() {
        // The user runs this by hand after an automatic mount failed, so it has
        // to reproduce the same mount, not a writable one.
        let read_only = manual_mount_command(MountPlatform::MacOs, "127.0.0.1", 42, "/tmp/m", true);
        assert!(read_only.contains(",ro,"), "{}", read_only);
    }

    #[test]
    fn manual_command_quotes_paths_with_spaces() {
        let cmd = manual_mount_command(
            MountPlatform::MacOs,
            "127.0.0.1",
            1,
            "/Users/me/My Drive",
            false,
        );
        assert!(cmd.ends_with("'/Users/me/My Drive'"), "{}", cmd);
    }

    #[test]
    fn drive_specs_are_normalized_and_validated() {
        assert_eq!(normalize_drive_spec("z:").as_deref(), Some("Z:"));
        assert_eq!(normalize_drive_spec("Z:\\").as_deref(), Some("Z:"));
        assert_eq!(normalize_drive_spec(" Z:/ ").as_deref(), Some("Z:"));
        assert_eq!(normalize_drive_spec("Z:\\mnt"), None);
        assert_eq!(normalize_drive_spec("/mnt/photos"), None);
        assert_eq!(normalize_drive_spec(""), None);
        assert_eq!(normalize_drive_spec("1:"), None);
    }
}
