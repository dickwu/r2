//! Opt-in guest helpers. The host witnesses a real kernel NFS fsync reply,
//! then cuts VM power; a later boot runs production recovery independently.
//! These tests never stop the VM themselves and cannot run on the host Mac.
use super::*;
use std::io::{Seek, Write};

const KEY: &str = "VM + 持久写入.bin";
const BUCKET: &str = "photos";
const ACCOUNT: &str = "isolated-vm-audit";
const SCOPE: &str = "isolated-vm-audit-scope";

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Mutation {
    Write {
        offset: usize,
        size: usize,
        seed: u8,
    },
    Truncate {
        size: usize,
    },
}

#[derive(Deserialize)]
struct Scenario {
    base_size: usize,
    mutations: Vec<Mutation>,
}

fn guest_root() -> PathBuf {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "Power-cut helper requires the disposable Linux VM"
    );
    let marker: serde_json::Value = serde_json::from_slice(
        &std::fs::read("/etc/r2-audit-disposable").expect("disposable VM marker"),
    )
    .unwrap();
    assert_eq!(marker["kind"], "r2-disposable-vm");
    assert_eq!(
        marker["owner"].as_str(),
        Some(std::env::var("R2_VM_OWNER").unwrap().as_str())
    );
    let root = PathBuf::from(std::env::var_os("R2_VM_AUDIT_ROOT").expect("explicit audit root"));
    let base = Path::new("/var/lib/r2-audit");
    assert_eq!(
        root.parent(),
        Some(base),
        "Use one owned case directory on the audit disk"
    );
    let name = root.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("case-") && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    );
    let canonical_base = base.canonicalize().unwrap();
    assert_eq!(
        canonical_base, base,
        "Audit filesystem must not be a host share/symlink"
    );
    root
}

fn pattern(size: usize, seed: u8) -> Vec<u8> {
    (0..size)
        .map(|index| ((index * 37 + usize::from(seed)) % 251) as u8)
        .collect()
}

fn scenario() -> Scenario {
    let scenario: Scenario =
        serde_json::from_str(&std::env::var("R2_VM_SCENARIO").unwrap()).unwrap();
    assert!(scenario.base_size <= 16 * 1024 * 1024);
    assert!(!scenario.mutations.is_empty() && scenario.mutations.len() <= 256);
    for mutation in &scenario.mutations {
        match mutation {
            Mutation::Write { offset, size, .. } => {
                assert!(*size > 0 && *size <= 1024 * 1024);
                assert!(offset
                    .checked_add(*size)
                    .is_some_and(|end| end <= 32 * 1024 * 1024));
            }
            Mutation::Truncate { size } => assert!(*size <= 32 * 1024 * 1024),
        }
    }
    scenario
}

