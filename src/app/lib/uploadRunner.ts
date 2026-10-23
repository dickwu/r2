import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { uploadFile, type StorageConfig } from '@/app/lib/r2cache';
import {
  accountDisplayName,
  preferRecorded,
  uploadFailure,
  type UploadFailureContext,
} from '@/app/lib/taskFailures';
import { useAccountStore } from '@/app/stores/accountStore';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';
import { useUploadStore, type UploadTask } from '@/app/stores/uploadStore';

interface UploadProgressEvent {
  task_id: string;
  percent: number;
  uploaded_bytes: number;
  total_bytes: number;
  speed?: number;
}

interface UploadRetryEvent {
  task_id: string;
  attempt: number;
  max_attempts: number;
  delay_ms: number;
  error: string;
}

// The object key an upload writes: the destination folder, then the file's
// (possibly auto-renamed) name
export function uploadKey(uploadPath: string, task: UploadTask): string {
  const folder = uploadPath && !uploadPath.endsWith('/') ? `${uploadPath}/` : uploadPath;
  return folder + (task.renamedFileName ?? task.fileName);
}

// Where an upload is going, as its failure record names it
export function uploadContext(config: StorageConfig | null, key: string): UploadFailureContext {
  if (!config) return { key };
  const { accounts } = useAccountStore.getState();
  return {
    key,
    bucket: config.bucket,
    account: accountDisplayName(accounts, config.provider, config.accountId),
  };
}

// Report the upload that just failed, read back from the store so the record
// carries the last progress the row showed
function reportUploadFailure(taskId: string, context: UploadFailureContext) {
  const failed = useUploadStore.getState().tasks.find((t) => t.id === taskId);
  if (failed) useTransferErrorStore.getState().report(uploadFailure(failed, context));
}

// "Show error" re-opens the reported record, which holds the destination the
// upload used; it rebuilds one only after the failure modal's list was cleared
export function showUploadFailure(task: UploadTask) {
  const { config, uploadPath } = useUploadStore.getState();
  const fresh = uploadFailure(task, uploadContext(config, uploadKey(uploadPath, task)));
  const { failures, show } = useTransferErrorStore.getState();
  show(preferRecorded(fresh, failures));
}

function missingCredentials(config: StorageConfig): boolean {
  if (!config.accessKeyId || !config.secretAccessKey) return true;
  if (config.provider === 'aws' && !config.region) return true;
  return (
    (config.provider === 'minio' || config.provider === 'rustfs') &&
    (!config.endpointHost || !config.endpointScheme)
  );
}

const isCancelled = (error: string) => error.includes('cancelled');

/** Run one upload and record its outcome on the task. */
async function runUpload(task: UploadTask): Promise<void> {
  const { config, uploadPath, updateTask } = useUploadStore.getState();
  const key = uploadKey(uploadPath, task);
  // A failed upload is recorded on the task and reported to the failure modal,
  // with the destination this upload used
  const fail = (error: string) => {
    updateTask(task.id, { status: 'error', error, speed: 0, retry: undefined });
    reportUploadFailure(task.id, uploadContext(config, key));
  };

  if (!config || missingCredentials(config)) {
    fail('Storage credentials are incomplete. Check the account settings.');
    return;
  }

  try {
    const result = await uploadFile(config, {
      taskId: task.id,
      filePath: task.filePath,
      key,
      contentType: task.contentType,
    });
    if (result.success) {
      updateTask(task.id, { status: 'success', progress: 100, speed: 0, retry: undefined });
    } else if (result.error && isCancelled(result.error)) {
      updateTask(task.id, { status: 'cancelled', speed: 0, retry: undefined });
    } else {
      fail(result.error || 'Upload failed');
    }
  } catch (e) {
    const errorMsg = e instanceof Error ? e.message : String(e);
    if (isCancelled(errorMsg)) {
      updateTask(task.id, { status: 'cancelled', speed: 0, retry: undefined });
    } else {
      fail(errorMsg);
    }
  }
}

function cancelUpload(taskId: string) {
  invoke('cancel_upload', { taskId }).catch(() => {});
}

function isUploading(taskId: string): boolean {
  return useUploadStore.getState().tasks.some((t) => t.id === taskId && t.status === 'uploading');
}

let installed = false;

/**
 * Install the upload runner and the app-wide progress/retry listeners, once.
 * Starting uploads is then driven by the store's queue rather than by each
 * row's effect, so at most `maxConcurrent` files transfer at a time.
 */
export function installUploadRunner(): void {
  if (installed) return;
  installed = true;

  listen<UploadProgressEvent>('upload-progress', ({ payload }) => {
    if (!isUploading(payload.task_id)) return;
    useUploadStore.getState().updateTask(payload.task_id, {
      progress: payload.percent,
      speed: payload.speed ?? 0,
    });
  }).catch(() => {});

  listen<UploadRetryEvent>('upload-retry', ({ payload }) => {
    if (!isUploading(payload.task_id)) return;
    useUploadStore.getState().updateTask(payload.task_id, {
      speed: 0,
      retry: {
        attempt: payload.attempt,
        maxAttempts: payload.max_attempts,
        error: payload.error,
      },
    });
  }).catch(() => {});

  useUploadStore.getState().setUploadRunner(runUpload, cancelUpload);
}
