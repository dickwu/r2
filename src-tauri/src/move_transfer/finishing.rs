//! Post-upload finishing tasks (cache update, delete source)

use crate::commands::delete_cache::queue_cache_after_delete;
use crate::commands::upload_cache::update_cache_after_upload;
use crate::db::move_sessions::{get_move_journal, save_move_journal, SourceIdentity};
use crate::db::MoveSession;
use aws_sdk_s3::error::ProvideErrorMetadata;
use chrono::Utc;
use log::info;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::AppHandle;

use super::config::MoveConfig;
use super::planner::{
    head_identity_checked, native_aws, storage_error, verified_destination, TRANSFER_MARKER,
};

#[derive(Debug, PartialEq, Eq)]
enum DeleteGuard<'a> {
    Version(&'a str),
    Etag(&'a str),
    Unverified,
}

fn delete_guard<'a>(config: &MoveConfig, source: &'a SourceIdentity) -> DeleteGuard<'a> {
    if let Some(version) = source.version_id.as_deref() {
        return DeleteGuard::Version(version);
    }
    if native_aws(config) {
        DeleteGuard::Etag(&source.etag)
    } else {
        DeleteGuard::Unverified
    }
}

fn check_control(cancelled: &AtomicBool, paused: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err("cancelled: Copy verified; source retained".into());
    }
    if paused.load(Ordering::SeqCst) {
        return Err("paused: Copy verified; source retained".into());
    }
    Ok(())
}

/// Run post-upload cache operations in background (non-blocking)
pub(crate) async fn run_cache_operations(
    app: AppHandle,
    task_id: String,
    dest_bucket: String,
    dest_account_id: String,
    dest_key: String,
    uploaded_size: u64,
) {
    info!(
        "finishing_cache_start: {} {}/{} key={} size={}",
        task_id, dest_account_id, dest_bucket, dest_key, uploaded_size
    );
    let last_modified = Utc::now().to_rfc3339();
    let _ = update_cache_after_upload(
        &app,
        &dest_bucket,
        &dest_account_id,
        &dest_key,
        uploaded_size as i64,
        &last_modified,
    )
    .await;
    info!(
        "finishing_cache_done: {} {}/{} key={}",
        task_id, dest_account_id, dest_bucket, dest_key
    );
}