fn witness(label: &str, value: serde_json::Value) {
    println!("{label} {value}");
    std::io::stdout().flush().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an owned disposable Linux VM; host cuts power after a client ACK witness"]
async fn vm_nfs_powercut_writer() {
    let root = guest_root();
    let scenario = scenario();
    assert!(
        !root.exists(),
        "Never reuse or overwrite a previous power-cut case"
    );
    let target = root.join("mount");
    let staging = root.join("staging");
    tokio::fs::create_dir_all(&target).await.unwrap();
    tokio::fs::create_dir_all(&staging).await.unwrap();
    super::super::super::recovery::save_mount_manifest(
        &staging,
        super::super::super::manager::MountProvider::Aws,
        ACCOUNT,
        BUCKET,
        SCOPE,
    )
    .unwrap();
    let mut expected = pattern(scenario.base_size, 11);
    let initial = Object {
        data: expected.clone(),
        meta: HashMap::new(),
    };
    let initial_etag = initial.etag();
    let objects = Arc::new(std::sync::Mutex::new(HashMap::from([(
        KEY.to_owned(),
        initial,
    )])));
    let fixture = serve({
        let objects = objects.clone();
        move |request| {
            let objects = objects.clone();
            async move { respond(request, &mut objects.lock().unwrap()) }
        }
    })
    .await;
    let fs = S3NfsFs::new(
        fixture.client.clone(),
        BUCKET.into(),
        false,
        staging.clone(),
    );
    fs.configure_transfer(crate::move_transfer::config::MoveConfig::Aws(
        crate::providers::aws::AwsConfig {
            bucket: BUCKET.into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            region: "us-east-1".into(),
            endpoint_scheme: None,
            endpoint_host: None,
            force_path_style: true,
        },
    ));
    // No background flusher: acknowledged bytes must survive in stage/WAL.
    let listener = NFSTcpListener::bind("127.0.0.1:0", fs.clone())
        .await
        .unwrap();
    let port = listener.get_listen_port();
    let server = tokio::spawn(async move { listener.handle_forever().await });
    let argv = super::super::super::platform::mount_argv(
        super::super::super::platform::MountPlatform::Linux,
        "127.0.0.1",
        port,
        target.to_str().unwrap(),
        false,
    );
    let mounted = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        mounted.status.success(),
        "NFS mount failed: {}",
        String::from_utf8_lossy(&mounted.stderr)
    );
    let file_path = target.join(KEY);
    for (index, mutation) in scenario.mutations.into_iter().enumerate() {
        let path = file_path.clone();
        let mutation_copy = mutation.clone();
        tokio::task::spawn_blocking(move || {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            match mutation_copy {
                Mutation::Write { offset, size, seed } => {
                    file.seek(std::io::SeekFrom::Start(offset as u64)).unwrap();
                    file.write_all(&pattern(size, seed)).unwrap();
                }
                Mutation::Truncate { size } => file.set_len(size as u64).unwrap(),
            }
            // Kernel client completion is the witness, not a server-side log
            // or marker-file write that might accidentally sync the audit disk.
            file.sync_all().unwrap();
        })
        .await
        .unwrap();
        match mutation {
            Mutation::Write { offset, size, seed } => {
                expected.resize(expected.len().max(offset + size), 0);
                expected[offset..offset + size].copy_from_slice(&pattern(size, seed));
            }
            Mutation::Truncate { size } => expected.resize(size, 0),
        }
        let handles: Vec<_> = fs.inner.stages.lock().await.values().cloned().collect();
        assert_eq!(handles.len(), 1);
        let stage = handles[0].lock().await;
        assert_eq!(stage.key, KEY);
        assert!(stage.dirty && stage.next_lsn > 1);
        let lsn = stage.next_lsn - 1;
        let generation = stage.dirty_gen;
        drop(stage);
        assert_eq!(
            objects.lock().unwrap()[KEY].etag(),
            initial_etag,
            "No remote publication may substitute for WAL recovery"
        );
        witness(
            "R2_VM_ACK",
            serde_json::json!({
                "operation": index + 1, "kind": "kernel_nfs_fsync_returned", "bucket": BUCKET, "key": KEY,
                "size": expected.len(), "sha256": format!("{:x}", Sha256::digest(&expected)),
                "lsn": lsn, "generation": generation, "initial_etag": initial_etag,
            }),
        );
        // The host either allows the next RPC or cuts guest power. Keeping
        // this handshake prevents a later unobserved overwrite racing a cut.
        let line = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap();
            line
        })
        .await
        .unwrap();
        assert_eq!(
            line.trim(),
            "continue",
            "Host must explicitly continue or cut VM power"
        );
    }
    std::future::pending::<()>().await;
    drop((fixture, server));
}

