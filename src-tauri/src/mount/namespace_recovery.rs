//! Durable namespace intents are kept until a conditional remote outcome and
//! local stage cleanup have both converged.
use super::*;
use crate::providers::conditional::Condition;

#[derive(Serialize, Deserialize)]
struct NamespaceIntent {
    version: u32,
    key: String,
    operation: String,
    token: String,
    previous_etag: Option<String>,
}

pub(super) async fn pending_operations(root: &Path) -> Result<Vec<stage::StageRecovery>, String> {
    let mut result = Vec::new();
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error.to_string()),
    };
    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if (!name.starts_with("namespace-") && !name.starts_with("rename-"))
            || !name.ends_with(".json")
            || name.starts_with("rename-part-")
        {
            continue;
        }
        let record = async {
            let metadata = tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(|e| e.to_string())?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 32 * 1024 * 1024
            {
                return Err("Invalid namespace recovery record".to_string());
            }
            let value: serde_json::Value = serde_json::from_slice(
                &tokio::fs::read(entry.path())
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let key = value
                .get("key")
                .or_else(|| value.get("from"))
                .and_then(|value| value.as_str())
                .ok_or("Namespace record omitted its source key")?
                .to_string();
            Ok(stage::StageRecovery {
                key,
                size: 0,
                mtime_secs: 0,
                generation: 0,
                dirty: false,
                state: if name.starts_with("rename-") {
                    "rename_recovery".into()
                } else {
                    "namespace_recovery".into()
                },
                error: value
                    .get("last_error")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                path: entry.path(),
                snapshot: None,
                publication_guard: None,
            })
        }
        .await;
        match record {
            Ok(record) => result.push(record),
            Err(error) => {
                if tokio::fs::try_exists(entry.path()).await.unwrap_or(true) {
                    result.push(stage::unreadable_record(entry.path(), String::new(), error));
                }
            }
        }
    }
    Ok(result)
}

impl S3NfsFs {
    pub(super) async fn object_head(
        &self,
        key: &str,
    ) -> Result<Option<aws_sdk_s3::operation::head_object::HeadObjectOutput>, nfsstat3> {
        match crate::providers::s3_client::retry_idempotent(3, || {
            self.inner
                .client
                .head_object()
                .bucket(&self.inner.bucket)
                .key(key)
                .send()
        })
        .await
        {
            Ok(head) => {
                self.io_succeeded();
                Ok(Some(head))
            }
            Err(error)
                if matches!(map_s3_error(&error), nfsstat3::NFS3ERR_NOENT)
                    && error.as_service_error().and_then(|e| e.code()) != Some("NoSuchBucket") =>
            {
                self.io_succeeded();
                Ok(None)
            }
            Err(error) => {
                self.io_failed(format!("HEAD: {}", describe_s3_error(&error)));
                Err(map_s3_error(&error))
            }
        }
    }

    pub(super) async fn put_empty_object(&self, key: &str, replace: bool) -> Result<(), nfsstat3> {
        self.ensure_key_settled(key).await?;
        let head = self.object_head(key).await?;
        if !replace && head.is_some() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }
        let previous_etag = head
            .as_ref()
            .map(|h| h.e_tag().ok_or(nfsstat3::NFS3ERR_IO).map(str::to_string))
            .transpose()?;
        let condition = if previous_etag.is_some() {
            Condition::PutMatch
        } else {
            Condition::PutCreate
        };
        if !self.condition_supported(condition).await? {
            return Err(nfsstat3::NFS3ERR_NOTSUPP);
        }
        let intent = NamespaceIntent {
            version: 1,
            key: key.into(),
            operation: "empty".into(),
            token: format!(
                "{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            previous_etag,
        };
        let path = self.namespace_journal_path(key);
        stage::write_json_atomic(&path, &intent)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        self.apply_namespace_intent(&path, &intent).await
    }

