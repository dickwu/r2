use super::*;
use crate::test_s3::{serve, Response};

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
        DirListing {
            children: Arc::new(vec![DirChild {
                fileid: id,
                name: key.into(),
            }]),
            fetched_at: Instant::now(),
        },
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
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "HEAD");
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
    assert!(fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.method == "GET"
            && r.headers
                .get("if-match")
                .is_some_and(|value| value == "\"version\"")));
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
        let token=if pages.fetch_add(1,Ordering::SeqCst).is_multiple_of(2) {"a"} else {"b"};
        Response::xml(200,&format!("<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>{token}</NextContinuationToken><Contents><Key>file</Key><Size>1</Size></Contents></ListBucketResult>"))
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
    fs.configure_quota(8).await;
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
        DirListing {
            children: Arc::new(vec![
                DirChild {
                    fileid: a,
                    name: "a".into(),
                },
                DirChild {
                    fileid: b,
                    name: "b".into(),
                },
            ]),
            fetched_at: Instant::now(),
        },
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
        DirListing {
            children: Arc::new(vec![
                DirChild {
                    fileid: a,
                    name: "a".into(),
                },
                DirChild {
                    fileid: b,
                    name: "b".into(),
                },
            ]),
            fetched_at: Instant::now(),
        },
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
    let journal = stage::MultipartJournal {
        upload_id: Some("already-uploaded".into()),
        part_size: stage::planned_part_size(size),
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