#[tokio::test]
#[ignore = "post-power-cut production recovery check inside the same owned Linux VM"]
async fn vm_nfs_powercut_recover() {
    let root = guest_root();
    let expected: serde_json::Value =
        serde_json::from_str(&std::env::var("R2_VM_EXPECTED_ACK").unwrap()).unwrap();
    let staging = root.join("staging");
    super::super::super::recovery::validate_identity(
        &staging,
        super::super::super::manager::MountProvider::Aws,
        ACCOUNT,
        BUCKET,
        SCOPE,
    )
    .unwrap();
    // Recreate the unchanged pre-cut provider state. The writer checked that
    // nothing had been published before every ACK, so this models a provider
    // that stayed alive while the client VM lost power.
    let initial = Object {
        data: pattern(scenario().base_size, 11),
        meta: HashMap::new(),
    };
    assert_eq!(initial.etag(), expected["initial_etag"].as_str().unwrap());
    let objects = Arc::new(std::sync::Mutex::new(HashMap::from([(
        KEY.to_owned(),
        initial,
    )])));
    let fixture = serve({
        let objects = objects.clone();
        move |request| {
            let objects = objects.clone();
            async move { respond(request, &mut objects.lock().unwrap()) }
        }
    })
    .await;
    let fs = S3NfsFs::new(
        fixture.client.clone(),
        BUCKET.into(),
        false,
        staging.clone(),
    );
    fs.configure_transfer(crate::move_transfer::config::MoveConfig::Aws(
        crate::providers::aws::AwsConfig {
            bucket: BUCKET.into(),
            access_key_id: "fixture".into(),
            secret_access_key: "fixture-secret".into(),
            region: "us-east-1".into(),
            endpoint_scheme: None,
            endpoint_host: None,
            force_path_style: true,
        },
    ));
    assert_eq!(
        fs.restore_stages().await.unwrap(),
        1,
        "Use the production mount restore/quarantine/quota path"
    );
    let handles: Vec<_> = fs
        .inner
        .stages
        .lock()
        .await
        .iter()
        .map(|(id, handle)| (*id, handle.clone()))
        .collect();
    assert_eq!(handles.len(), 1);
    let (id, handle) = &handles[0];
    let recovered = handle.lock().await;
    assert_eq!(recovered.key, KEY);
    assert!(recovered.dirty && recovered.first_dirty_at.is_some());
    assert_eq!(
        recovered.publication_guard,
        Some(stage::PublicationGuard::Match {
            etag: expected["initial_etag"].as_str().unwrap().into()
        })
    );
    assert_eq!(recovered.size, expected["size"].as_u64().unwrap());
    assert!(recovered.checkpoint_lsn >= expected["lsn"].as_u64().unwrap());
    let checkpoint_lsn = recovered.checkpoint_lsn;
    let generation = recovered.dirty_gen;
    let publication_guard = recovered.publication_guard.clone();
    drop(recovered);
    let mut bytes = Vec::new();
    while bytes.len() < expected["size"].as_u64().unwrap() as usize {
        let (part, _) = fs.read(*id, bytes.len() as u64, 1024 * 1024).await.unwrap();
        assert!(
            !part.is_empty(),
            "Restored mount ended before acknowledged bytes"
        );
        bytes.extend(part);
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(digest, expected["sha256"].as_str().unwrap());
    let pending = tokio::time::timeout(Duration::from_secs(30), fs.drain(2, 1))
        .await
        .unwrap();
    assert_eq!(pending, 0, "Recovered publication must fully converge");
    assert_eq!(
        format!("{:x}", Sha256::digest(&objects.lock().unwrap()[KEY].data)),
        digest
    );
    assert_eq!(fs.pending_upload_count().await, 0);
    witness(
        "R2_VM_RECOVERED",
        serde_json::json!({
            "bucket": BUCKET, "key": KEY, "size": bytes.len(), "sha256": digest,
            "checkpoint_lsn": checkpoint_lsn, "generation": generation,
            "publication_guard": publication_guard, "mount_restore": true, "provider_published": true,
            "provider_mode": "recreated unchanged loopback provider fixture", "passed": true,
        }),
    );
}
