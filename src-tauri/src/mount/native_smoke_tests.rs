//! Opt-in host-kernel NFS smoke test. Uses disposable paths and a local HTTP
//! storage fixture; it is not evidence of AWS/R2/MinIO deployment compatibility.
use super::*;
use crate::test_s3::{serve, Request, Response};
use nfsserve::tcp::NFSTcp;
use sha2::{Digest, Sha256};

#[cfg(unix)]
#[path = "vm_powercut_tests.rs"]
mod vm_powercut_tests;

#[path = "native_smoke_listing.rs"]
mod listing;
#[path = "native_smoke_probes.rs"]
mod probes;

#[derive(Clone)]
struct Object {
    data: Vec<u8>,
    meta: HashMap<String, String>,
}
impl Object {
    fn etag(&self) -> String {
        format!("\"{:x}\"", Sha256::digest(&self.data))
    }
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn body(request: &Request) -> Vec<u8> {
    if !request
        .headers
        .get("content-encoding")
        .is_some_and(|value| value.contains("aws-chunked"))
    {
        return request.body.clone();
    }
    let mut result = Vec::new();
    let mut input = request.body.as_slice();
    while let Some(end) = input.windows(2).position(|bytes| bytes == b"\r\n") {
        let header = String::from_utf8_lossy(&input[..end]);
        let length = usize::from_str_radix(header.split(';').next().unwrap(), 16).unwrap();
        input = &input[end + 2..];
        if length == 0 {
            break;
        }
        result.extend_from_slice(&input[..length]);
        input = &input[length + 2..];
    }
    result
}
fn respond(request: Request, objects: &mut HashMap<String, Object>) -> Response {
    let url = reqwest::Url::parse(&format!("http://fixture{}", request.path)).unwrap();
    let key = urlencoding::decode(url.path().strip_prefix("/photos/").unwrap_or(""))
        .unwrap()
        .into_owned();
    let query: HashMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if request.method == "GET" && query.contains_key("list-type") {
        let prefix = query.get("prefix").map(String::as_str).unwrap_or("");
        let delimiter = query.get("delimiter").map(String::as_str).unwrap_or("");
        let mut contents = String::new();
        let mut directories = std::collections::BTreeSet::new();
        let mut keys: Vec<_> = objects.keys().collect();
        keys.sort();
        for key in keys {
            let Some(relative) = key.strip_prefix(prefix) else {
                continue;
            };
            if delimiter == "/" {
                if let Some((directory, _)) = relative.split_once('/') {
                    directories.insert(format!("{prefix}{directory}/"));
                    continue;
                }
            }
            let object = &objects[key];
            contents.push_str(&format!("<Contents><Key>{}</Key><Size>{}</Size><ETag>{}</ETag><LastModified>2026-09-12T00:00:00Z</LastModified></Contents>",xml(key),object.data.len(),xml(&object.etag())));
        }
        for directory in directories {
            contents.push_str(&format!(
                "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
                xml(&directory)
            ));
        }
        return Response::xml(
            200,
            &format!(
                "<ListBucketResult><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>"
            ),
        );
    }
    let current = objects.get(&key);
    if request
        .headers
        .get("if-none-match")
        .is_some_and(|value| value == "*")
        && current.is_some()
        || request
            .headers
            .get("if-match")
            .is_some_and(|value| current.is_none_or(|object| object.etag() != *value))
    {
        return Response::xml(412, "<Error><Code>PreconditionFailed</Code></Error>");
    }
    match request.method.as_str() {
        "HEAD" => match current {
            Some(object) => {
                let mut response = Response::empty(200)
                    .header("content-length", object.data.len())
                    .header("etag", object.etag())
                    .header("last-modified", "Sat, 12 Sep 2026 00:00:00 GMT");
                for (name, value) in &object.meta {
                    response = response.header(&format!("x-amz-meta-{name}"), value);
                }
                response
            }
            None => Response::empty(404),
        },
        "GET" => match current {
            Some(object) => {
                let (start, end, status) = if let Some(range) = request.headers.get("range") {
                    let (start, end) = range.trim_start_matches("bytes=").split_once('-').unwrap();
                    (
                        start.parse::<usize>().unwrap(),
                        end.parse::<usize>().unwrap(),
                        206,
                    )
                } else {
                    (0, object.data.len().saturating_sub(1), 200)
                };
                let mut response = Response {
                    status,
                    headers: vec![("etag".into(), object.etag())],
                    body: if object.data.is_empty() {
                        Vec::new()
                    } else {
                        object.data[start..=end].to_vec()
                    },
                };
                if status == 206 {
                    response = response.header(
                        "content-range",
                        format!("bytes {start}-{end}/{}", object.data.len()),
                    );
                }
                response
            }
            None => Response::empty(404),
        },
        "PUT" => {
            let data = if let Some(source) = request.headers.get("x-amz-copy-source") {
                let source = urlencoding::decode(source).unwrap();
                let source = source
                    .trim_start_matches('/')
                    .strip_prefix("photos/")
                    .unwrap();
                let Some(object) = objects.get(source) else {
                    return Response::empty(404);
                };
                if request
                    .headers
                    .get("x-amz-copy-source-if-match")
                    .is_some_and(|value| *value != object.etag())
                {
                    return Response::xml(412, "<Error><Code>PreconditionFailed</Code></Error>");
                }
                object.data.clone()
            } else {
                body(&request)
            };
            let meta = request
                .headers
                .iter()
                .filter_map(|(name, value)| {
                    name.strip_prefix("x-amz-meta-")
                        .map(|name| (name.to_string(), value.clone()))
                })
                .collect();
            let object = Object { data, meta };
            let etag = object.etag();
            objects.insert(key, object);
            if request.headers.contains_key("x-amz-copy-source") {
                Response::xml(
                    200,
                    &format!(
                        "<CopyObjectResult><ETag>{}</ETag></CopyObjectResult>",
                        xml(&etag)
                    ),
                )
            } else {
                Response::empty(200).header("etag", etag)
            }
        }
        "DELETE" => {
            objects.remove(&key);
            Response::empty(204)
        }
        _ => Response::empty(400),
    }
}

#[test]
#[ignore = "local storage daemon for the isolated native IPC audit; requires R2_AUDIT_SERVER_READY"]
fn ipc_storage_fixture_daemon() {
    let ready =
        PathBuf::from(std::env::var_os("R2_AUDIT_SERVER_READY").expect("fixture readiness path"));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        async fn endpoint(source:bool)->crate::test_s3::Fixture {
            let mut initial: HashMap<String,Object>=HashMap::new();initial.insert("source.txt".into(),Object {data:if source {b"correct-data".to_vec()} else {b"wrong---data".to_vec()},meta:HashMap::new()});
            let objects=Arc::new(std::sync::Mutex::new(initial));let calls=Arc::new(std::sync::Mutex::new(Vec::<(String,String)>::new()));let deny_delete=Arc::new(AtomicBool::new(source));
            serve(move |request|{let objects=objects.clone();let calls=calls.clone();let deny_delete=deny_delete.clone();async move {
                let path=request.path.split('?').next().unwrap_or_default().to_string();
                if path=="/__fixture_status" {
                    let data:HashMap<String,String>=objects.lock().unwrap().iter().map(|(key,object)|(key.clone(),String::from_utf8_lossy(&object.data).into_owned())).collect();
                    return Response {status:200,headers:vec![("content-type".into(),"application/json".into())],body:serde_json::to_vec(&serde_json::json!({"objects":data,"calls":calls.lock().unwrap().clone()})).unwrap()};
                }
                calls.lock().unwrap().push((request.method.clone(),path.clone()));
                if request.method=="DELETE" && path=="/photos/source.txt" && deny_delete.swap(false,Ordering::SeqCst) {return Response::xml(403,"<Error><Code>AccessDenied</Code><Message>Fixture denies the first source deletion</Message></Error>");}
                respond(request,&mut objects.lock().unwrap())
            }}).await
        }
        let source=endpoint(true).await;let destination=endpoint(false).await;
        tokio::fs::write(&ready,serde_json::to_vec(&serde_json::json!({"source":source.endpoint,"destination":destination.endpoint})).unwrap()).await.unwrap();
        std::future::pending::<()>().await;
        drop((source,destination));
    });
}

