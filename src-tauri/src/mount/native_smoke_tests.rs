//! Opt-in host-kernel NFS smoke test. Uses disposable paths and a local HTTP
//! storage fixture; it is not evidence of AWS/R2/MinIO deployment compatibility.
use super::*;
use crate::test_s3::{serve, Request, Response};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use sha2::{Digest, Sha256};

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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the host NFS client and permission to create a temporary loopback mount"]
async fn native_nfs_write_read_rename_unmount() {
    let objects = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let fixture = serve({
        let objects = objects.clone();
        move |request| {
            let objects = objects.clone();
            async move { respond(request, &mut objects.lock().unwrap()) }
        }
    })
    .await;
    let root = std::env::temp_dir().join(format!(
        "r2-native-nfs-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let target = root.join("mount");
    let staging = root.join("staging");
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
    let listener = NFSTcpListener::bind("127.0.0.1:0", fs.clone())
        .await
        .unwrap();
    let port = listener.get_listen_port();
    let server = tokio::spawn(async move { listener.handle_forever().await });
    let target_text = target.to_string_lossy().to_string();
    let argv = super::super::platform::mount_argv(
        super::super::platform::MountPlatform::CURRENT,
        "127.0.0.1",
        port,
        &target_text,
        false,
    );
    let mount = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let mounted = matches!(&mount,Ok(Ok(output)) if output.status.success());
    let mut failure = if mounted {
        None
    } else {
        Some(format!("Native NFS mount unavailable: {mount:?}"))
    };
    if mounted {
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let file = target.join("file + 中文.txt");
            let renamed = target.join("renamed.txt");
            tokio::fs::write(&file, b"native NFS bytes").await?;
            if tokio::fs::read(&file).await? != b"native NFS bytes" {
                return Err(std::io::Error::other("Native write/read content mismatch"));
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
            if objects.lock().unwrap().contains_key("file + 中文.txt") {
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
            Ok::<_, std::io::Error>(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            failure = Some(format!("Native NFS operations failed: {result:?}"));
        }
    }
    // This path was created solely by this test. Forced cleanup may discard
    // only fixture data, and prevents leaving a dead loopback mount on failure.
    let unmount = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("umount")
            .args(["-f", &target_text])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    fs.stop_accepting_writes();
    server.abort();
    if !mounted || matches!(&unmount,Ok(Ok(output)) if output.status.success()) {
        let _ = tokio::fs::remove_dir_all(&root).await;
    } else {
        failure = Some(format!(
            "Fixture mount cleanup failed at {target_text}: {unmount:?}"
        ));
    }
    assert!(failure.is_none(), "{}", failure.unwrap_or_default());
}
