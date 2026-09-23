use super::*;
use crate::test_s3::{serve, Request, Response};

fn filesystem(client: Client, label: &str) -> S3NfsFs {
    let fs = S3NfsFs::new(
        client,
        "photos".into(),
        false,
        std::env::temp_dir().join(format!(
            "r2-nfs-protocol-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        )),
    );
    // The fixture implements the native AWS conditional-operation contract.
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
    fs
}
async fn intern(fs: &S3NfsFs, key: &str) -> fileid3 {
    let id = fs
        .intern_child(key, ROOT_ID, EntryKind::File, 0, 0)
        .unwrap();
    fs.inner.dirs.write().unwrap().insert(
        ROOT_ID,
        DirListing::complete(Arc::new(vec![DirChild {
            fileid: id,
            name: key.into(),
        }])),
    );
    // This fixture begins with a newly created, empty local stage.
    let mut stage = fs.reset_stage(id, &fs.inode(id).unwrap()).await.unwrap();
    stage.publication_guard = Some(stage::PublicationGuard::Absent);
    drop(stage);
    id
}

#[tokio::test]
async fn failed_head_never_reaches_zero_byte_put() {
    let fixture = serve(|request| async move {
        if request.method == "HEAD" {
            Response::empty(503)
        } else {
            Response::empty(200)
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "head-error");
    let name: filename3 = b"existing".as_slice().into();
    assert!(fs.create(ROOT_ID, &name, sattr3::default()).await.is_err());
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "HEAD")
            .count(),
        3,
        "transient HEAD failures should retry in the common executor"
    );
    assert!(
        requests.iter().all(|request| request.method == "HEAD"),
        "a failed existence probe must never reach zero-byte PUT"
    );
}

#[tokio::test]
async fn editing_a_cached_zero_byte_inode_preserves_new_remote_content() {
    let fixture = serve(|request| async move {
        if request.method == "HEAD" {
            Response::empty(200)
                .header("content-length", 4)
                .header("etag", "\"version\"")
        } else {
            Response::xml(200, "tail").header("etag", "\"version\"")
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "stale-size");
    let id = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 0, 0)
        .unwrap();
    fs.write(id, 1, b"X").await.unwrap();
    assert_eq!(fs.read(id, 0, 100).await.unwrap().0, b"tXil");
    assert!(fixture.requests.lock().unwrap().iter().any(|r| {
        r.method == "GET"
            && r.headers
                .get("if-match")
                .is_some_and(|value| value == "\"version\"")
    }));
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn a_lost_put_response_requires_matching_content_not_only_saved_metadata() {
    for changed in [false, true] {
        let fixture = serve(move |request| async move {
            if request.method == "HEAD" {
                Response::empty(200)
                    .header("content-length", 4)
                    .header("etag", "\"remote\"")
                    .header("x-amz-meta-r2-stage-snapshot", "payload.snapshot")
            } else {
                Response::xml(200, if changed { "else" } else { "ours" })
                    .header("etag", "\"remote\"")
            }
        })
        .await;
        let fs = filesystem(fixture.client.clone(), "unknown-snapshot");
        tokio::fs::create_dir_all(fs.staging_root()).await.unwrap();
        let snapshot = UploadSnapshot {
            path: fs.staging_root().join("payload.snapshot"),
            size: 4,
            generation: 1,
            publication_guard: None,
        };
        tokio::fs::write(&snapshot.path, b"ours").await.unwrap();
        assert_eq!(fs.snapshot_published("key", &snapshot).await, !changed);
        assert!(snapshot.path.exists());
        let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
    }
}

#[tokio::test]
async fn an_external_version_change_retires_the_old_nfs_handle() {
    let changed = Arc::new(AtomicBool::new(false));
    let fixture = serve({
        let changed = changed.clone();
        move |request| {
            let changed = changed.clone();
            async move {
                let (etag, body) = if changed.load(Ordering::SeqCst) {
                    ("\"new\"", "new!")
                } else {
                    ("\"old\"", "old!")
                };
                if request.method == "HEAD" {
                    Response::empty(200)
                        .header("content-length", 4)
                        .header("etag", etag)
                } else {
                    Response::xml(206, body)
                        .header("content-range", "bytes 0-3/4")
                        .header("etag", etag)
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "read-version");
    let old = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 4, 0)
        .unwrap();
    assert_eq!(fs.read(old, 0, 4).await.unwrap().0, b"old!");
    changed.store(true, Ordering::SeqCst);
    fs.inner
        .read_identities
        .lock()
        .await
        .get_mut(&old)
        .unwrap()
        .observed_at = Instant::now() - DIR_CACHE_TTL;
    assert!(matches!(
        fs.read(old, 0, 4).await,
        Err(nfsstat3::NFS3ERR_STALE)
    ));
    let new = *fs.inner.inodes.read().unwrap().by_key.get("note").unwrap();
    assert_ne!(new, old);
    assert!(matches!(
        fs.getattr(old).await,
        Err(nfsstat3::NFS3ERR_STALE)
    ));
    assert_eq!(fs.read(new, 0, 4).await.unwrap().0, b"new!");
}

#[tokio::test]
async fn storage_health_recovers_after_successful_io() {
    let offline = Arc::new(AtomicBool::new(true));
    let fixture = serve({
        let offline = offline.clone();
        move |_| {
            let offline = offline.clone();
            async move {
                if offline.load(Ordering::SeqCst) {
                    Response::empty(403)
                } else {
                    Response::empty(200)
                        .header("content-length", 0)
                        .header("etag", "\"empty\"")
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "health");
    assert!(fs.object_head("key").await.is_err());
    assert!(fs.health_snapshot().await.last_error.is_some());
    offline.store(false, Ordering::SeqCst);
    assert!(fs.object_head("key").await.unwrap().is_some());
    let health = fs.health_snapshot().await;
    assert!(health.last_error.is_none());
    assert!(health.last_successful_io.is_some());
}

#[tokio::test]
async fn chunk_read_retries_a_transient_get_inside_the_same_identity() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let attempts = attempts.clone();
        move |request| {
            let attempts = attempts.clone();
            async move {
                if request.method == "HEAD" {
                    Response::empty(200)
                        .header("content-length", 4)
                        .header("etag", "\"retry\"")
                } else if request.method == "GET" {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Response::empty(503)
                    } else {
                        Response::xml(206, "data")
                            .header("content-range", "bytes 0-3/4")
                            .header("etag", "\"retry\"")
                    }
                } else {
                    Response::empty(400)
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "read-get-retry");
    let id = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 4, 0)
        .unwrap();

    assert_eq!(fs.read(id, 0, 4).await.unwrap().0, b"data");
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        2
    );
}

#[tokio::test]
async fn chunk_read_does_not_retry_authorization_failures() {
    let fixture = serve(|request| async move {
        if request.method == "HEAD" {
            Response::empty(200)
                .header("content-length", 4)
                .header("etag", "\"auth\"")
        } else if request.method == "GET" {
            Response::empty(403)
        } else {
            Response::empty(400)
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "read-get-auth");
    let id = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 4, 0)
        .unwrap();

    assert!(matches!(
        fs.read(id, 0, 4).await,
        Err(nfsstat3::NFS3ERR_ACCES)
    ));
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        1
    );
}

#[tokio::test]
async fn chunk_read_rejects_identity_header_mismatch_without_retry() {
    let fixture = serve(|request| async move {
        if request.method == "HEAD" {
            Response::empty(200)
                .header("content-length", 1)
                .header("etag", "\"short\"")
        } else if request.method == "GET" {
            Response {
                status: 206,
                headers: Vec::new(),
                body: b"x".to_vec(),
            }
            .header("content-range", "bytes 0-1/1")
            .header("content-length", "1")
            .header("etag", "\"short\"")
        } else {
            Response::empty(400)
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "read-get-header-mismatch");
    let id = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 1, 0)
        .unwrap();

    assert!(matches!(fs.read(id, 0, 1).await, Err(nfsstat3::NFS3ERR_IO)));
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        1
    );
}

#[tokio::test]
async fn stage_prime_retries_interrupted_body_without_applying_partial_bytes() {
    let gets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let gets = gets.clone();
        move |request| {
            let gets = gets.clone();
            async move {
                if request.method == "HEAD" {
                    return Response::empty(200)
                        .header("content-length", 4)
                        .header("etag", "\"stage\"");
                }
                if request.method == "GET" {
                    if gets.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Response {
                            status: 200,
                            headers: vec![
                                ("content-length".into(), "4".into()),
                                ("etag".into(), "\"stage\"".into()),
                            ],
                            body: b"da".to_vec(),
                        };
                    }
                    return Response::xml(200, "data").header("etag", "\"stage\"");
                }
                Response::empty(400)
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "stage-get-body-retry");
    let id = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 4, 0)
        .unwrap();

    fs.write(id, 1, b"X").await.unwrap();
    assert_eq!(fs.read(id, 0, 4).await.unwrap().0, b"dXta");
    assert_eq!(gets.load(Ordering::SeqCst), 2);
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn stage_prime_does_not_retry_authorization_failures() {
    let fixture = serve(|request| async move {
        if request.method == "HEAD" {
            Response::empty(200)
                .header("content-length", 4)
                .header("etag", "\"stage-auth\"")
        } else if request.method == "GET" {
            Response::empty(403)
        } else {
            Response::empty(400)
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "stage-get-auth");
    let id = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 4, 0)
        .unwrap();

    assert!(matches!(
        fs.write(id, 1, b"X").await,
        Err(nfsstat3::NFS3ERR_ACCES)
    ));
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        1
    );
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn relay_rename_upload_part_retries_transient_upload_and_honors_cancel_before_get() {
    const PART: u64 = 5 * 1024 * 1024;
    let puts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let puts = puts.clone();
        move |request| {
            let puts = puts.clone();
            async move {
                if request.method == "GET" {
                    return Response {
                        status: 206,
                        headers: vec![
                            ("content-length".into(), PART.to_string()),
                            (
                                "content-range".into(),
                                format!("bytes 0-{}/{}", PART - 1, PART),
                            ),
                            ("etag".into(), "\"relay-source\"".into()),
                        ],
                        body: vec![b'r'; PART as usize],
                    };
                }
                if request.method == "PUT" {
                    if puts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Response::xml(
                            503,
                            "<Error><Code>ServiceUnavailable</Code></Error>",
                        );
                    }
                    // S3 returns a part's ETag as a response header, not a body.
                    return Response::empty(200).header("etag", "\"relay-part\"");
                }
                Response::empty(400)
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "relay-upload-part-retry");
    let object = RenameObject {
        from: "source".into(),
        to: "target".into(),
        source_etag: "\"relay-source\"".into(),
        size: PART,
        destination_etag: None,
        phase: "pending".into(),
        replaced_etag: None,
        source_version: None,
    };
    let plan = crate::move_transfer::stream::MultipartPlan::new(
        fs.inner.transfer_config.get().unwrap(),
        PART,
        Some(PART),
    )
    .unwrap();
    assert_eq!(
        fs.copy_rename_part(&object, "upload", "", false, 1, &plan)
            .await
            .unwrap(),
        (1, "\"relay-part\"".into())
    );
    assert_eq!(puts.load(Ordering::SeqCst), 2);

    let cancelled = filesystem(fixture.client.clone(), "relay-upload-part-cancel");
    cancelled.abort_storage_operations();
    assert!(cancelled
        .copy_rename_part(&object, "upload", "", false, 1, &plan)
        .await
        .is_err());
    let get_count = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.method == "GET")
        .count();
    assert_eq!(get_count, 1, "cancelled retry must not start another GET");
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
    let _ = tokio::fs::remove_dir_all(cancelled.staging_root()).await;
}

#[tokio::test]
async fn namespace_recovery_reconciles_a_committed_put_after_a_lost_response() {
    let committed = Arc::new(std::sync::Mutex::new(None::<String>));
    let fixture = serve({
        let committed = committed.clone();
        move |request| {
            let committed = committed.clone();
            async move {
                if request.method == "HEAD" {
                    return match committed.lock().unwrap().clone() {
                        Some(token) => Response::empty(200)
                            .header("content-length", 0)
                            .header("etag", "\"empty\"")
                            .header("x-amz-meta-r2-namespace-operation", token),
                        None => Response::empty(404),
                    };
                }
                if request.method == "PUT" {
                    *committed.lock().unwrap() = request
                        .headers
                        .get("x-amz-meta-r2-namespace-operation")
                        .cloned();
                    return Response::xml(500, "<Error><Code>InternalError</Code></Error>");
                }
                Response::empty(400)
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "namespace-response-loss");
    tokio::fs::create_dir_all(fs.staging_root()).await.unwrap();
    assert!(fs.put_empty_object("new", false).await.is_err());
    assert_eq!(
        fs.pending_upload_count().await,
        1,
        "namespace-only recovery prevents cleanup"
    );
    assert_eq!(fs.drain(1, 1).await, 1);
    fs.resume_namespace_operations().await.unwrap();
    assert_eq!(fs.pending_upload_count().await, 0);
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn nfs_directory_listing_rejects_nonadjacent_cursor_cycles() {
    let pages = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=serve({let pages=pages.clone();move |_|{let pages=pages.clone();async move {
        let index=pages.fetch_add(1,Ordering::SeqCst); let token=if index.is_multiple_of(2) {"a"} else {"b"};
        Response::xml(200,&format!("<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>{token}</NextContinuationToken><Contents><Key>file{index}</Key><Size>1</Size></Contents></ListBucketResult>"))
    }}}).await;
    let fs = filesystem(fixture.client.clone(), "cursor-cycle");
    assert!(
        tokio::time::timeout(Duration::from_secs(3), fs.children_of(ROOT_ID, ""))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(pages.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn large_native_rename_uses_conditional_server_parts_without_downloads() {
    let copies = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=serve({let copies=copies.clone();move |request|{let copies=copies.clone();async move {
        if request.method=="HEAD" {return Response::empty(200).header("content-length",6*1024u64.pow(3)).header("etag","\"source\"");}
        if request.method=="GET" {return Response::xml(200,"<ListPartsResult><IsTruncated>false</IsTruncated></ListPartsResult>");}
        if request.method=="POST" && request.path.contains("uploads") {return Response::xml(200,"<InitiateMultipartUploadResult><UploadId>large-upload</UploadId></InitiateMultipartUploadResult>");}
        if request.method=="PUT" {assert_eq!(request.headers.get("x-amz-copy-source-if-match").map(String::as_str),Some("\"source\""));copies.fetch_add(1,Ordering::SeqCst);return Response::xml(200,"<CopyPartResult><ETag>&quot;part&quot;</ETag></CopyPartResult>");}
        assert_eq!(request.headers.get("if-none-match").map(String::as_str),Some("*"));
        Response::xml(200,"<CompleteMultipartUploadResult><ETag>&quot;large-result&quot;</ETag></CompleteMultipartUploadResult>")
    }}}).await;
    let fs = filesystem(fixture.client.clone(), "large-copy");
    tokio::fs::create_dir_all(fs.staging_root()).await.unwrap();
    let object = RenameObject {
        from: "large-source".into(),
        to: "large-destination".into(),
        source_etag: "\"source\"".into(),
        size: 6 * 1024u64.pow(3),
        destination_etag: None,
        phase: "pending".into(),
        replaced_etag: None,
        source_version: None,
    };
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(30),
            fs.copy_large_object(&object, "large-operation")
        )
        .await
        .unwrap()
        .unwrap(),
        "\"large-result\""
    );
    assert_eq!(copies.load(Ordering::SeqCst), 308);
    assert!(!fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.method == "GET" && r.headers.contains_key("range")));
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[test]
#[ignore = "child process driver invoked only by acknowledged_writes_survive_process_death"]
fn crash_child_driver() {
    let root = PathBuf::from(std::env::var_os("R2_AUDIT_CRASH_ROOT").expect("isolated crash root"));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = crate::providers::s3_client::create_s3_client(
            &crate::providers::s3_client::S3ClientConfig {
                access_key_id: "fixture",
                secret_access_key: "fixture",
                region: "us-east-1",
                endpoint_url: Some("http://127.0.0.1:1"),
                force_path_style: true,
            },
        )
        .unwrap();
        let fs = S3NfsFs::new(client, "photos".into(), false, root.clone());
        super::super::recovery::save_mount_manifest(
            &root,
            super::super::manager::MountProvider::Aws,
            "crash-account",
            "photos",
            "crash-scope",
        )
        .unwrap();
        let id = intern(&fs, "folder/持久写入.txt").await;
        fs.write(id, 0, b"acknowledged durable contents")
            .await
            .unwrap();
        tokio::fs::write(root.join("acknowledged"), b"ready")
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
}

#[tokio::test]
async fn acknowledged_writes_survive_process_death() {
    let root = std::env::temp_dir().join(format!(
        "r2-process-crash-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    tokio::fs::create_dir_all(&root).await.unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "mount::nfs_fs::protocol_tests::crash_child_driver",
            "--nocapture",
        ])
        .env("R2_AUDIT_CRASH_ROOT", &root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(15), async {
        while !root.join("acknowledged").exists() {
            if child.try_wait().unwrap().is_some() {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        true
    })
    .await
    .unwrap_or(false);
    let _ = child.kill();
    let _ = child.wait();
    assert!(ready, "crash child did not acknowledge its write");
    super::super::recovery::validate_identity(
        &root,
        super::super::manager::MountProvider::Aws,
        "crash-account",
        "photos",
        "crash-scope",
    )
    .unwrap();
    let fixture = serve(|_| async { Response::empty(500) }).await;
    let fs = S3NfsFs::new(fixture.client.clone(), "photos".into(), false, root.clone());
    assert_eq!(fs.restore_stages().await.unwrap(), 1);
    let id = *fs
        .inner
        .inodes
        .read()
        .unwrap()
        .by_key
        .get("folder/持久写入.txt")
        .unwrap();
    assert_eq!(
        fs.read(id, 0, 100).await.unwrap().0,
        b"acknowledged durable contents"
    );
    assert!(
        fixture.requests.lock().unwrap().is_empty(),
        "local recovery must not guess a remote destination"
    );
    drop(fs);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn delete_waits_for_the_actual_in_flight_put() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let exists = Arc::new(AtomicBool::new(false));
    let fixture = serve({
        let started = started.clone();
        let release = release.clone();
        let exists = exists.clone();
        move |request| {
            let started = started.clone();
            let release = release.clone();
            let exists = exists.clone();
            async move {
                match request.method.as_str() {
                    "PUT" => {
                        started.notify_one();
                        release.notified().await;
                        exists.store(true, Ordering::SeqCst);
                        Response::empty(200).header("etag", "\"written\"")
                    }
                    "DELETE" => {
                        exists.store(false, Ordering::SeqCst);
                        Response::empty(204)
                    }
                    "HEAD" if exists.load(Ordering::SeqCst) => Response::empty(200)
                        .header("etag", "\"written\"")
                        .header("content-length", 13),
                    _ => Response::empty(404),
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "delete-put");
    let id = intern(&fs, "note").await;
    fs.write(id, 0, b"durable bytes").await.unwrap();
    let flush = tokio::spawn({
        let fs = fs.clone();
        async move { fs.drain(1, 1).await }
    });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    let removal = tokio::spawn({
        let fs = fs.clone();
        async move {
            let name: filename3 = b"note".as_slice().into();
            fs.remove(ROOT_ID, &name).await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!removal.is_finished());
    assert!(!fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.method == "DELETE"));
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), flush)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), removal)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!exists.load(Ordering::SeqCst));
    assert_eq!(fs.pending_upload_count().await, 0);
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn a_drain_timeout_keeps_its_publisher_owned_until_it_settles() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let exists = Arc::new(AtomicBool::new(false));
    let fixture = serve({
        let started = started.clone();
        let release = release.clone();
        let exists = exists.clone();
        move |request| {
            let started = started.clone();
            let release = release.clone();
            let exists = exists.clone();
            async move {
                match request.method.as_str() {
                    "PUT" => {
                        started.notify_one();
                        release.notified().await;
                        exists.store(true, Ordering::SeqCst);
                        Response::empty(200).header("etag", "\"written\"")
                    }
                    "HEAD" if exists.load(Ordering::SeqCst) => Response::empty(200)
                        .header("content-length", 4)
                        .header("etag", "\"written\""),
                    "DELETE" => {
                        exists.store(false, Ordering::SeqCst);
                        Response::empty(204)
                    }
                    _ => Response::empty(404),
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "drain-timeout");
    let id = intern(&fs, "note").await;
    fs.write(id, 0, b"data").await.unwrap();
    let waiter = tokio::spawn({
        let fs = fs.clone();
        async move { tokio::time::timeout(Duration::from_millis(100), fs.drain(1, 1)).await }
    });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    assert!(waiter.await.unwrap().is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), fs.wait_for_flushes())
            .await
            .is_err()
    );
    let removal = tokio::spawn({
        let fs = fs.clone();
        async move {
            let name: filename3 = b"note".as_slice().into();
            fs.remove(ROOT_ID, &name).await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!removal.is_finished());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), removal)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!exists.load(Ordering::SeqCst));
    assert_eq!(fs.pending_upload_count().await, 0);
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn quota_rejection_does_not_acknowledge_or_replace_staged_bytes() {
    let fixture = serve(|_| async { Response::empty(404) }).await;
    let fs = filesystem(fixture.client.clone(), "quota");
    let id = intern(&fs, "note").await;
    fs.configure_quota(8_210).await;
    fs.write(id, 0, b"four").await.unwrap();
    assert!(matches!(
        fs.write(id, 0, b"too long").await,
        Err(nfsstat3::NFS3ERR_NOSPC)
    ));
    assert_eq!(fs.read(id, 0, 100).await.unwrap().0, b"four");
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn repeated_drain_rounds_do_not_retry_an_auth_failure() {
    let puts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let puts = puts.clone();
        move |request| {
            let puts = puts.clone();
            async move {
                if request.method == "PUT" {
                    puts.fetch_add(1, Ordering::SeqCst);
                }
                Response::xml(403, "<Error><Code>AccessDenied</Code></Error>")
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "auth-budget");
    let id = intern(&fs, "note").await;
    fs.write(id, 0, b"data").await.unwrap();
    assert_eq!(fs.drain(3, 3).await, 1);
    assert_eq!(fs.drain(3, 3).await, 1);
    assert_eq!(puts.load(Ordering::SeqCst), 1);
    assert!(fs.health_snapshot().await.last_error.is_some());
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn rename_failure_before_copy_does_not_freeze_the_destination_stage() {
    let fixture = serve(|request| async move {
        if request.method == "PUT" && request.path.starts_with("/photos/a") {
            Response::xml(403, "<Error><Code>AccessDenied</Code></Error>")
        } else if request.method == "PUT" {
            Response::empty(200).header("etag", "\"b\"")
        } else {
            Response::empty(404)
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "rename-before-copy");
    let a = intern(&fs, "a").await;
    let b = intern(&fs, "b").await;
    fs.inner.dirs.write().unwrap().insert(
        ROOT_ID,
        DirListing::complete(Arc::new(vec![
            DirChild {
                fileid: a,
                name: "a".into(),
            },
            DirChild {
                fileid: b,
                name: "b".into(),
            },
        ])),
    );
    fs.write(a, 0, b"AAAA").await.unwrap();
    fs.write(b, 0, b"BBBB").await.unwrap();
    let a_name: filename3 = b"a".as_slice().into();
    let b_name: filename3 = b"b".as_slice().into();
    assert!(fs.rename(ROOT_ID, &a_name, ROOT_ID, &b_name).await.is_err());
    assert!(!tokio::fs::try_exists(fs.rename_journal_path("a", "b"))
        .await
        .unwrap());
    assert_ne!(fs.stage_guard(b).await.unwrap().state, FlushState::Paused);
    assert_eq!(fs.read(b, 0, 100).await.unwrap().0, b"BBBB");
    {
        let _namespace = fs.inner.namespace.write().await;
        fs.flush_stage_blocking(b).await.unwrap();
    }
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn uncertain_put_recovery_replays_only_with_its_original_condition() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let attempts = attempts.clone();
        move |request| {
            let attempts = attempts.clone();
            async move {
                if request.method == "PUT" {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        return Response::xml(500, "<Error><Code>InternalError</Code></Error>");
                    }
                    if request.headers.get("if-none-match").map(String::as_str) != Some("*") {
                        return Response::xml(400, "<Error><Code>InvalidRequest</Code></Error>");
                    }
                    return Response::empty(200).header("etag", "\"committed\"");
                }
                Response::empty(404)
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "unknown-put-retry");
    let id = intern(&fs, "note").await;
    fs.write(id, 0, b"data").await.unwrap();
    let root = fs.staging_root().to_path_buf();
    let config = fs.inner.transfer_config.get().unwrap().clone();
    assert_eq!(fs.drain(1, 1).await, 1);
    drop(fs);
    let restored = S3NfsFs::new(fixture.client.clone(), "photos".into(), false, root.clone());
    restored.configure_transfer(config);
    restored.restore_stages().await.unwrap();
    assert_eq!(
        restored.drain(1, 1).await,
        0,
        "an uncertain PUT must not be a permanent latch"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.method == "PUT")
        .all(|r| r.headers.get("if-none-match").map(String::as_str) == Some("*")));
    drop(restored);
    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn transient_capability_failure_schedules_recovery_without_remounting() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let attempts = attempts.clone();
        move |request| {
            let attempts = attempts.clone();
            async move {
                if request.path.contains(".r2-operation-checks/") {
                    if request.method == "PUT"
                        && !request.headers.contains_key("if-none-match")
                        && attempts.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        return Response::xml(503, "<Error><Code>SlowDown</Code></Error>");
                    }
                    if request.headers.contains_key("if-none-match") {
                        return Response::xml(
                            412,
                            "<Error><Code>PreconditionFailed</Code></Error>",
                        );
                    }
                    return Response::empty(if request.method == "DELETE" { 204 } else { 200 })
                        .header("etag", "\"probe\"")
                        .header(
                            "content-length",
                            if request.method == "HEAD" { 8 } else { 0 },
                        );
                }
                if request.method == "PUT" {
                    Response::empty(200).header("etag", "\"saved\"")
                } else {
                    Response::empty(404)
                }
            }
        }
    })
    .await;
    let native = filesystem(fixture.client.clone(), "probe-recovery");
    let fs = S3NfsFs::new(
        fixture.client.clone(),
        "photos".into(),
        false,
        native.staging_root().into(),
    );
    fs.configure_transfer(crate::move_transfer::config::MoveConfig::Minio(
        crate::providers::minio::MinioConfig {
            bucket: "photos".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: fixture.endpoint.trim_start_matches("http://").into(),
            force_path_style: true,
        },
    ));
    let id = intern(&fs, "note").await;
    fs.write(id, 0, b"data").await.unwrap();
    assert_eq!(fs.drain(1, 1).await, 1);
    {
        let mut stage = fs.stage_guard(id).await.unwrap();
        assert!(
            matches!(stage.state, FlushState::Failed { .. }),
            "temporary capability failure must remain retryable"
        );
        stage.state = FlushState::Failed {
            retry_after: Instant::now(),
        };
    }
    assert_eq!(fs.drain(1, 1).await, 0);
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn unsupported_rename_is_rejected_before_it_can_freeze_staged_files() {
    let fixture = serve(|request| async move {
        if request.path.contains(".r2-operation-checks/") {
            if request.headers.contains_key("x-amz-copy-source") {
                return Response::xml(
                    200,
                    "<CopyObjectResult><ETag>&quot;copied&quot;</ETag></CopyObjectResult>",
                );
            }
            if request.headers.contains_key("if-match")
                || request.headers.contains_key("if-none-match")
            {
                return Response::xml(412, "<Error><Code>PreconditionFailed</Code></Error>");
            }
            return Response::empty(if request.method == "DELETE" { 204 } else { 200 })
                .header("etag", "\"probe\"")
                .header(
                    "content-length",
                    if request.method == "HEAD" { 8 } else { 0 },
                );
        }
        if request.method == "HEAD" && request.path.split('?').next() == Some("/photos/a") {
            return Response::empty(200)
                .header("etag", "\"source\"")
                .header("content-length", 1);
        }
        if request.method == "PUT" {
            return Response::empty(200).header("etag", "\"source\"");
        }
        Response::empty(404)
    })
    .await;
    let native = filesystem(fixture.client.clone(), "unsupported-rename");
    let fs = S3NfsFs::new(
        fixture.client.clone(),
        "photos".into(),
        false,
        native.staging_root().into(),
    );
    fs.configure_transfer(crate::move_transfer::config::MoveConfig::Minio(
        crate::providers::minio::MinioConfig {
            bucket: "photos".into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            endpoint_scheme: "http".into(),
            endpoint_host: fixture.endpoint.trim_start_matches("http://").into(),
            force_path_style: true,
        },
    ));
    let a = intern(&fs, "a").await;
    let b = intern(&fs, "b").await;
    fs.inner.dirs.write().unwrap().insert(
        ROOT_ID,
        DirListing::complete(Arc::new(vec![
            DirChild {
                fileid: a,
                name: "a".into(),
            },
            DirChild {
                fileid: b,
                name: "b".into(),
            },
        ])),
    );
    fs.write(a, 0, b"A").await.unwrap();
    fs.write(b, 0, b"B").await.unwrap();
    let result = fs
        .rename(
            ROOT_ID,
            &b"a".as_slice().into(),
            ROOT_ID,
            &b"b".as_slice().into(),
        )
        .await;
    assert!(
        matches!(result, Err(nfsstat3::NFS3ERR_NOTSUPP)),
        "result={result:?}, requests={:?}",
        fixture.requests.lock().unwrap()
    );
    assert!(!fs.rename_journal_path("a", "b").exists());
    assert!(fs.ensure_rename_available("a").is_ok());
    assert!(fs.ensure_rename_available("b").is_ok());
    assert_ne!(fs.stage_guard(b).await.unwrap().state, FlushState::Paused);
    {
        let _namespace = fs.inner.namespace.write().await;
        fs.flush_stage_blocking(b).await.unwrap();
    }
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn uncertain_large_copy_completes_existing_parts_without_recopying() {
    let size = 6 * 1024u64.pow(3);
    let parts=(1..=308).map(|number|{let (_,length)=stage::part_range(size,number-1);format!("<Part><PartNumber>{number}</PartNumber><ETag>&quot;p{number}&quot;</ETag><Size>{length}</Size></Part>")}).collect::<String>();
    let fixture=serve(move |request|{let parts=parts.clone();async move {
        if request.method=="HEAD" && request.path.contains("destination") {return Response::empty(404);}
        if request.method=="HEAD" {return Response::empty(200).header("content-length",size).header("etag","\"source\"");}
        if request.method=="GET" {return Response::xml(200,&format!("<ListPartsResult><IsTruncated>false</IsTruncated>{parts}</ListPartsResult>"));}
        if request.method=="POST" && request.headers.get("if-none-match").map(String::as_str)==Some("*") {return Response::xml(200,"<CompleteMultipartUploadResult><ETag>&quot;done&quot;</ETag></CompleteMultipartUploadResult>");}
        Response::empty(500)
    }}).await;
    let fs = filesystem(fixture.client.clone(), "large-complete-recovery");
    tokio::fs::create_dir_all(fs.staging_root()).await.unwrap();
    let object = RenameObject {
        from: "source".into(),
        to: "destination".into(),
        source_etag: "\"source\"".into(),
        size,
        destination_etag: None,
        phase: "copying".into(),
        replaced_etag: None,
        source_version: None,
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    object.to.hash(&mut hash);
    let path = fs
        .staging_root()
        .join(format!("rename-part-recovery-{:016x}.json", hash.finish()));
    let part_size = stage::planned_part_size(size);
    let journal = stage::MultipartJournal {
        upload_id: Some("already-uploaded".into()),
        part_size,
        parts: (1..=308)
            .map(|number| (number, format!("\"p{number}\"")))
            .collect(),
        completing: true,
        precondition: Some(stage::PublicationGuard::Absent),
        ..Default::default()
    };
    stage::write_json_atomic(&path, &journal).await.unwrap();
    assert_eq!(
        fs.copy_large_object(&object, "recovery").await.unwrap(),
        "\"done\""
    );
    assert!(!fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.method == "PUT"));
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn recovery_of_healthy_writes_is_not_blocked_by_an_unrelated_bad_manifest() {
    let fixture = serve(|request| async move {
        if request.method == "PUT" {
            Response::empty(200).header("etag", "\"saved\"")
        } else {
            Response::empty(404)
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "partial-stage-recovery");
    let id = intern(&fs, "healthy").await;
    fs.write(id, 0, b"data").await.unwrap();
    let root = fs.staging_root().to_path_buf();
    let config = fs.inner.transfer_config.get().unwrap().clone();
    drop(fs);
    tokio::fs::write(root.join("broken.stage.json"), b"{")
        .await
        .unwrap();
    let restored = S3NfsFs::new(fixture.client.clone(), "photos".into(), false, root.clone());
    restored.configure_transfer(config);
    assert_eq!(restored.restore_stages().await.unwrap(), 1);
    assert_eq!(
        restored.drain(1, 1).await,
        1,
        "the bad record remains visible while the healthy write uploads"
    );
    assert!(restored.health_snapshot().await.last_error.is_some());
    assert!(root.join("broken.stage.json").exists());
    assert_eq!(fixture.requests.lock().unwrap().iter().filter(|request|request.method=="PUT" && request.path.starts_with("/photos/healthy")).count(),1);
    drop(restored);
    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn an_unreadable_write_intent_quarantines_only_its_own_stage() {
    for malformed in [true, false] {
        let fixture = serve(|request| async move {
            if request.method == "PUT" {
                Response::empty(200).header("etag", "\"saved\"")
            } else {
                Response::empty(404)
            }
        })
        .await;
        let fs = filesystem(fixture.client.clone(), "partial-write-recovery");
        let good = intern(&fs, "healthy").await;
        let bad = intern(&fs, "interrupted").await;
        fs.write(good, 0, b"good").await.unwrap();
        fs.write(bad, 0, b"data").await.unwrap();
        let root = fs.staging_root().to_path_buf();
        let config = fs.inner.transfer_config.get().unwrap().clone();
        let path = fs.stage_guard(bad).await.unwrap().path().to_path_buf();
        drop(fs);
        let wal = path.with_extension("write.json");
        if malformed {
            tokio::fs::write(&wal, b"{").await.unwrap();
        } else {
            let state: serde_json::Value = serde_json::from_slice(
                &tokio::fs::read(path.with_extension("stage.json"))
                    .await
                    .unwrap(),
            )
            .unwrap();
            // A valid JSON record whose write cannot be replayed; the on-disk
            // file still has the manifest's size, so a length check cannot help.
            stage::write_json_atomic(&wal, &serde_json::json!({"state":state,"change":{"Write":{"offset":u64::MAX,"data":[1]}}})).await.unwrap();
        }
        let restored = S3NfsFs::new(fixture.client.clone(), "photos".into(), false, root.clone());
        restored.configure_transfer(config);
        assert_eq!(restored.restore_stages().await.unwrap(), 1);
        assert_eq!(restored.drain(1, 1).await, 1);
        assert!(wal.exists());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"data");
        assert!(restored.ensure_rename_available("interrupted").is_err());
        let puts: Vec<_> = fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "PUT")
            .map(|r| r.path.clone())
            .collect();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].split('?').next(), Some("/photos/healthy"));
        drop(restored);
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}

#[tokio::test]
async fn multipart_recovery_can_retry_definite_failure_and_recreate_expired_upload() {
    for expired in [false, true] {
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fixture = serve({
            let completed = completed.clone();
            move |request| {
                let completed = completed.clone();
                async move {
                    if request.method == "HEAD" { return Response::empty(404); }
                    if request.method == "GET" {
                        if expired && completed.load(Ordering::SeqCst) > 0 {
                            return Response::xml(404, "<Error><Code>NoSuchUpload</Code></Error>");
                        }
                        return Response::xml(200, "<ListPartsResult><IsTruncated>false</IsTruncated><Part><PartNumber>1</PartNumber><ETag>&quot;part&quot;</ETag><Size>4</Size></Part></ListPartsResult>");
                    }
                    if request.method == "POST" && request.path.contains("uploads") {
                        return Response::xml(200, "<InitiateMultipartUploadResult><UploadId>new-upload</UploadId></InitiateMultipartUploadResult>");
                    }
                    if request.method == "POST" {
                        assert_eq!(request.headers.get("if-none-match").map(String::as_str), Some("*"));
                        if completed.fetch_add(1, Ordering::SeqCst) == 0 {
                            return if expired { Response::xml(500,"<Error><Code>InternalError</Code></Error>") } else { Response::xml(403,"<Error><Code>AccessDenied</Code></Error>") };
                        }
                        return Response::xml(200, "<CompleteMultipartUploadResult><ETag>&quot;done&quot;</ETag></CompleteMultipartUploadResult>");
                    }
                    Response::empty(200).header("etag", "\"part\"")
                }
            }
        }).await;
        let fs = filesystem(fixture.client.clone(), "multipart-retry");
        let id = intern(&fs, "note").await;
        fs.write(id, 0, b"data").await.unwrap();
        let snapshot = fs
            .stage_guard(id)
            .await
            .unwrap()
            .upload_snapshot()
            .await
            .unwrap();
        let mut journal = snapshot.journal().await.unwrap();
        journal.upload_id = Some("old-upload".into());
        snapshot.save_journal(&journal).await.unwrap();
        assert!(fs
            .upload_stage_multipart("note", &snapshot, None, 1)
            .await
            .is_err());
        assert_eq!(snapshot.journal().await.unwrap().completing, expired);
        fs.upload_stage_multipart("note", &snapshot, None, 1)
            .await
            .unwrap();
        let journal = snapshot.journal().await.unwrap();
        assert_eq!(journal.published_etag.as_deref(), Some("\"done\""));
        assert_eq!(
            journal.upload_id.as_deref(),
            Some(if expired { "new-upload" } else { "old-upload" })
        );
        {
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(
                requests.iter().filter(|r| r.method == "PUT").count(),
                usize::from(expired)
            );
        }
        let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
    }
}

#[tokio::test]
async fn uncertain_snapshot_recovery_never_overwrites_a_foreign_target() {
    let fixture = serve(|_| async move {
        Response::empty(200)
            .header("etag", "\"foreign\"")
            .header("content-length", 4)
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "foreign-target-recovery");
    let id = intern(&fs, "note").await;
    fs.write(id, 0, b"data").await.unwrap();
    let snapshot = fs
        .stage_guard(id)
        .await
        .unwrap()
        .upload_snapshot()
        .await
        .unwrap();
    let mut journal = snapshot.journal().await.unwrap();
    journal.completing = true;
    snapshot.save_journal(&journal).await.unwrap();
    assert!(fs
        .upload_stage_file("note", &snapshot, None, 1)
        .await
        .is_err());
    assert!(snapshot.path.exists());
    assert!(fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.method == "HEAD"));
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn a_namespace_conflict_does_not_block_unrelated_recovered_uploads() {
    let fixture = serve(|request| async move {
        if request.method == "HEAD" && request.path.starts_with("/photos/conflict") {
            Response::empty(200)
                .header("content-length", 4)
                .header("etag", "\"changed\"")
        } else if request.method == "PUT" {
            Response::empty(200).header("etag", "\"saved\"")
        } else {
            Response::empty(404)
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "partial-namespace-recovery");
    let id = intern(&fs, "healthy").await;
    fs.write(id, 0, b"data").await.unwrap();
    let path = fs.namespace_journal_path("conflict");
    stage::write_json_atomic(&path,&serde_json::json!({"version":1,"key":"conflict","operation":"delete","token":"fixture","previous_etag":"\"old\""})).await.unwrap();
    let root = fs.staging_root().to_path_buf();
    let config = fs.inner.transfer_config.get().unwrap().clone();
    drop(fs);
    let restored = S3NfsFs::new(fixture.client.clone(), "photos".into(), false, root.clone());
    restored.configure_transfer(config);
    assert_eq!(restored.restore_stages().await.unwrap(), 1);
    assert_eq!(restored.drain(1, 1).await, 1);
    assert!(path.exists());
    assert!(!fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.method == "DELETE"));
    assert!(restored.health_snapshot().await.last_error.is_some());
    drop(restored);
    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn uploading_a_snapshot_does_not_mix_later_writes() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let fixture = serve({
        let started = started.clone();
        let release = release.clone();
        move |_| {
            let started = started.clone();
            let release = release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                Response::empty(200).header("etag", "\"original\"")
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "snapshot");
    let id = intern(&fs, "note").await;
    fs.write(id, 0, b"OLD-CONTENT").await.unwrap();
    let flush = tokio::spawn({
        let fs = fs.clone();
        async move { fs.drain(1, 1).await }
    });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    fs.write(id, 0, b"NEW-CONTENT").await.unwrap();
    release.notify_one();
    let pending = tokio::time::timeout(Duration::from_secs(3), flush)
        .await
        .unwrap()
        .unwrap();
    {
        let requests = fixture.requests.lock().unwrap();
        assert!(requests[0].body.windows(11).any(|b| b == b"OLD-CONTENT"));
        assert!(!requests[0].body.windows(11).any(|b| b == b"NEW-CONTENT"));
    }
    assert_eq!(
        pending, 1,
        "a newer durable generation must remain pending after the older snapshot commits"
    );
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn unordered_copy_failure_keeps_the_successful_key_in_its_journal() {
    let b_finished = Arc::new(tokio::sync::Notify::new());
    let fixture = serve({
        let b_finished = b_finished.clone();
        move |request| {
            let b_finished = b_finished.clone();
            async move {
                if request.method == "HEAD" {
                    return Response::empty(200)
                        .header("etag", "\"source\"")
                        .header("content-length", 1);
                }
                if request
                    .headers
                    .get("x-amz-copy-source")
                    .is_some_and(|s| s.ends_with("/A"))
                {
                    b_finished.notified().await;
                    return Response::xml(503, "<Error><Code>ServiceUnavailable</Code></Error>");
                }
                b_finished.notify_one();
                Response::xml(
                    200,
                    "<CopyObjectResult><ETag>&quot;copied-B&quot;</ETag></CopyObjectResult>",
                )
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "unordered");
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fs.rename_objects(
            "from/",
            "to/",
            vec![
                ("from/A".into(), "to/A".into()),
                ("from/B".into(), "to/B".into()),
            ],
        ),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    let journal: RenameJournal = serde_json::from_slice(
        &tokio::fs::read(fs.rename_journal_path("from/", "to/"))
            .await
            .unwrap(),
    )
    .unwrap();
    let b = journal.objects.iter().find(|o| o.from == "from/B").unwrap();
    assert_eq!(b.to, "to/B");
    assert_eq!(b.phase, "copied");
    assert_eq!(b.destination_etag.as_deref(), Some("\"copied-B\""));
    assert!(!fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.method == "DELETE"));
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

fn directory_response(keys: &[&str], prefixes: &[&str], next: Option<&str>) -> Response {
    let mut body = format!(
        "<ListBucketResult><IsTruncated>{}</IsTruncated>",
        next.is_some()
    );
    for key in keys {
        body.push_str(&format!(
            "<Contents><Key>{key}</Key><Size>1</Size></Contents>"
        ));
    }
    for prefix in prefixes {
        body.push_str(&format!(
            "<CommonPrefixes><Prefix>{prefix}</Prefix></CommonPrefixes>"
        ));
    }
    if let Some(token) = next {
        body.push_str(&format!(
            "<NextContinuationToken>{token}</NextContinuationToken>"
        ));
    }
    body.push_str("</ListBucketResult>");
    Response::xml(200, &body)
}

fn directory_names(page: &ReadDirResult) -> Vec<String> {
    page.entries
        .iter()
        .map(|entry| String::from_utf8(entry.name.as_ref().to_vec()).unwrap())
        .collect()
}

#[tokio::test]
async fn first_readdir_returns_before_the_next_provider_page_is_requested() {
    let second_entered = Arc::new(tokio::sync::Notify::new());
    let release_second = Arc::new(tokio::sync::Notify::new());
    let fixture = serve({
        let second_entered = second_entered.clone();
        let release_second = release_second.clone();
        move |request| {
            let second_entered = second_entered.clone();
            let release_second = release_second.clone();
            async move {
                if request.path.contains("continuation-token=next") {
                    second_entered.notify_one();
                    release_second.notified().await;
                    directory_response(&["gamma"], &[], None)
                } else {
                    directory_response(&["alpha", "beta"], &[], Some("next"))
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "incremental-first-page");
    let first = tokio::time::timeout(Duration::from_secs(2), fs.readdir(ROOT_ID, 0, 100))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(directory_names(&first), ["alpha"]);
    assert!(!first.end);
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    let second = tokio::spawn({
        let fs = fs.clone();
        let cookie = first.entries[0].fileid;
        async move { fs.readdir(ROOT_ID, cookie, 100).await }
    });
    second_entered.notified().await;
    assert!(!second.is_finished());
    release_second.notify_one();
    let last = second.await.unwrap().unwrap();
    assert_eq!(directory_names(&last), ["beta", "gamma"]);
    assert!(last.end);
}

#[tokio::test]
async fn incremental_readdir_cookies_cover_a_stable_directory_exactly_once() {
    let fixture = serve(|request| async move {
        if request.path.contains("continuation-token=p2") {
            directory_response(&["e", "f"], &[], None)
        } else if request.path.contains("continuation-token=p1") {
            directory_response(&["c", "d"], &[], Some("p2"))
        } else {
            directory_response(&["a", "b"], &[], Some("p1"))
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "incremental-cookies");
    let mut cookie = 0;
    let mut names = Vec::new();
    let mut ids = std::collections::HashSet::new();
    loop {
        // A zero entry limit still has to make progress.
        let page = fs.readdir(ROOT_ID, cookie, 0).await.unwrap();
        assert_eq!(page.entries.len(), 1);
        cookie = page.entries[0].fileid;
        assert!(ids.insert(cookie));
        names.extend(directory_names(&page));
        if page.end {
            break;
        }
    }
    assert_eq!(names, ["a", "b", "c", "d", "e", "f"]);
    assert_eq!(fixture.requests.lock().unwrap().len(), 3);
    let eof = fs.readdir(ROOT_ID, cookie, 1).await.unwrap();
    assert!(eof.end && eof.entries.is_empty());
    let warm = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert_eq!(directory_names(&warm), names);
    assert!(warm.end);
    assert_eq!(fixture.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_later_directory_prefix_wins_before_any_colliding_name_is_emitted() {
    let fixture = serve(|request| async move {
        if request.path.contains("continuation-token=p2") {
            directory_response(&["b"], &["a/"], None)
        } else if request.path.contains("continuation-token=p1") {
            directory_response(&["a#", "a."], &[], Some("p2"))
        } else {
            directory_response(&["0", "a", "a!"], &[], Some("p1"))
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "delimiter-collision");
    let first = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert_eq!(directory_names(&first), ["0"]);
    assert!(!first.end);
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    let rest = fs
        .readdir(ROOT_ID, first.entries[0].fileid, 100)
        .await
        .unwrap();
    assert_eq!(directory_names(&rest), ["a", "a!", "a#", "a.", "b"]);
    assert!(matches!(rest.entries[0].attr.ftype, ftype3::NF3DIR));
    assert!(rest.end);
    assert_eq!(fixture.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_directory_without_a_same_named_file_can_sort_before_prior_page_files() {
    let fixture = serve(|request| async move {
        if request.path.contains("continuation-token=next") {
            directory_response(&["b"], &["a/"], None)
        } else {
            directory_response(&["a!", "a#"], &[], Some("next"))
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "implicit-delimiter-collision");
    let page = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert_eq!(directory_names(&page), ["a", "a!", "a#", "b"]);
    assert!(page.end);
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        2,
        "the ambiguous first provider page needs lookahead"
    );
}

#[tokio::test]
async fn partial_directory_cache_does_not_establish_lookup_absence() {
    let fixture = serve(|request| async move {
        if request.path.contains("continuation-token=next") {
            directory_response(&["later"], &[], None)
        } else {
            directory_response(&["a", "b"], &[], Some("next"))
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "partial-lookup");
    let first = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert!(!first.end);
    let later = fs.lookup_child(ROOT_ID, "", "later").await.unwrap();
    assert!(later.is_some());
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert!(fs.readdir(ROOT_ID, 0, 100).await.unwrap().end);
}

#[tokio::test]
async fn lookup_of_published_partial_child_does_not_fetch_the_rest_of_the_directory() {
    let second_entered = Arc::new(tokio::sync::Notify::new());
    let release_second = Arc::new(tokio::sync::Notify::new());
    let fixture = serve({
        let second_entered = second_entered.clone();
        let release_second = release_second.clone();
        move |request| {
            let second_entered = second_entered.clone();
            let release_second = release_second.clone();
            async move {
                if request.path.contains("continuation-token=next") {
                    second_entered.notify_one();
                    release_second.notified().await;
                    directory_response(&["omega"], &[], None)
                } else {
                    directory_response(&["alpha", "beta"], &[], Some("next"))
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "partial-positive-lookup");
    let page = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert_eq!(directory_names(&page), ["alpha"]);
    assert!(!page.end);

    let alpha: filename3 = b"alpha".as_slice().into();
    let looked_up = tokio::time::timeout(Duration::from_millis(250), fs.lookup(ROOT_ID, &alpha))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(looked_up, page.entries[0].fileid);
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            !requests
                .iter()
                .any(|request| request.path.contains("continuation-token=next")),
            "exact lookup must not fetch unrelated directory pages"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), second_entered.notified())
            .await
            .is_err()
    );
    release_second.notify_waiters();
}

#[tokio::test]
async fn exact_lookup_probes_directory_before_colliding_file_and_singleflights() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let fixture = serve({
        let entered = entered.clone();
        let release = release.clone();
        move |request| {
            let entered = entered.clone();
            let release = release.clone();
            async move {
                if request.method == "GET" && request.path.contains("list-type") {
                    if request.path.contains("prefix=photos%2F") {
                        entered.notify_one();
                        release.acquire().await.unwrap().forget();
                        return directory_response(&["photos/inside.jpg"], &[], None);
                    }
                    directory_response(&["photos", "zeta"], &[], Some("next"))
                } else if request.method == "HEAD" {
                    Response::empty(200)
                        .header("content-length", 3)
                        .header("etag", "\"file\"")
                } else {
                    Response::empty(400)
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "exact-directory-singleflight");
    let first = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert!(!first.end);

    let a = tokio::spawn({
        let fs = fs.clone();
        async move { fs.lookup_child(ROOT_ID, "", "photos").await }
    });
    entered.notified().await;
    let b = tokio::spawn({
        let fs = fs.clone();
        async move { fs.lookup_child(ROOT_ID, "", "photos").await }
    });
    release.add_permits(1);

    let a = a.await.unwrap().unwrap().unwrap();
    let b = b.await.unwrap().unwrap().unwrap();
    assert_eq!(a.0, b.0);
    assert_eq!(a.1.kind, EntryKind::Dir);

    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.path.contains("prefix=photos%2F"))
            .count(),
        1
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.method == "HEAD" && request.path.ends_with("/photos")),
        "directory wins without also probing the colliding object"
    );
}

#[tokio::test]
async fn no_listing_rename_allows_concurrent_target_exact_lookup_after_fence_release() {
    let copy_entered = Arc::new(tokio::sync::Notify::new());
    let release_copy = Arc::new(tokio::sync::Notify::new());
    let copied = Arc::new(std::sync::Mutex::new(None::<String>));
    let removed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fixture = serve({
        let copy_entered = copy_entered.clone();
        let release_copy = release_copy.clone();
        let copied = copied.clone();
        let removed = removed.clone();
        move |request| {
            let copy_entered = copy_entered.clone();
            let release_copy = release_copy.clone();
            let copied = copied.clone();
            let removed = removed.clone();
            async move {
                if request.path.contains(".r2-operation-checks/") {
                    if request.method == "DELETE" && request.headers.contains_key("if-match") {
                        return Response::xml(
                            412,
                            "<Error><Code>PreconditionFailed</Code></Error>",
                        );
                    }
                    return Response::empty(if request.method == "DELETE" { 204 } else { 200 })
                        .header("etag", "\"probe\"")
                        .header(
                            "content-length",
                            if request.method == "HEAD" { 8 } else { 0 },
                        );
                }
                if request.method == "GET" && request.path.contains("list-type") {
                    return directory_response(&[], &[], None);
                }
                let key = request
                    .path
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .trim_start_matches("/photos/")
                    .to_string();
                match request.method.as_str() {
                    "HEAD" if key == "source" && !removed.load(Ordering::SeqCst) => {
                        Response::empty(200)
                            .header("content-length", 1)
                            .header("etag", "\"source\"")
                    }
                    "HEAD" if key == "target" => {
                        if let Some(token) = copied.lock().unwrap().clone() {
                            Response::empty(200)
                                .header("content-length", 1)
                                .header("etag", "\"copied\"")
                                .header("x-amz-meta-r2-rename-operation", token)
                        } else {
                            Response::empty(404)
                        }
                    }
                    "HEAD" => Response::empty(404),
                    "PUT"
                        if key == "target" && request.headers.contains_key("x-amz-copy-source") =>
                    {
                        copy_entered.notify_one();
                        release_copy.notified().await;
                        *copied.lock().unwrap() = request
                            .headers
                            .get("x-amz-meta-r2-rename-operation")
                            .cloned();
                        Response::xml(
                            200,
                            "<CopyObjectResult><ETag>&quot;copied&quot;</ETag></CopyObjectResult>",
                        )
                    }
                    "DELETE" if key == "source" => {
                        removed.store(true, Ordering::SeqCst);
                        Response::empty(204)
                    }
                    _ => Response::empty(400),
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "no-listing-rename-lookup");
    let source_name: filename3 = b"source".as_slice().into();
    let target_name: filename3 = b"target".as_slice().into();
    let source_id = fs.lookup(ROOT_ID, &source_name).await.unwrap();
    assert_eq!(fs.inode(source_id).unwrap().key, "source");

    let rename = tokio::spawn({
        let fs = fs.clone();
        let source_name = source_name.clone();
        let target_name = target_name.clone();
        async move {
            fs.rename(ROOT_ID, &source_name, ROOT_ID, &target_name)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), copy_entered.notified())
        .await
        .expect("rename should reach the slow copy without deadlocking on its own exact lookup");

    let target_lookup = tokio::spawn({
        let fs = fs.clone();
        let target_name = target_name.clone();
        async move { fs.lookup(ROOT_ID, &target_name).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), async {
            while !target_lookup.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err(),
        "target exact lookup should wait on the rename fence while copy is in flight"
    );
    release_copy.notify_one();

    let rename_result = tokio::time::timeout(Duration::from_secs(2), rename)
        .await
        .unwrap()
        .unwrap();
    if let Err(status) = rename_result {
        eprintln!(
            "rename failed with {status:?}; requests: {:?}",
            fixture.requests.lock().unwrap()
        );
        panic!("rename failed");
    }
    let target_id = tokio::time::timeout(Duration::from_secs(2), target_lookup)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(target_id, source_id);
    assert_eq!(fs.inode(target_id).unwrap().key, "target");
    assert!(fixture.requests.lock().unwrap().iter().any(|request| {
        request.method == "PUT"
            && request.path.starts_with("/photos/target")
            && request.headers.contains_key("x-amz-copy-source")
    }));
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn exact_lookup_negative_cache_is_generation_scoped() {
    let found = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fixture = serve({
        let found = found.clone();
        move |request| {
            let found = found.clone();
            async move {
                if request.method == "GET" && request.path.contains("list-type") {
                    if request.path.contains("prefix=missing%2F") {
                        return Response::xml(
                            200,
                            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
                        );
                    }
                    return directory_response(&[], &[], None);
                }
                if request.method == "HEAD" && request.path.starts_with("/photos/missing") {
                    if found.load(Ordering::SeqCst) {
                        Response::empty(200)
                            .header("content-length", 1)
                            .header("etag", "\"found\"")
                    } else {
                        Response::empty(404)
                    }
                } else {
                    Response::empty(400)
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "negative-generation");
    assert!(fs
        .lookup_child(ROOT_ID, "", "missing")
        .await
        .unwrap()
        .is_none());
    assert!(fs
        .lookup_child(ROOT_ID, "", "missing")
        .await
        .unwrap()
        .is_none());

    found.store(true, Ordering::SeqCst);
    fs.invalidate_dir(ROOT_ID);
    assert!(fs
        .lookup_child(ROOT_ID, "", "missing")
        .await
        .unwrap()
        .is_some());

    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.path.contains("prefix=missing%2F"))
            .count(),
        2,
        "the second miss should use the generation-bound negative cache before invalidation"
    );
}

#[tokio::test]
async fn concurrent_first_readdir_requests_share_one_provider_page() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let fixture = serve({
        let entered = entered.clone();
        let release = release.clone();
        move |_| {
            let entered = entered.clone();
            let release = release.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                directory_response(&["a", "b"], &[], Some("next"))
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "readdir-singleflight");
    let a = tokio::spawn({
        let fs = fs.clone();
        async move { fs.readdir(ROOT_ID, 0, 100).await }
    });
    entered.notified().await;
    let b = tokio::spawn({
        let fs = fs.clone();
        async move { fs.readdir(ROOT_ID, 0, 100).await }
    });
    release.notify_one();
    let a = a.await.unwrap().unwrap();
    let b = b.await.unwrap().unwrap();
    assert_eq!(directory_names(&a), ["a"]);
    assert_eq!(directory_names(&b), ["a"]);
    assert_eq!(a.entries[0].fileid, b.entries[0].fileid);
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn canceling_a_page_fetch_keeps_the_previous_cursor_for_retry() {
    let second_entered = Arc::new(tokio::sync::Notify::new());
    let release_second = Arc::new(tokio::sync::Notify::new());
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let entered = second_entered.clone();
        let release = release_second.clone();
        let attempts = attempts.clone();
        move |request| {
            let entered = entered.clone();
            let release = release.clone();
            let attempts = attempts.clone();
            async move {
                if request.path.contains("continuation-token=next") {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        entered.notify_one();
                        release.notified().await;
                    }
                    directory_response(&["c"], &[], None)
                } else {
                    directory_response(&["a", "b"], &[], Some("next"))
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "cancel-directory-fetch");
    let first = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    let cookie = first.entries[0].fileid;
    let pending = tokio::spawn({
        let fs = fs.clone();
        async move { fs.readdir(ROOT_ID, cookie, 100).await }
    });
    second_entered.notified().await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    release_second.notify_one();
    let resumed = fs.readdir(ROOT_ID, cookie, 100).await.unwrap();
    assert_eq!(directory_names(&resumed), ["b", "c"]);
    assert!(resumed.end);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path.contains("continuation-token=next"))
            .count(),
        2
    );
}

#[tokio::test]
async fn invalidation_rejects_the_inflight_generation_without_restarting_other_directories() {
    for invalidate_same in [true, false] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let fixture = serve({
            let entered = entered.clone();
            let release = release.clone();
            move |_| {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    directory_response(&["a", "b"], &[], Some("next"))
                }
            }
        })
        .await;
        let fs = filesystem(fixture.client.clone(), "directory-epoch");
        let other = fs
            .intern_child("other/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
            .unwrap();
        let pending = tokio::spawn({
            let fs = fs.clone();
            async move { fs.readdir(ROOT_ID, 0, 100).await }
        });
        entered.notified().await;
        fs.invalidate_dir(if invalidate_same { ROOT_ID } else { other });
        release.notify_one();
        let result = pending.await.unwrap();
        if invalidate_same {
            assert!(matches!(result, Err(nfsstat3::NFS3ERR_BAD_COOKIE)));
            assert!(!fs.inner.dirs.read().unwrap().contains_key(&ROOT_ID));
        } else {
            assert_eq!(directory_names(&result.unwrap()), ["a"]);
        }
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn stale_and_foreign_directory_cookies_are_rejected_explicitly() {
    let fixture = serve(|_| async { directory_response(&["a", "b"], &[], None) }).await;
    let fs = filesystem(fixture.client.clone(), "stale-directory-cookie");
    let first = fs.readdir(ROOT_ID, 0, 1).await.unwrap();
    let cookie = first.entries[0].fileid;
    let other = fs
        .intern_child("other/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();
    assert!(matches!(
        fs.readdir(other, cookie, 100).await,
        Err(nfsstat3::NFS3ERR_BAD_COOKIE)
    ));
    fs.invalidate_dir(ROOT_ID);
    assert!(matches!(
        fs.readdir(ROOT_ID, cookie, 100).await,
        Err(nfsstat3::NFS3ERR_BAD_COOKIE)
    ));
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    assert_eq!(
        directory_names(&fs.readdir(ROOT_ID, 0, 100).await.unwrap()),
        ["a", "b"]
    );
}

#[tokio::test]
async fn a_malformed_later_page_preserves_partial_rows_and_reports_an_error() {
    for malformed in ["missing", "repeated", "regressed"] {
        let fixture = serve(move |request| async move {
            if !request.path.contains("continuation-token=next") {
                return directory_response(&["a", "b"], &[], Some("next"));
            }
            match malformed {
                "missing" => Response::xml(200, "<ListBucketResult><IsTruncated>true</IsTruncated><Contents><Key>c</Key><Size>1</Size></Contents></ListBucketResult>"),
                "repeated" => directory_response(&["c"], &[], Some("next")),
                _ => directory_response(&["a"], &[], None),
            }
        }).await;
        let fs = filesystem(fixture.client.clone(), "malformed-directory-page");
        let first = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
        assert_eq!(directory_names(&first), ["a"]);
        assert!(matches!(
            fs.readdir(ROOT_ID, first.entries[0].fileid, 100).await,
            Err(nfsstat3::NFS3ERR_IO)
        ));
        let retained = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
        assert_eq!(directory_names(&retained), ["a"]);
        assert!(!retained.end);
        assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn complete_directory_lookup_yields_between_pages_to_a_waiting_readdir() {
    let first_entered = Arc::new(tokio::sync::Notify::new());
    let release_first = Arc::new(tokio::sync::Notify::new());
    let release_last = Arc::new(tokio::sync::Notify::new());
    let fixture = serve({
        let entered = first_entered.clone();
        let first = release_first.clone();
        let last = release_last.clone();
        move |request| {
            let entered = entered.clone();
            let first = first.clone();
            let last = last.clone();
            async move {
                if request.path.contains("continuation-token=next") {
                    last.notified().await;
                    directory_response(&["c"], &[], None)
                } else {
                    entered.notify_one();
                    first.notified().await;
                    directory_response(&["a", "b"], &[], Some("next"))
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "directory-page-fairness");
    let complete = tokio::spawn({
        let fs = fs.clone();
        async move { fs.children_of(ROOT_ID, "").await }
    });
    first_entered.notified().await;
    let readdir = tokio::spawn({
        let fs = fs.clone();
        async move { fs.readdir(ROOT_ID, 0, 100).await }
    });
    tokio::task::yield_now().await;
    release_first.notify_one();
    let page = tokio::time::timeout(Duration::from_secs(2), readdir)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(directory_names(&page), ["a"]);
    assert!(!page.end);
    assert!(!complete.is_finished());
    release_last.notify_one();
    assert_eq!(complete.await.unwrap().unwrap().len(), 3);
}

#[tokio::test]
async fn empty_s3_path_components_are_skipped_without_losing_the_provider_cursor() {
    for dir_key in ["", "a/"] {
        let fixture = serve(move |request| async move {
            if request.path.contains("continuation-token=next") {
                directory_response(&[&format!("{dir_key}b"), &format!("{dir_key}c")], &[], None)
            } else {
                directory_response(
                    &[&format!("{dir_key}!")],
                    &[&format!("{dir_key}/")],
                    Some("next"),
                )
            }
        })
        .await;
        let fs = filesystem(fixture.client.clone(), "empty-directory-component");
        let dirid = if dir_key.is_empty() {
            ROOT_ID
        } else {
            fs.intern_child(dir_key, ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
                .unwrap()
        };
        let first = fs.readdir(dirid, 0, 100).await.unwrap();
        assert_eq!(directory_names(&first), ["!"]);
        assert!(!first.end);
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
        let last = fs
            .readdir(dirid, first.entries[0].fileid, 100)
            .await
            .unwrap();
        assert_eq!(directory_names(&last), ["b", "c"]);
        assert!(last.end);
        assert!(fixture
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .path
            .contains("continuation-token=next"));
        assert_eq!(fs.children_of(dirid, dir_key).await.unwrap().len(), 3);
    }
}

#[tokio::test]
async fn a_skipped_empty_prefix_still_sets_the_raw_pagination_watermark() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = serve({
        let calls = calls.clone();
        move |_| {
            let calls = calls.clone();
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    directory_response(&[], &["/"], Some("next"))
                } else {
                    // This is not an empty directory: the provider has regressed
                    // behind the continuation despite returning no visible names.
                    directory_response(&[], &["/"], None)
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "empty-prefix-watermark");
    assert!(matches!(
        fs.readdir(ROOT_ID, 0, 100).await,
        Err(nfsstat3::NFS3ERR_IO)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn renaming_a_directory_preserves_an_unrelated_directorys_active_cookies() {
    let copy_entered = Arc::new(tokio::sync::Notify::new());
    let copy_permits = Arc::new(Semaphore::new(0));
    let copied = Arc::new(std::sync::Mutex::new(HashMap::<String, String>::new()));
    let removed = Arc::new(std::sync::Mutex::new(
        std::collections::HashSet::<String>::new(),
    ));
    let fixture = serve({
        let copy_entered = copy_entered.clone();
        let copy_permits = copy_permits.clone();
        let copied = copied.clone();
        let removed = removed.clone();
        move |request| {
            let copy_entered = copy_entered.clone();
            let copy_permits = copy_permits.clone();
            let copied = copied.clone();
            let removed = removed.clone();
            async move {
                let url = reqwest::Url::parse(&format!("http://fixture{}", request.path)).unwrap();
                let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
                let key = url
                    .path()
                    .strip_prefix("/photos/")
                    .unwrap_or_default()
                    .to_string();
                if request.method == "GET" && query.contains_key("list-type") {
                    return match query.get("prefix").map(String::as_str).unwrap_or_default() {
                        "" => directory_response(&[], &["A/", "B/"], None),
                        "A/" if query.contains_key("delimiter") => {
                            directory_response(&["A/x"], &["A/sub/"], None)
                        }
                        "A/" => directory_response(&["A/sub/y", "A/x"], &[], None),
                        "A/sub/" => directory_response(&["A/sub/y"], &[], None),
                        "B/" if query.contains_key("continuation-token") => {
                            directory_response(&["B/c"], &[], None)
                        }
                        "B/" => directory_response(&["B/a", "B/b"], &[], Some("b-next")),
                        _ => Response::empty(400),
                    };
                }
                match request.method.as_str() {
                    "HEAD" => {
                        if matches!(key.as_str(), "A/x" | "A/sub/y")
                            && !removed.lock().unwrap().contains(&key)
                        {
                            Response::empty(200)
                                .header("content-length", 1)
                                .header("etag", "\"source\"")
                        } else if matches!(key.as_str(), "B/a" | "B/b" | "B/c") {
                            Response::empty(200)
                                .header("content-length", 1)
                                .header("etag", "\"b-object\"")
                        } else if let Some(token) = copied.lock().unwrap().get(&key) {
                            Response::empty(200)
                                .header("content-length", 1)
                                .header("etag", "\"copied\"")
                                .header("x-amz-meta-r2-rename-operation", token)
                        } else {
                            Response::empty(404)
                        }
                    }
                    "PUT" if request.headers.contains_key("x-amz-copy-source") => {
                        copy_entered.notify_one();
                        copy_permits.acquire().await.unwrap().forget();
                        copied.lock().unwrap().insert(
                            key,
                            request
                                .headers
                                .get("x-amz-meta-r2-rename-operation")
                                .unwrap()
                                .clone(),
                        );
                        Response::xml(
                            200,
                            "<CopyObjectResult><ETag>&quot;copied&quot;</ETag></CopyObjectResult>",
                        )
                    }
                    "PUT" if key == "B/a" => Response::empty(200).header("etag", "\"b-stage\""),
                    "GET" if key == "B/a" => Response {
                        status: 206,
                        headers: Vec::new(),
                        body: b"b".to_vec(),
                    }
                    .header("content-range", "bytes 0-0/1")
                    .header("content-length", "1")
                    .header("etag", "\"b-object\""),
                    "DELETE" => {
                        assert_eq!(
                            request.headers.get("if-match").map(String::as_str),
                            Some("\"source\"")
                        );
                        removed.lock().unwrap().insert(key);
                        Response::empty(204)
                    }
                    _ => Response::empty(400),
                }
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "rename-directory-cookie-scope");
    let root = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    let a = root.entries[0].fileid;
    let b = root.entries[1].fileid;
    let a_page = fs.readdir(a, 0, 1).await.unwrap();
    let sub = a_page.entries[0].fileid;
    let sub_page = fs.readdir(sub, 0, 1).await.unwrap();
    let first_b = fs.readdir(b, 0, 100).await.unwrap();
    assert_eq!(directory_names(&first_b), ["a"]);
    let rename = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.rename(
                ROOT_ID,
                &b"A".as_slice().into(),
                ROOT_ID,
                &b"C".as_slice().into(),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), copy_entered.notified())
        .await
        .unwrap();
    let b_file = first_b.entries[0].fileid;
    let middle_b = fs.readdir(b, b_file, 1).await.unwrap();
    assert_eq!(directory_names(&middle_b), ["b"]);
    assert!(!middle_b.end);
    assert_eq!(fs.read(b_file, 0, 1).await.unwrap().0, b"b");
    fs.write(b_file, 0, b"Z").await.unwrap();
    let _pending = tokio::time::timeout(Duration::from_secs(3), fs.drain(1, 1))
        .await
        .expect("unrelated subtree flush should not wait for A rename");
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.method == "PUT" && request.path.starts_with("/photos/B/a")),
        "unrelated subtree writeback must be allowed while A rename is stalled"
    );
    let mut late_child = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.create(a, &b"late".as_slice().into(), sattr3::default())
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut late_child)
            .await
            .is_err(),
        "new children under A must wait behind A's rename prefix fence"
    );
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.path.contains("A/late")),
        "blocked late child must not publish before the rename fence releases"
    );
    late_child.abort();
    copy_permits.add_permits(2);
    tokio::time::timeout(Duration::from_secs(3), rename)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let last_b = fs
        .readdir(b, middle_b.entries[0].fileid, 100)
        .await
        .unwrap();
    assert_eq!(directory_names(&last_b), ["c"]);
    assert!(last_b.end);
    assert_eq!(fs.inode(a).unwrap().key, "C/");
    assert_eq!(fs.inode(sub).unwrap().key, "C/sub/");
    assert!(matches!(
        fs.readdir(a, sub, 100).await,
        Err(nfsstat3::NFS3ERR_BAD_COOKIE)
    ));
    assert!(matches!(
        fs.readdir(sub, sub_page.entries[0].fileid, 100).await,
        Err(nfsstat3::NFS3ERR_BAD_COOKIE)
    ));
    assert!(matches!(
        fs.readdir(ROOT_ID, a, 100).await,
        Err(nfsstat3::NFS3ERR_BAD_COOKIE)
    ));
    assert_eq!(removed.lock().unwrap().len(), 2);
    assert_eq!(copied.lock().unwrap().len(), 2);
    let b_requests = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.path.contains("prefix=B%2F"))
        .count();
    assert_eq!(
        b_requests, 2,
        "unrelated listing must not restart after rename"
    );
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn wait_for_mutations_waits_for_scoped_fence_participants() {
    let fixture = serve(|_| async { Response::empty(500) }).await;
    let fs = filesystem(fixture.client.clone(), "wait-scoped-fence");
    let held = fs.fence_exact_key("busy").await;
    let mut waiter = tokio::spawn({
        let fs = fs.clone();
        async move { fs.wait_for_mutations().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut waiter)
            .await
            .is_err(),
        "wait_for_mutations must wait for scoped fence participants outside the namespace lock"
    );
    drop(held);
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

#[tokio::test]
async fn pending_rename_record_blocks_source_and_target_prefix_operations() {
    let fixture = serve(|_| async { Response::empty(500) }).await;
    let fs = filesystem(fixture.client.clone(), "pending-rename-prefix");
    let source_dir = fs
        .intern_child("A/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();
    let source_file = fs
        .intern_child("A/file", source_dir, EntryKind::File, 1, 0)
        .unwrap();
    let target_file = fs
        .intern_child("C/file", ROOT_ID, EntryKind::File, 1, 0)
        .unwrap();
    let record = fs.rename_journal_path("A/", "C/");
    fs.inner
        .pending_renames
        .write()
        .unwrap()
        .insert(record, ("A/".into(), "C/".into()));

    assert!(matches!(
        fs.read(source_file, 0, 1).await,
        Err(nfsstat3::NFS3ERR_IO)
    ));
    assert!(matches!(
        fs.write(target_file, 0, b"x").await,
        Err(nfsstat3::NFS3ERR_IO)
    ));
    assert!(matches!(
        fs.create(source_dir, &b"late".as_slice().into(), sattr3::default())
            .await,
        Err(nfsstat3::NFS3ERR_IO)
    ));
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

/// Objects behind a fixture, for tests that assert on the namespace a race
/// leaves behind rather than on the requests it sent. Every condition the
/// mount relies on is enforced the way S3 enforces it, at the moment the
/// request is applied — after any delay a test adds in front of it.
#[derive(Default)]
struct ModelBucket {
    objects: BTreeMap<String, ModelObject>,
    versions: u64,
}

struct ModelObject {
    body: Vec<u8>,
    etag: String,
    metadata: Vec<(String, String)>,
}

impl ModelBucket {
    fn with(objects: &[(&str, &[u8])]) -> Arc<std::sync::Mutex<Self>> {
        let mut bucket = Self::default();
        for (key, body) in objects {
            bucket.store(key, body.to_vec(), Vec::new());
        }
        Arc::new(std::sync::Mutex::new(bucket))
    }

    fn store(&mut self, key: &str, body: Vec<u8>, metadata: Vec<(String, String)>) -> String {
        self.versions += 1;
        let etag = format!("\"model-{}\"", self.versions);
        self.objects.insert(
            key.to_string(),
            ModelObject {
                body,
                etag: etag.clone(),
                metadata,
            },
        );
        etag
    }

    fn keys(&self) -> Vec<String> {
        self.objects.keys().cloned().collect()
    }

    fn body(&self, key: &str) -> Option<&[u8]> {
        self.objects.get(key).map(|object| object.body.as_slice())
    }

    fn respond(&mut self, request: &Request) -> Response {
        let url = reqwest::Url::parse(&format!("http://fixture{}", request.path)).unwrap();
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let key = urlencoding::decode(
            url.path()
                .trim_start_matches("/photos")
                .trim_start_matches('/'),
        )
        .unwrap()
        .into_owned();
        let precondition_failed =
            || Response::xml(412, "<Error><Code>PreconditionFailed</Code></Error>");
        let current = self.objects.get(&key).map(|object| object.etag.clone());
        match request.method.as_str() {
            "GET" if query.contains_key("list-type") => self.list(&query),
            "HEAD" | "GET" => {
                let Some(object) = self.objects.get(&key) else {
                    return if request.method == "HEAD" {
                        Response::empty(404)
                    } else {
                        Response::xml(404, "<Error><Code>NoSuchKey</Code></Error>")
                    };
                };
                if request
                    .headers
                    .get("if-match")
                    .is_some_and(|expected| *expected != object.etag)
                {
                    return precondition_failed();
                }
                let total = object.body.len();
                let range = request
                    .headers
                    .get("range")
                    .and_then(|range| range.strip_prefix("bytes="))
                    .and_then(|range| range.split_once('-'))
                    .map(|(start, end)| {
                        let start: usize = start.parse().unwrap();
                        let end = end.parse::<usize>().unwrap().min(total.max(1) - 1);
                        (start, end)
                    });
                let mut response = match (request.method.as_str(), range) {
                    ("HEAD", _) => Response::empty(200).header("content-length", total),
                    (_, Some((start, end))) => Response {
                        status: 206,
                        headers: Vec::new(),
                        body: object.body[start..=end].to_vec(),
                    }
                    .header("content-range", format!("bytes {start}-{end}/{total}")),
                    _ => Response {
                        status: 200,
                        headers: Vec::new(),
                        body: object.body.clone(),
                    },
                }
                .header("etag", &object.etag);
                for (name, value) in &object.metadata {
                    response = response.header(&format!("x-amz-meta-{name}"), value);
                }
                response
            }
            "PUT" if query.contains_key("uploadId") => Response::empty(400),
            "PUT" => {
                let copied = if let Some(source) = request.headers.get("x-amz-copy-source") {
                    let source = urlencoding::decode(
                        source.trim_start_matches('/').trim_start_matches("photos/"),
                    )
                    .unwrap()
                    .into_owned();
                    let Some(source) = self.objects.get(&source) else {
                        return Response::xml(404, "<Error><Code>NoSuchKey</Code></Error>");
                    };
                    if request
                        .headers
                        .get("x-amz-copy-source-if-match")
                        .is_some_and(|expected| *expected != source.etag)
                    {
                        return precondition_failed();
                    }
                    Some(source.body.clone())
                } else {
                    None
                };
                let exclusive = request
                    .headers
                    .get("if-none-match")
                    .is_some_and(|value| value == "*");
                if (exclusive && current.is_some())
                    || request
                        .headers
                        .get("if-match")
                        .is_some_and(|expected| current.as_ref() != Some(expected))
                {
                    return precondition_failed();
                }
                let is_copy = copied.is_some();
                let metadata = request
                    .headers
                    .iter()
                    .filter_map(|(name, value)| {
                        name.strip_prefix("x-amz-meta-")
                            .map(|name| (name.to_string(), value.clone()))
                    })
                    .collect();
                let etag = self.store(
                    &key,
                    copied.unwrap_or_else(|| aws_chunked_payload(request)),
                    metadata,
                );
                if is_copy {
                    Response::xml(
                        200,
                        &format!(
                            "<CopyObjectResult><ETag>{}</ETag></CopyObjectResult>",
                            etag.replace('"', "&quot;")
                        ),
                    )
                } else {
                    Response::empty(200).header("etag", etag)
                }
            }
            "DELETE" => {
                if request
                    .headers
                    .get("if-match")
                    .is_some_and(|expected| current.as_ref() != Some(expected))
                {
                    return precondition_failed();
                }
                self.objects.remove(&key);
                Response::empty(204)
            }
            _ => Response::empty(400),
        }
    }

    fn list(&self, query: &HashMap<String, String>) -> Response {
        let prefix = query.get("prefix").cloned().unwrap_or_default();
        let delimiter = query.get("delimiter");
        let max_keys = query
            .get("max-keys")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1000usize);
        let mut entries = BTreeMap::new();
        for (key, object) in self
            .objects
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
        {
            let common = delimiter.and_then(|delimiter| {
                key[prefix.len()..]
                    .find(delimiter.as_str())
                    .map(|index| key[..prefix.len() + index + delimiter.len()].to_string())
            });
            match common {
                Some(common) => entries.entry(common).or_insert(None),
                None => entries.entry(key.clone()).or_insert(Some(object)),
            };
        }
        let after = query.get("continuation-token");
        let page: Vec<_> = entries
            .iter()
            .filter(|(key, _)| after.is_none_or(|after| key.as_str() > after.as_str()))
            .take(max_keys + 1)
            .collect();
        let truncated = page.len() > max_keys;
        let page = &page[..page.len().min(max_keys)];
        let mut body = format!("<ListBucketResult><IsTruncated>{truncated}</IsTruncated>");
        for (key, object) in page {
            if let Some(object) = object {
                body.push_str(&format!(
                    "<Contents><Key>{key}</Key><Size>{}</Size><ETag>{}</ETag></Contents>",
                    object.body.len(),
                    object.etag.replace('"', "&quot;")
                ));
            }
        }
        for (key, object) in page {
            if object.is_none() {
                body.push_str(&format!(
                    "<CommonPrefixes><Prefix>{key}</Prefix></CommonPrefixes>"
                ));
            }
        }
        if truncated {
            if let Some((last, _)) = page.last() {
                body.push_str(&format!(
                    "<NextContinuationToken>{last}</NextContinuationToken>"
                ));
            }
        }
        body.push_str("</ListBucketResult>");
        Response::xml(200, &body)
    }
}

/// The bytes a PUT stores. The SDK may frame a streamed body with
/// `aws-chunked` content encoding and a trailing checksum.
fn aws_chunked_payload(request: &Request) -> Vec<u8> {
    let framed = request
        .headers
        .get("content-encoding")
        .is_some_and(|value| value.contains("aws-chunked"))
        || request.headers.contains_key("x-amz-decoded-content-length");
    if !framed {
        return request.body.clone();
    }
    let mut payload = Vec::new();
    let mut rest = request.body.as_slice();
    while let Some(end) = rest.windows(2).position(|pair| pair == b"\r\n") {
        let header = std::str::from_utf8(&rest[..end]).unwrap();
        let size = usize::from_str_radix(header.split(';').next().unwrap().trim(), 16).unwrap();
        rest = &rest[end + 2..];
        if size == 0 {
            break;
        }
        payload.extend_from_slice(&rest[..size]);
        rest = &rest[(size + 2).min(rest.len())..];
    }
    payload
}

/// A staged upload that is still in flight when a rename of its key (or onto
/// its key) is issued must settle before the rename copies or deletes
/// anything: a late PUT landing after the rename's DELETE would bring the
/// moved-away source back, and one landing after the copy would overwrite
/// the renamed content with the replaced file's bytes.
#[tokio::test]
async fn a_late_staged_put_cannot_resurrect_a_rename_source_or_its_replaced_target() {
    for replace_target in [false, true] {
        let bucket = if replace_target {
            ModelBucket::with(&[("src", b"renamed bytes")])
        } else {
            ModelBucket::with(&[])
        };
        let put_entered = Arc::new(tokio::sync::Notify::new());
        let release_put = Arc::new(tokio::sync::Notify::new());
        let held = Arc::new(AtomicBool::new(false));
        let fixture = serve({
            let bucket = bucket.clone();
            let put_entered = put_entered.clone();
            let release_put = release_put.clone();
            let held = held.clone();
            move |request| {
                let bucket = bucket.clone();
                let put_entered = put_entered.clone();
                let release_put = release_put.clone();
                let held = held.clone();
                async move {
                    // Only the publication of the staged bytes is delayed.
                    if request.headers.contains_key("x-amz-meta-r2-stage-snapshot")
                        && !held.swap(true, Ordering::SeqCst)
                    {
                        put_entered.notify_one();
                        release_put.notified().await;
                    }
                    bucket.lock().unwrap().respond(&request)
                }
            }
        })
        .await;
        let fs = filesystem(fixture.client.clone(), "late-put-rename");
        let staged_key = if replace_target { "dst" } else { "src" };
        let staged = intern(&fs, staged_key).await;
        fs.write(staged, 0, b"late staged bytes").await.unwrap();
        if replace_target {
            let source = fs
                .intern_child("src", ROOT_ID, EntryKind::File, 13, 0)
                .unwrap();
            fs.inner.dirs.write().unwrap().insert(
                ROOT_ID,
                DirListing::complete(Arc::new(vec![
                    DirChild {
                        fileid: staged,
                        name: "dst".into(),
                    },
                    DirChild {
                        fileid: source,
                        name: "src".into(),
                    },
                ])),
            );
        }

        let flush = tokio::spawn({
            let fs = fs.clone();
            async move { fs.drain(1, 1).await }
        });
        tokio::time::timeout(Duration::from_secs(3), put_entered.notified())
            .await
            .unwrap();
        let mut rename = tokio::spawn({
            let fs = fs.clone();
            async move {
                fs.rename(
                    ROOT_ID,
                    &b"src".as_slice().into(),
                    ROOT_ID,
                    &b"dst".as_slice().into(),
                )
                .await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut rename)
                .await
                .is_err(),
            "the rename must wait for the in-flight publication of {staged_key}"
        );
        assert!(!fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.headers.contains_key("x-amz-copy-source")));
        release_put.notify_one();
        tokio::time::timeout(Duration::from_secs(3), flush)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), rename)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        {
            let bucket = bucket.lock().unwrap();
            assert_eq!(bucket.keys(), ["dst"], "nothing may come back at src");
            let expected: &[u8] = if replace_target {
                b"renamed bytes"
            } else {
                b"late staged bytes"
            };
            assert_eq!(bucket.body("dst"), Some(expected));
        }
        assert_eq!(fs.pending_upload_count().await, 0);
        let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
    }
}

/// A fixture over `bucket` that announces every server-side copy on
/// `copy_entered` and holds it until `release_copy` grants it a permit.
async fn copy_gated_fixture(
    bucket: Arc<std::sync::Mutex<ModelBucket>>,
    copy_entered: Arc<tokio::sync::Notify>,
    release_copy: Arc<tokio::sync::Semaphore>,
) -> crate::test_s3::Fixture {
    serve(move |request| {
        let bucket = bucket.clone();
        let copy_entered = copy_entered.clone();
        let release_copy = release_copy.clone();
        async move {
            if request.headers.contains_key("x-amz-copy-source") {
                copy_entered.notify_one();
                release_copy.acquire().await.unwrap().forget();
            }
            bucket.lock().unwrap().respond(&request)
        }
    })
    .await
}

/// `rm d/x` resolved to the file's id while `mv d/x t/y` held the fence.
/// Once the rename finishes that id names `t/y`, so a REMOVE that acts on the
/// id instead of the name would delete the file the user just moved.
#[tokio::test]
async fn remove_racing_a_rename_never_deletes_the_renamed_file() {
    let bucket = ModelBucket::with(&[("d/x", b"payload"), ("t/", b"")]);
    let copy_entered = Arc::new(tokio::sync::Notify::new());
    let release_copy = Arc::new(tokio::sync::Semaphore::new(0));
    let fixture =
        copy_gated_fixture(bucket.clone(), copy_entered.clone(), release_copy.clone()).await;
    let fs = filesystem(fixture.client.clone(), "remove-racing-rename");
    let d = fs
        .intern_child("d/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();
    let t = fs
        .intern_child("t/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();
    // A complete listing lets REMOVE resolve `x` without waiting on anything.
    let listed = fs.readdir(d, 0, 100).await.unwrap();
    assert_eq!(directory_names(&listed), ["x"]);
    let x = listed.entries[0].fileid;

    let rename = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.rename(d, &b"x".as_slice().into(), t, &b"y".as_slice().into())
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), copy_entered.notified())
        .await
        .unwrap();
    let mut removal = tokio::spawn({
        let fs = fs.clone();
        async move { fs.remove(d, &b"x".as_slice().into()).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut removal)
            .await
            .is_err(),
        "REMOVE must wait for the rename that holds d/x"
    );
    release_copy.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), rename)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let removed = tokio::time::timeout(Duration::from_secs(3), removal)
        .await
        .unwrap()
        .unwrap();

    {
        let bucket = bucket.lock().unwrap();
        assert_eq!(
            bucket.keys(),
            ["t/", "t/y"],
            "the renamed file must survive"
        );
        assert_eq!(bucket.body("t/y"), Some(b"payload".as_slice()));
    }
    assert!(
        matches!(removed, Err(nfsstat3::NFS3ERR_NOENT)),
        "d/x no longer exists, so REMOVE must report it missing: {removed:?}"
    );
    assert_eq!(fs.inode(x).unwrap().key, "t/y");
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

/// A CREATE and a MKDIR inside A waited behind `mv A C` with keys built from
/// A's old path. Publishing those keys once the rename finished would bring
/// A back as a second directory next to C.
#[tokio::test]
async fn create_and_mkdir_inside_a_directory_being_renamed_land_in_its_new_path() {
    let bucket = ModelBucket::with(&[("A/x", b"x")]);
    let copy_entered = Arc::new(tokio::sync::Notify::new());
    let release_copy = Arc::new(tokio::sync::Semaphore::new(0));
    let fixture =
        copy_gated_fixture(bucket.clone(), copy_entered.clone(), release_copy.clone()).await;
    let fs = filesystem(fixture.client.clone(), "create-in-renamed-directory");
    let a = fs
        .intern_child("A/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();

    let rename = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.rename(
                ROOT_ID,
                &b"A".as_slice().into(),
                ROOT_ID,
                &b"C".as_slice().into(),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), copy_entered.notified())
        .await
        .unwrap();
    let mut created = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.create(a, &b"late".as_slice().into(), sattr3::default())
                .await
        }
    });
    let mut made = tokio::spawn({
        let fs = fs.clone();
        async move { fs.mkdir(a, &b"sub".as_slice().into()).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut created)
            .await
            .is_err()
    );
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut made)
        .await
        .is_err());
    release_copy.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), rename)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (created, _) = tokio::time::timeout(Duration::from_secs(3), created)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (made, _) = tokio::time::timeout(Duration::from_secs(3), made)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(
        bucket.lock().unwrap().keys(),
        ["C/late", "C/sub/", "C/x"],
        "nothing may be published under the old path A/"
    );
    assert_eq!(fs.inode(created).unwrap().key, "C/late");
    assert_eq!(fs.inode(made).unwrap().key, "C/sub/");
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

/// A WRITE that waited behind `mv A C` must hold its fence on the key it
/// actually writes, C/f, so a rename or delete of C/f waits for it, and its
/// bytes must be published there.
#[tokio::test]
async fn a_write_racing_a_directory_rename_is_fenced_on_the_renamed_key() {
    let bucket = ModelBucket::with(&[("A/f", b"old!")]);
    let copy_entered = Arc::new(tokio::sync::Notify::new());
    let release_copy = Arc::new(tokio::sync::Semaphore::new(0));
    let read_entered = Arc::new(tokio::sync::Notify::new());
    let release_read = Arc::new(tokio::sync::Semaphore::new(0));
    let read_held = Arc::new(AtomicBool::new(false));
    let fixture = serve({
        let bucket = bucket.clone();
        let copy_entered = copy_entered.clone();
        let release_copy = release_copy.clone();
        let read_entered = read_entered.clone();
        let release_read = release_read.clone();
        let read_held = read_held.clone();
        move |request| {
            let bucket = bucket.clone();
            let copy_entered = copy_entered.clone();
            let release_copy = release_copy.clone();
            let read_entered = read_entered.clone();
            let release_read = release_read.clone();
            let read_held = read_held.clone();
            async move {
                if request.headers.contains_key("x-amz-copy-source") {
                    copy_entered.notify_one();
                    release_copy.acquire().await.unwrap().forget();
                }
                // The write primes its stage from the renamed object.
                if request.method == "GET"
                    && request.path.starts_with("/photos/C/f")
                    && !read_held.swap(true, Ordering::SeqCst)
                {
                    read_entered.notify_one();
                    release_read.acquire().await.unwrap().forget();
                }
                bucket.lock().unwrap().respond(&request)
            }
        }
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "write-racing-rename");
    let a = fs
        .intern_child("A/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();
    let f = fs.intern_child("A/f", a, EntryKind::File, 4, 0).unwrap();

    let rename = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.rename(
                ROOT_ID,
                &b"A".as_slice().into(),
                ROOT_ID,
                &b"C".as_slice().into(),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), copy_entered.notified())
        .await
        .unwrap();
    let write = tokio::spawn({
        let fs = fs.clone();
        async move { fs.write(f, 0, b"new").await }
    });
    release_copy.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), rename)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), read_entered.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), fs.fence_exact_key("C/f"))
            .await
            .is_err(),
        "the write must hold its fence on C/f, the key it is writing"
    );
    release_read.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), write)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), fs.drain(1, 1))
            .await
            .unwrap(),
        0
    );

    {
        let bucket = bucket.lock().unwrap();
        assert_eq!(bucket.keys(), ["C/f"]);
        assert_eq!(bucket.body("C/f"), Some(b"new!".as_slice()));
    }
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

/// Renames that waited behind `mv A C` with A's old path in their source or
/// target must follow A to C: the source is no longer at A/x, and a target
/// under A/ would bring A back.
#[tokio::test]
async fn renames_waiting_behind_a_directory_rename_follow_it_to_its_new_path() {
    let bucket = ModelBucket::with(&[("A/x", b"x-bytes"), ("D/f", b"f-bytes")]);
    let copy_entered = Arc::new(tokio::sync::Notify::new());
    let release_copy = Arc::new(tokio::sync::Semaphore::new(0));
    let fixture =
        copy_gated_fixture(bucket.clone(), copy_entered.clone(), release_copy.clone()).await;
    let fs = filesystem(fixture.client.clone(), "rename-behind-directory-rename");
    let a = fs
        .intern_child("A/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();
    let d = fs
        .intern_child("D/", ROOT_ID, EntryKind::Dir, DIR_SIZE, 0)
        .unwrap();
    // Cached listings resolve both sources before their fences are waited on.
    let x = fs.readdir(a, 0, 100).await.unwrap().entries[0].fileid;
    let f = fs.readdir(d, 0, 100).await.unwrap().entries[0].fileid;

    let directory = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.rename(
                ROOT_ID,
                &b"A".as_slice().into(),
                ROOT_ID,
                &b"C".as_slice().into(),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), copy_entered.notified())
        .await
        .unwrap();
    let mut out_of_a = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.rename(a, &b"x".as_slice().into(), d, &b"y".as_slice().into())
                .await
        }
    });
    let mut into_a = tokio::spawn({
        let fs = fs.clone();
        async move {
            fs.rename(d, &b"f".as_slice().into(), a, &b"g".as_slice().into())
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut out_of_a)
            .await
            .is_err()
    );
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut into_a)
        .await
        .is_err());
    // One copy for the directory's object, then one per file rename.
    release_copy.add_permits(3);
    let mut results = Vec::new();
    for rename in [directory, out_of_a, into_a] {
        results.push(
            tokio::time::timeout(Duration::from_secs(3), rename)
                .await
                .unwrap()
                .unwrap(),
        );
    }

    assert_eq!(
        bucket.lock().unwrap().keys(),
        ["C/g", "D/y"],
        "both files must have followed A to C"
    );
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert_eq!(fs.inode(x).unwrap().key, "D/y");
    assert_eq!(fs.inode(f).unwrap().key, "C/g");
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

/// Unmount refuses new writes before its final drain, and that drain must
/// still publish what was acknowledged: a staged file past the single-PUT
/// limit needs ListParts and UploadPart, which a cancelled request executor
/// refuses outright, leaving the file staged.
#[tokio::test]
async fn a_drain_after_writes_stop_still_publishes_a_multipart_stage() {
    const SIZE: u64 = stage::MULTIPART_THRESHOLD + 1;
    let fixture = serve(|request| async move {
        if request.method == "GET" && request.path.contains("uploadId=resumed") {
            // Five full parts reached the provider before the unmount began;
            // the one-byte tail did not.
            let mut body = String::from("<ListPartsResult><IsTruncated>false</IsTruncated>");
            for number in 1..=5 {
                body.push_str(&format!(
                    "<Part><PartNumber>{number}</PartNumber><ETag>&quot;part-{number}&quot;</ETag><Size>{}</Size></Part>",
                    stage::PART_SIZE
                ));
            }
            body.push_str("</ListPartsResult>");
            return Response::xml(200, &body);
        }
        if request.method == "PUT" && request.path.contains("partNumber=6") {
            return Response::empty(200).header("etag", "\"part-6\"");
        }
        if request.method == "POST" && request.path.contains("uploadId=resumed") {
            return Response::xml(
                200,
                "<CompleteMultipartUploadResult><ETag>&quot;large&quot;</ETag></CompleteMultipartUploadResult>",
            );
        }
        Response::empty(404)
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "drain-after-stop");
    let id = intern(&fs, "large").await;
    fs.setattr(
        id,
        sattr3 {
            size: set_size3::size(SIZE),
            ..sattr3::default()
        },
    )
    .await
    .unwrap();
    let snapshot = fs
        .stage_guard(id)
        .await
        .unwrap()
        .upload_snapshot()
        .await
        .unwrap();
    let mut journal = snapshot.journal().await.unwrap();
    journal.upload_id = Some("resumed".into());
    snapshot.save_journal(&journal).await.unwrap();

    fs.stop_accepting_writes();
    assert!(
        matches!(fs.write(id, 0, b"late").await, Err(nfsstat3::NFS3ERR_IO)),
        "writes stop being accepted"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), fs.drain(1, 1))
            .await
            .unwrap(),
        0,
        "the staged file must be published"
    );
    {
        let requests = fixture.requests.lock().unwrap();
        assert!(requests
            .iter()
            .any(|r| r.method == "PUT" && r.path.contains("partNumber=6")));
        assert!(requests
            .iter()
            .any(|r| r.method == "POST" && r.path.contains("uploadId=resumed")));
    }
    let _ = tokio::fs::remove_dir_all(fs.staging_root()).await;
}

/// Retiring a handle whose object changed re-interns its key under `inodes`
/// and then invalidates the parent listing under `dirs`; a directory page
/// takes `dirs` and then `inodes`. Holding `inodes` while waiting for `dirs`
/// deadlocks the two on std locks, blocking runtime workers until the whole
/// mount stops answering.
///
/// A worker blocked on a std lock can also leave the runtime's timers
/// undriven, so everything that must happen while `dirs` is held runs on a
/// plain thread with its own deadline, and `dirs` is always released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retiring_a_changed_handle_never_holds_inodes_while_waiting_for_dirs() {
    const BOUND: Duration = Duration::from_secs(10);
    let fixture = serve(|_| async {
        Response::empty(200)
            .header("content-length", 4)
            .header("etag", "\"new\"")
    })
    .await;
    let fs = filesystem(fixture.client.clone(), "inode-directory-lock-order");
    let old = fs
        .intern_child("note", ROOT_ID, EntryKind::File, 4, 0)
        .unwrap();
    fs.inner.read_identities.lock().await.insert(
        old,
        ReadIdentity {
            etag: "\"old\"".into(),
            version_id: None,
            size: 4,
            observed_at: Instant::now() - DIR_CACHE_TTL,
        },
    );
    let generation = fs.inner.directory_generation.load(Ordering::SeqCst);

    // Holds `dirs` the way a directory page does before it takes `inodes`,
    // and checks `inodes` once the retire is waiting for `dirs`.
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn({
        let fs = fs.clone();
        move || {
            let dirs = fs.inner.dirs.write().unwrap();
            held_tx.send(()).unwrap();
            // `invalidate_dir` bumps the generation just before it waits
            // for `dirs`.
            let deadline = std::time::Instant::now() + BOUND;
            while fs.inner.directory_generation.load(Ordering::SeqCst) == generation
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            let reached = fs.inner.directory_generation.load(Ordering::SeqCst) != generation;
            let inodes_free = fs.inner.inodes.try_write().is_ok();
            drop(dirs);
            (reached, inodes_free)
        }
    });
    held_rx.recv_timeout(BOUND).unwrap();
    let retire = tokio::spawn({
        let fs = fs.clone();
        async move { fs.read_identity(old, "note").await }
    });
    // Blocks this (non-worker) thread for at most the holder's deadline.
    let (reached, inodes_free) = holder.join().unwrap();

    assert!(reached, "the handle was never retired");
    assert!(inodes_free, "`inodes` was held while waiting for `dirs`");
    assert!(matches!(
        tokio::time::timeout(BOUND, retire).await.unwrap().unwrap(),
        Err(nfsstat3::NFS3ERR_STALE)
    ));
}

/// A root whose name `x` is missing until `created` is set — by another
/// client, so nothing on this mount invalidates the directory. Its first
/// listing page is partial and already shows `x`: `z` is held back as the
/// lookahead for the page after it.
async fn externally_created_child_fixture(created: Arc<AtomicBool>) -> crate::test_s3::Fixture {
    serve(move |request| {
        let created = created.clone();
        async move {
            if request.method == "GET" && request.path.contains("list-type") {
                if request.path.contains("prefix=x%2F") {
                    return directory_response(&[], &[], None);
                }
                return directory_response(&["a", "x", "z"], &[], Some("next"));
            }
            if request.method == "HEAD"
                && request.path.starts_with("/photos/x")
                && created.load(Ordering::SeqCst)
            {
                return Response::empty(200)
                    .header("content-length", 1)
                    .header("etag", "\"x\"");
            }
            Response::empty(404)
        }
    })
    .await
}

/// LOOKUP cached `x` as missing while the directory had no listing; `x` was
/// then created elsewhere and a READDIR listed it. That listing was given the
/// generation the miss had been recorded under, so the next LOOKUP answered
/// NOENT for a name the client had just been shown.
#[tokio::test]
async fn a_listing_made_after_a_cached_miss_is_not_contradicted_by_it() {
    let created = Arc::new(AtomicBool::new(false));
    let fixture = externally_created_child_fixture(created.clone()).await;
    let fs = filesystem(fixture.client.clone(), "listing-after-miss");
    assert!(fs.lookup_child(ROOT_ID, "", "x").await.unwrap().is_none());
    created.store(true, Ordering::SeqCst);

    let page = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert_eq!(directory_names(&page), ["a", "x"]);
    assert!(!page.end);
    assert!(
        fs.lookup_child(ROOT_ID, "", "x").await.unwrap().is_some(),
        "the listing that shows `x` is newer than the cached miss"
    );
}

/// The same miss must not answer a LOOKUP that began before the listing
/// existed but waited — here behind a fence on `x` — until after it was
/// made: whatever that LOOKUP checks or records belongs to the listing's
/// generation, not the older one it started with.
#[tokio::test]
async fn a_lookup_that_waited_out_a_new_listing_does_not_answer_from_an_older_miss() {
    let created = Arc::new(AtomicBool::new(false));
    let fixture = externally_created_child_fixture(created.clone()).await;
    let fs = filesystem(fixture.client.clone(), "lookup-across-listing");
    assert!(fs.lookup_child(ROOT_ID, "", "x").await.unwrap().is_none());
    created.store(true, Ordering::SeqCst);

    let held = fs.fence_exact_key("x").await;
    let mut lookup = tokio::spawn({
        let fs = fs.clone();
        async move { fs.lookup_child(ROOT_ID, "", "x").await }
    });
    // The lookup reads its generation, then parks on the fence.
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut lookup)
        .await
        .is_err());
    let page = fs.readdir(ROOT_ID, 0, 100).await.unwrap();
    assert_eq!(directory_names(&page), ["a", "x"]);
    drop(held);
    let found = tokio::time::timeout(Duration::from_secs(3), lookup)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        found.is_some(),
        "a listing that shows `x` outranks the older miss"
    );
}