/// Resume only source deletion after checking the durable destination receipt.
/// There is no unconditional DELETE fallback for an unverified provider.
pub(crate) async fn run_delete_original(
    app: &AppHandle,
    session: &MoveSession,
    source_config: &MoveConfig,
    dest_config: &MoveConfig,
    cancelled: &Arc<AtomicBool>,
    paused: &Arc<AtomicBool>,
) -> Result<(), String> {
    check_control(cancelled, paused)?;
    let mut journal = get_move_journal(&session.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Missing move journal; source retained")?;
    journal
        .metrics
        .verification_started_at_ms
        .get_or_insert_with(|| chrono::Utc::now().timestamp_millis());
    save_move_journal(&journal)
        .await
        .map_err(|e| format!("Cannot persist verification metrics: {e}"))?;
    let (destination, head) = super::stream::protocol::interruptible(
        cancelled,
        paused,
        head_identity_checked(dest_config, &session.dest_key, cancelled, paused),
    )
    .await??
    .ok_or("conflict: Verified destination disappeared; source retained")?;
    verified_destination(
        &journal,
        &destination,
        head.metadata()
            .and_then(|m| m.get(TRANSFER_MARKER))
            .map(String::as_str),
    )?;
    let source = super::stream::protocol::interruptible(
        cancelled,
        paused,
        head_identity_checked(source_config, &session.source_key, cancelled, paused),
    )
    .await??;
    journal.metrics.verification_completed_at_ms = Some(chrono::Utc::now().timestamp_millis());
    if let Some((identity, _)) = source {
        if identity != journal.source {
            return Err(
                "conflict: Source changed after copying; the current source has been retained"
                    .into(),
            );
        }
        journal
            .metrics
            .delete_started_at_ms
            .get_or_insert_with(|| chrono::Utc::now().timestamp_millis());
        journal.stage = "delete_pending".into();
        save_move_journal(&journal)
            .await
            .map_err(|e| format!("Cannot persist pending deletion: {e}"))?;
        let mut guard = delete_guard(source_config, &journal.source);
        if guard == DeleteGuard::Unverified
            && source_config
                .supports_condition(crate::providers::conditional::Condition::DeleteMatch)
                .await?
        {
            guard = DeleteGuard::Etag(&journal.source.etag);
        }
        if guard == DeleteGuard::Unverified {
            return Err("needs_action: Copy verified and source retained: conditional deletion has not been verified for this provider. Remove the source separately after review.".into());
        }
        check_control(cancelled, paused)?;
        let client = source_config.client().await?;
        let request = client
            .delete_object()
            .bucket(source_config.bucket())
            .key(&session.source_key);
        let request = match guard {
            DeleteGuard::Version(version) => request.version_id(version),
            DeleteGuard::Etag(etag) => request.if_match(etag),
            DeleteGuard::Unverified => unreachable!(),
        };
        journal.stage = "delete_unknown".into();
        save_move_journal(&journal)
            .await
            .map_err(|e| format!("Cannot persist deletion intent: {e}"))?;
        check_control(cancelled, paused)?;
        if let Err(error) =
            super::stream::protocol::interruptible(cancelled, paused, request.send()).await?
        {
            let reason = storage_error(
                "DeleteObject",
                error.as_service_error().and_then(|e| e.code()),
                error.raw_response().map(|r| r.status().as_u16()),
                &error,
                true,
            );
            if reason.starts_with("outcome_unknown:") {
                return Err(reason);
            }
            journal.stage = "delete_pending".into();
            save_move_journal(&journal)
                .await
                .map_err(|e| e.to_string())?;
            return Err(
                if reason.starts_with("needs_auth:") || reason.starts_with("conflict:") {
                    reason
                } else {
                    format!("delete_pending: {reason}")
                },
            );
        }
        // Deleting a pinned version must never delete another version that
        // became current. Exposing an older version is visible to the user.
        if journal.source.version_id.is_some()
            && super::stream::protocol::interruptible(
                cancelled,
                paused,
                head_identity_checked(source_config, &session.source_key, cancelled, paused),
            )
            .await??
            .is_some()
        {
            journal.stage = "delete_pending".into();
            save_move_journal(&journal)
                .await
                .map_err(|e| e.to_string())?;
            return Err("needs_action: Copied source version was removed; a different version remains at the source key and has been retained".into());
        }
    }
    journal.metrics.delete_completed_at_ms = Some(chrono::Utc::now().timestamp_millis());
    journal.stage = "complete".into();
    save_move_journal(&journal)
        .await
        .map_err(|e| format!("Cannot persist completed deletion: {e}"))?;
    queue_cache_after_delete(
        app.clone(),
        session.source_bucket.clone(),
        session.source_account_id.clone(),
        session.source_key.clone(),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unversioned_custom_provider_cannot_fall_back_to_unconditional_delete() {
        let config = MoveConfig::R2(crate::r2::R2Config {
            account_id: "account".into(),
            bucket: "bucket".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
        });
        let mut identity = SourceIdentity {
            size: 1,
            etag: "etag".into(),
            version_id: None,
        };
        assert_eq!(delete_guard(&config, &identity), DeleteGuard::Unverified);
        identity.version_id = Some("immutable-version".into());
        assert_eq!(
            delete_guard(&config, &identity),
            DeleteGuard::Version("immutable-version")
        );
    }
    #[test]
    fn native_aws_deletion_is_conditional() {
        let config = MoveConfig::Aws(crate::providers::aws::AwsConfig {
            bucket: "bucket".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            region: "us-east-1".into(),
            endpoint_scheme: None,
            endpoint_host: None,
            force_path_style: false,
        });
        let identity = SourceIdentity {
            size: 1,
            etag: "etag".into(),
            version_id: None,
        };
        assert_eq!(delete_guard(&config, &identity), DeleteGuard::Etag("etag"));
    }
}