    pub(super) async fn delete_object(&self, key: &str) -> Result<(), nfsstat3> {
        self.ensure_key_settled(key).await?;
        let Some(head) = self.object_head(key).await? else {
            return Ok(());
        };
        if !self.condition_supported(Condition::DeleteMatch).await? {
            return Err(nfsstat3::NFS3ERR_NOTSUPP);
        }
        let intent = NamespaceIntent {
            version: 1,
            key: key.into(),
            operation: "delete".into(),
            token: format!(
                "{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            previous_etag: Some(head.e_tag().ok_or(nfsstat3::NFS3ERR_IO)?.into()),
        };
        let path = self.namespace_journal_path(key);
        stage::write_json_atomic(&path, &intent)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        self.apply_namespace_intent(&path, &intent).await
    }

    async fn apply_namespace_intent(
        &self,
        path: &Path,
        intent: &NamespaceIntent,
    ) -> Result<(), nfsstat3> {
        if intent.version != 1 {
            return Err(nfsstat3::NFS3ERR_NOTSUPP);
        }
        let head = self.object_head(&intent.key).await?;
        let already_done = match intent.operation.as_str() {
            "empty" => head.as_ref().is_some_and(|h| {
                h.content_length() == Some(0)
                    && h.metadata().and_then(|m| m.get("r2-namespace-operation"))
                        == Some(&intent.token)
            }),
            "delete" => head.is_none(),
            _ => return Err(nfsstat3::NFS3ERR_NOTSUPP),
        };
        if !already_done {
            let condition = if intent.operation == "delete" {
                Condition::DeleteMatch
            } else if intent.previous_etag.is_some() {
                Condition::PutMatch
            } else {
                Condition::PutCreate
            };
            if !self.condition_supported(condition).await? {
                return Err(nfsstat3::NFS3ERR_NOTSUPP);
            }
            if head.as_ref().and_then(|h| h.e_tag()) != intent.previous_etag.as_deref() {
                return Err(nfsstat3::NFS3ERR_IO);
            }
            if intent.operation == "empty" {
                let request = self
                    .inner
                    .client
                    .put_object()
                    .bucket(&self.inner.bucket)
                    .key(&intent.key)
                    .metadata("r2-namespace-operation", &intent.token)
                    .body(ByteStream::from_static(b""));
                let request = if let Some(etag) = &intent.previous_etag {
                    request.if_match(etag)
                } else {
                    request.if_none_match("*")
                };
                request.send().await.map_err(|e| map_s3_error(&e))?;
            } else {
                self.inner
                    .client
                    .delete_object()
                    .bucket(&self.inner.bucket)
                    .key(&intent.key)
                    .if_match(
                        intent
                            .previous_etag
                            .as_deref()
                            .ok_or(nfsstat3::NFS3ERR_IO)?,
                    )
                    .send()
                    .await
                    .map_err(|e| map_s3_error(&e))?;
            }
        }
        let id = self
            .inner
            .inodes
            .read()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .by_key
            .get(&intent.key)
            .copied();
        if let Some(id) = id {
            self.discard_stage(id).await;
            self.inner.read_cache.forget_file(id);
        }
        tokio::fs::remove_file(path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        stage::sync_parent(path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        Ok(())
    }

    async fn resume_namespace_record(&self, record: &stage::StageRecovery) -> Result<(), String> {
        let bytes = tokio::fs::read(&record.path)
            .await
            .map_err(|e| e.to_string())?;
        if record.state == "namespace_recovery" {
            let intent: NamespaceIntent = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Incomplete namespace identity: {e}"))?;
            self.apply_namespace_intent(&record.path, &intent)
                .await
                .map_err(|e| {
                    format!(
                        "Namespace recovery for {} needs attention: {e:?}",
                        intent.key
                    )
                })?;
        } else {
            let journal: RenameJournal =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let pairs = journal
                .objects
                .iter()
                .map(|o| (o.from.clone(), o.to.clone()))
                .collect();
            self.rename_objects(&journal.from, &journal.to, pairs)
                .await
                .map_err(|e| format!("Rename recovery needs attention: {e:?}"))?;
            for object in &journal.objects {
                for key in [&object.from, &object.to] {
                    let id = self
                        .inner
                        .inodes
                        .read()
                        .map_err(|_| "Inode registry unavailable")?
                        .by_key
                        .get(key)
                        .copied();
                    if let Some(id) = id {
                        self.discard_stage(id).await;
                    }
                }
            }
            tokio::fs::remove_file(&record.path)
                .await
                .map_err(|e| e.to_string())?;
            stage::sync_parent(&record.path)
                .await
                .map_err(|e| e.to_string())?;
            self.inner
                .pending_renames
                .write()
                .map_err(|_| "Rename registry unavailable")?
                .remove(&record.path);
        }
        self.inner
            .pending_renames
            .write()
            .map_err(|_| "Recovery registry unavailable")?
            .remove(&record.path);
        Ok(())
    }

    pub async fn resume_namespace_operations(&self) -> Result<(), String> {
        let _namespace = self.inner.namespace.write().await;
        self.inner
            .recovery_errors
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        for record in pending_operations(&self.inner.staging_root).await? {
            let result = if record.state == "unreadable" {
                Err(record
                    .error
                    .clone()
                    .unwrap_or_else(|| "Unreadable namespace record".into()))
            } else {
                self.resume_namespace_record(&record).await
            };
            if let Err(error) = result {
                self.inner
                    .recovery_errors
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(error.clone());
                if !record.key.is_empty() {
                    self.inner
                        .pending_renames
                        .write()
                        .map_err(|_| "Recovery registry unavailable")?
                        .entry(record.path.clone())
                        .or_insert((record.key.clone(), record.key.clone()));
                }
                if let Ok(bytes) = tokio::fs::read(&record.path).await {
                    if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        if !value.is_object() {
                            continue;
                        }
                        value["last_error"] = serde_json::Value::String(error);
                        if let Err(error) = stage::write_json_atomic(&record.path, &value).await {
                            self.io_failed(format!("Unable to save recovery diagnostics: {error}"));
                        }
                    }
                }
            }
        }
        self.invalidate_all_dirs();
        Ok(())
    }
}