/// Drive selection is explicit and checks assigned letters, rather than
/// opening a potentially disconnected/user-owned drive to test its existence.
fn explicit_test_drive(value: Option<&str>, assigned_drives: u32) -> Result<String, String> {
    let value =
        value.ok_or("Set R2_NFS_TEST_DRIVE to an explicitly reserved, unused drive letter")?;
    let drive = super::super::platform::normalize_drive_spec(value)
        .ok_or("R2_NFS_TEST_DRIVE must be a drive letter such as Z:, not a folder or share")?;
    let bit = u32::from(drive.as_bytes()[0] - b'A');
    if assigned_drives & (1 << bit) != 0 {
        return Err(format!(
            "Refusing native NFS smoke: drive {drive} is already assigned"
        ));
    }
    Ok(drive)
}

#[cfg(windows)]
fn assigned_windows_drives() -> Result<u32, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    // SAFETY: GetLogicalDrives has no parameters or pointer requirements and
    // returns an owned bitmask. A zero result is failure, never proof of no drives.
    let drives = unsafe { GetLogicalDrives() };
    if drives == 0 {
        return Err(format!(
            "Unable to inventory Windows drive letters: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(drives)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn windows_nfs_tool(name: &str) -> Result<String, String> {
    let root = std::env::var_os("SystemRoot").ok_or("SystemRoot is missing")?;
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Err("SystemRoot is not an absolute Windows directory".into());
    }
    let path = root.join("System32").join(name);
    if !path.is_file() {
        return Err(format!(
            "Windows Client for NFS is unavailable: {} is missing",
            path.display()
        ));
    }
    Ok(path.to_string_lossy().into_owned())
}

// Opt-in diagnostics (R2_NFS_TRACE). They only observe and never change what
// the test asserts. A native client that mounts and then fails on its first
// file operation is otherwise opaque; this shows which RPC it stopped at.

/// On unless `R2_NFS_TRACE` is unset, empty, `0` or `false`.
fn trace_enabled() -> bool {
    std::env::var_os("R2_NFS_TRACE").is_some_and(|value| {
        let value = value.to_string_lossy();
        !(value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false"))
    })
}

/// Streams nfsserve's own tracing (every accepted connection and every
/// portmap, mount and NFS call) into the test's stderr, captured like `diag`
/// and in order with it.
fn install_nfs_trace() {
    use tracing_subscriber::{
        filter::{LevelFilter, Targets},
        layer::SubscriberExt,
        util::SubscriberInitExt,
        Layer,
    };
    if !trace_enabled() {
        return;
    }
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(tracing_subscriber::fmt::TestWriter::with_stderr())
        .with_filter(Targets::new().with_target("nfsserve", LevelFilter::DEBUG));
    let _ = tracing_subscriber::registry().with(layer).try_init();
}

/// A stderr message whose first line is stamped like the tracing output, so
/// tool output and the server trace read in time order.
fn diag(message: impl AsRef<str>) {
    if trace_enabled() {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        eprintln!("[diag {now}] {}", message.as_ref());
    }
}

type ToolRun = Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed>;

/// A tool's console output as text. NUL bytes are dropped so UTF-16 console
/// output stays readable.
fn console_text(bytes: &[u8]) -> String {
    let text: Vec<u8> = bytes.iter().copied().filter(|byte| *byte != 0).collect();
    String::from_utf8_lossy(&text).into_owned()
}

/// How a tool run ended, one prefixed line per output line.
fn render_tool_run(run: &ToolRun) -> String {
    match run {
        Ok(Ok(output)) => {
            let mut rendered = output.status.to_string();
            for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
                for line in console_text(bytes)
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                {
                    rendered.push_str(&format!("\n  {stream}| {line}"));
                }
            }
            rendered
        }
        Ok(Err(error)) => format!("could not run: {error}"),
        Err(_) => "timed out".to_string(),
    }
}

/// Runs an external tool and prints how it ended. A missing tool, a hang or a
/// failure is evidence too, so nothing here can fail the test.
#[cfg_attr(not(windows), allow(dead_code))]
async fn probe(stage: &str, program: &str, args: &[&str]) {
    if !trace_enabled() {
        return;
    }
    let started = std::time::Instant::now();
    let run = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let command = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    diag(format!(
        "{stage}: {command} ({} ms)\n{}",
        started.elapsed().as_millis(),
        render_tool_run(&run)
    ));
}

/// What Windows believes about the mount. A drive listed as `Unavailable`
/// despite a successful `mount.exe` is one whose every path fails.
#[cfg_attr(not(windows), allow(dead_code))]
async fn mount_state_probes(stage: &str) {
    for (tool, args) in [("mount.exe", &[][..]), ("net.exe", &["use"][..])] {
        match windows_nfs_tool(tool) {
            Ok(program) => probe(stage, &program, args).await,
            Err(error) => diag(format!("{stage}: {tool} not run: {error}")),
        }
    }
}

#[test]
fn native_windows_smoke_requires_an_explicit_unused_drive() {
    let assigned = (1 << 2) | (1 << 25); // C: and Z: are in use.
    assert!(explicit_test_drive(None, assigned).is_err());
    for invalid in [
        "",
        "*",
        "C:\\Users\\user",
        "\\\\server\\share",
        "E:folder",
        "1:",
    ] {
        assert!(
            explicit_test_drive(Some(invalid), assigned).is_err(),
            "{invalid}"
        );
    }
    assert!(explicit_test_drive(Some("C:"), assigned).is_err());
    assert!(explicit_test_drive(Some("z:"), assigned).is_err());
    assert_eq!(explicit_test_drive(Some(" y:\\ "), assigned).unwrap(), "Y:");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the host NFS client and permission to create a temporary loopback mount"]
async fn native_nfs_write_read_rename_unmount() {
    install_nfs_trace();
    #[cfg(windows)]
    let (drive, mount_tool, unmount_tool) = {
        let value = std::env::var("R2_NFS_TEST_DRIVE").ok();
        let drive = explicit_test_drive(
            value.as_deref(),
            assigned_windows_drives().expect("read drive inventory"),
        )
        .expect("validate explicitly reserved test drive");
        let mount = windows_nfs_tool("mount.exe").expect("Windows Client for NFS mount tool");
        let unmount = windows_nfs_tool("umount.exe").expect("Windows Client for NFS unmount tool");
        (drive, mount, unmount)
    };
    let objects = Arc::new(std::sync::Mutex::new(HashMap::from(
        [
            (probes::CASE_SEED_KEY, probes::CASE_SEED_DATA),
            (probes::EXISTING_SEED_KEY, probes::EXISTING_SEED_DATA),
        ]
        .map(|(key, data)| {
            (
                key.to_string(),
                Object {
                    data: data.to_vec(),
                    meta: HashMap::new(),
                },
            )
        }),
    )));
    let fixture = serve({
        let objects = objects.clone();
        move |request| {
            let objects = objects.clone();
            async move {
                diag(format!(
                    "storage fixture: {} {}",
                    request.method, request.path
                ));
                respond(request, &mut objects.lock().unwrap())
            }
        }
    })
    .await;
    let root = std::env::temp_dir().join(format!(
        "r2-native-nfs-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    #[cfg(not(windows))]
    let target = root.join("mount");
    #[cfg(windows)]
    let target = PathBuf::from(format!("{drive}\\"));
    let staging = root.join("staging");
    #[cfg(not(windows))]
    tokio::fs::create_dir_all(&target).await.unwrap();
    tokio::fs::create_dir_all(&staging).await.unwrap();
    let fs = S3NfsFs::new(fixture.client.clone(), "photos".into(), false, staging);
    fs.configure_transfer(crate::move_transfer::config::MoveConfig::Aws(
        crate::providers::aws::AwsConfig {
            bucket: "photos".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            region: "us-east-1".into(),
            endpoint_scheme: None,
            endpoint_host: None,
            force_path_style: true,
        },
    ));
    // The product's own bind: on Windows a loopback address of its own,
    // never reused within a run.
    let listener =
        super::super::manager::bind_nfs_server(super::super::platform::MountPlatform::CURRENT, &fs)
            .await
            .unwrap();
    let port = listener.get_listen_port();
    let server_ip = listener.get_listen_ip().to_string();
    let server = tokio::spawn(async move { listener.handle_forever().await });
    #[cfg(not(windows))]
    let target_text = target.to_string_lossy().to_string();
    #[cfg(windows)]
    let target_text = drive.clone();
    let argv = super::super::platform::mount_argv(
        super::super::platform::MountPlatform::CURRENT,
        &server_ip,
        port,
        &target_text,
        false,
    );
    #[cfg(windows)]
    let argv = {
        let mut argv = argv;
        // Resolve only the executable; retain every production mount option.
        argv[0] = mount_tool;
        // Recheck just before mounting. mount.exe is not given any force or
        // replacement option if another process claims the letter meanwhile.
        explicit_test_drive(Some(&drive), assigned_windows_drives().unwrap()).unwrap();
        argv
    };
    eprintln!(
        "Native NFS fixture mount: {argv:?}; disposable data: {}",
        root.display()
    );
    let mount = tokio::time::timeout(
        Duration::from_secs(if cfg!(windows) { 30 } else { 15 }),
        tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    diag(format!("mount command: {}", render_tool_run(&mount)));
    let mounted = matches!(&mount,Ok(Ok(output)) if output.status.success());
    let mut failure = if mounted {
        None
    } else {
        Some(format!("Native NFS mount unavailable: {mount:?}"))
    };
    if mounted {
        // The Windows client sends names in the system ANSI code page (the
        // mount asks for lang=ansi). A character outside it arrives as "?",
        // which the server refuses, or, where Windows' best-fit tables have
        // an ASCII look-alike for it, as that look-alike (`čas.txt` was
        // stored as `cas.txt`), which the server cannot tell from a real
        // name; the beyond-ASCII probes below log each outcome. Unless the code
        // page is UTF-8, a character inside it arrives as code-page bytes,
        // refused unless they happen to be valid UTF-8. So only ASCII names
        // round-trip on every Windows system.
        let name = if cfg!(windows) {
            "file + plus.txt"
        } else {
            "file + 中文.txt"
        };
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let file = target.join(name);
            let renamed = target.join("renamed.txt");
            tokio::fs::write(&file, b"native NFS bytes").await?;
            if tokio::fs::read(&file).await? != b"native NFS bytes" {
                return Err(std::io::Error::other("Native write/read content mismatch"));
            }
            if !objects.lock().unwrap().contains_key(name) {
                return Err(std::io::Error::other(
                    "The native client did not preserve the exact object key",
                ));
            }
            tokio::fs::rename(&file, &renamed).await?;
            let cloud = objects
                .lock()
                .unwrap()
                .get("renamed.txt")
                .map(|object| object.data.clone());
            if cloud.as_deref() != Some(b"native NFS bytes".as_slice()) {
                return Err(std::io::Error::other(
                    "Storage fixture did not receive the exact native write",
                ));
            }
            if objects.lock().unwrap().contains_key(name) {
                return Err(std::io::Error::other(
                    "Rename did not delete the original storage object",
                ));
            }
            if tokio::fs::read(&renamed).await? != b"native NFS bytes" {
                return Err(std::io::Error::other("Native rename content mismatch"));
            }
            tokio::fs::remove_file(&renamed).await?;
            if objects.lock().unwrap().contains_key("renamed.txt") {
                return Err(std::io::Error::other(
                    "Native delete left the storage object behind",
                ));
            }
            // Every probe runs whatever the ones before it found, each under
            // its own time limit, so that one run reports every failure and
            // which probe it came from. Beyond ASCII the outcome depends on
            // the code page and on best-fit; what the server must keep is
            // that no name is stored under a key other than its own or its
            // documented look-alike.
            let outcomes = [
                probes::named(
                    "case probe",
                    probes::case_variants_stay_apart(&fs, &target, &objects),
                )
                .await,
                probes::named(
                    "overwrite probe",
                    probes::an_existing_file_takes_new_bytes(&fs, &target, &objects),
                )
                .await,
                probes::named(
                    "folder probe",
                    probes::a_folder_takes_a_file_and_goes_again(&fs, &target, &objects),
                )
                .await,
            ];
            #[cfg(windows)]
            let outcomes = outcomes.into_iter().chain([probes::named(
                "beyond-ASCII probes",
                probes::beyond_ascii_names_keep_their_keys(&target, &objects),
            )
            .await]);
            probes::all_passed(outcomes)
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            failure = Some(format!("Native NFS operations failed: {result:?}"));
            #[cfg(windows)]
            mount_state_probes("after the operations failed").await;
        }
        // Only now, so the operations above ran with the client exactly as
        // users get it: mount.exe mounts even when it ignored some options,
        // and its listing is what shows which took effect.
        #[cfg(windows)]
        if let Err(error) = listing::mount_options_took_effect(&drive).await {
            failure = Some(match failure {
                Some(operations) => format!("{operations}; {error}"),
                None => error,
            });
        }
    }
    // Only a successful mount command establishes ownership. A failed or
    // timed-out mount can have an uncertain result: do not unmount a drive
    // merely because it matches the requested letter, and retain diagnostics.
    let mut unmounted = false;
    if mounted {
        #[cfg(not(windows))]
        let unmount_argv = ["umount".to_string(), "-f".to_string(), target_text.clone()];
        #[cfg(windows)]
        let unmount_argv = [unmount_tool.clone(), target_text.clone()];
        let unmount = tokio::time::timeout(
            Duration::from_secs(if cfg!(windows) { 15 } else { 10 }),
            tokio::process::Command::new(&unmount_argv[0])
                .args(&unmount_argv[1..])
                .kill_on_drop(true)
                .output(),
        )
        .await;
        unmounted = matches!(&unmount, Ok(Ok(output)) if output.status.success());
        if !unmounted {
            failure = Some(format!(
                "Fixture mount cleanup failed at {target_text}: {unmount:?}. Disposable data: {}",
                root.display()
            ));
            #[cfg(windows)]
            {
                // This drive was unused before our successful mount. Forced
                // cleanup may discard fixture bytes only, and cannot turn a
                // failed normal-unmount acceptance test into a passing one.
                let cleanup = tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::process::Command::new(&unmount_tool)
                        .args(["-f", &target_text])
                        .kill_on_drop(true)
                        .output(),
                )
                .await;
                unmounted = matches!(&cleanup, Ok(Ok(output)) if output.status.success());
                eprintln!("Owned Windows fixture forced cleanup: {cleanup:?}");
            }
        }
        #[cfg(windows)]
        if unmounted {
            match assigned_windows_drives() {
                Ok(drives) if explicit_test_drive(Some(&drive), drives).is_ok() => {}
                result => {
                    unmounted = false;
                    failure = Some(format!(
                        "Fixture drive {drive} remains assigned or cannot be verified after unmount: {result:?}"
                    ));
                }
            }
        }
    }
    fs.stop_accepting_writes();
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        fs.wait_for_mutations().await;
        fs.wait_for_flushes().await;
    })
    .await
    .is_ok();
    server.abort();
    let _ = server.await;
    if unmounted && settled {
        let _ = tokio::fs::remove_dir_all(&root).await;
    } else {
        eprintln!(
            "Native fixture retained at {}; mount_confirmed={mounted}, unmount_confirmed={unmounted}, VFS_settled={settled}",
            root.display()
        );
        if !settled {
            failure = Some(format!(
                "Native fixture operations did not settle; data retained at {}",
                root.display()
            ));
        }
    }

    assert!(failure.is_none(), "{}", failure.unwrap_or_default());
}
