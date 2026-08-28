//! Per-OS NFS client command construction.
//!
//! The mount itself is performed by the operating system's built-in NFSv3
//! client talking to the localhost server started by [`crate::mount::manager`].
//! Every command is built here as a pure `argv` vector so the exact flags are
//! unit-testable for all three platforms regardless of the build host.

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

/// NFS client options for the unix mount commands. `actimeo` keeps attribute
/// lookups off the network for two minutes, which matters a lot when every
/// miss is an S3 round trip.
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
    let sizes = match platform {
        MountPlatform::MacOs => "rsize=131072,wsize=131072,readahead=128",
        _ => "rsize=1048576,wsize=1048576",
    };
    format!(
        "{access}vers=3,tcp,{sizes},actimeo=120,port={port},mountport={port}",
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
            format!("user,noacl,nolock,{}", unix_options(platform, port, read_only)),
            format!("{}:/", server_ip),
            target.to_string(),
        ],
        // The Windows NFS client has no `port=` option — it resolves the
        // server through the portmapper on port 111 of `server_ip`, which the
        // manager binds for exactly this reason. The UNC hides the read-only
        // flag too: the client has no `ro`, so the server enforces it alone.
        // Options otherwise kept identical to the upstream nfsserve recipe.
        MountPlatform::Windows => vec![
            "mount.exe".to_string(),
            "-o".to_string(),
            "anon,nolock,mtype=soft,fileaccess=6,casesensitive,lang=ansi,rsize=128,wsize=128,timeout=60,retry=2".to_string(),
            format!(r"\\{}\", server_ip),
            target.to_string(),
        ],
    }
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
                "nolocks,vers=3,tcp,rsize=131072,wsize=131072,readahead=128,actimeo=120,port=51234,mountport=51234",
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
            "nolocks,ro,vers=3,tcp,rsize=131072,wsize=131072,readahead=128,actimeo=120,port=1,mountport=1"
        );

        let linux = mount_argv(MountPlatform::Linux, "127.0.0.1", 1, "/tmp/m", true);
        assert_eq!(
            linux[2],
            "user,noacl,nolock,ro,vers=3,tcp,rsize=1048576,wsize=1048576,actimeo=120,port=1,mountport=1"
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
        // bound; the portmapper there answers with its own port.
        let argv = mount_argv(MountPlatform::Windows, "127.88.0.1", 111, "Z:", false);
        assert_eq!(
            argv,
            vec![
                "mount.exe",
                "-o",
                "anon,nolock,mtype=soft,fileaccess=6,casesensitive,lang=ansi,rsize=128,wsize=128,timeout=60,retry=2",
                r"\\127.88.0.1\",
                "Z:",
            ]
        );
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
