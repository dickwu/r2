'use client';

import { useEffect } from 'react';
import { Button, Select, Typography } from 'antd';
import { StopOutlined } from '@ant-design/icons';
import { MAX_CONCURRENT_UPLOADS, MIN_CONCURRENT_UPLOADS } from '@/app/lib/uploadQueue';
import { installUploadRunner } from '@/app/lib/uploadRunner';
import {
  useUploadStore,
  selectPendingCount,
  selectQueuedCount,
  selectUploadingCount,
  selectFinishedCount,
  selectHasActiveUploads,
} from '@/app/stores/uploadStore';
import UploadTaskItem from '@/app/components/UploadTaskItem';

const { Text } = Typography;

const CONCURRENCY_OPTIONS = Array.from(
  { length: MAX_CONCURRENT_UPLOADS - MIN_CONCURRENT_UPLOADS + 1 },
  (_, i) => ({ value: MIN_CONCURRENT_UPLOADS + i, label: `${MIN_CONCURRENT_UPLOADS + i}` })
);

export default function UploadTaskList() {
  const tasks = useUploadStore((s) => s.tasks);
  const startAllPending = useUploadStore((s) => s.startAllPending);
  const clearFinished = useUploadStore((s) => s.clearFinished);
  const maxConcurrent = useUploadStore((s) => s.maxConcurrent);
  const setMaxConcurrent = useUploadStore((s) => s.setMaxConcurrent);

  const pendingCount = useUploadStore(selectPendingCount);
  const uploadingCount = useUploadStore(selectUploadingCount);
  const queuedCount = useUploadStore(selectQueuedCount);
  const finishedCount = useUploadStore(selectFinishedCount);
  const hasActiveUploads = useUploadStore(selectHasActiveUploads);

  // Uploads are started by the store's queue; make sure it has its runner
  useEffect(() => {
    installUploadRunner();
  }, []);

  if (tasks.length === 0) return null;

  return (
    <div style={{ marginTop: 16 }}>
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'center',
          marginBottom: 8,
        }}
      >
        <Text strong>{tasks.length} file(s) selected</Text>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <Text type="secondary" style={{ fontSize: 12 }}>
            Parallel
          </Text>
          <Select
            size="small"
            value={maxConcurrent}
            options={CONCURRENCY_OPTIONS}
            onChange={setMaxConcurrent}
            style={{ width: 64 }}
            aria-label="Files uploaded in parallel"
          />
          {finishedCount > 0 && !hasActiveUploads && (
            <Button type="link" size="small" onClick={clearFinished}>
              Clear finished
            </Button>
          )}
        </div>
      </div>

      <div style={{ maxHeight: 200, overflow: 'auto' }}>
        {tasks.map((task) => (
          <UploadTaskItem key={task.id} task={task} />
        ))}
      </div>

      <div style={{ marginTop: 16, display: 'flex', gap: 8 }}>
        {hasActiveUploads ? (
          <Button disabled icon={<StopOutlined />} block>
            Uploading... ({uploadingCount} active{queuedCount > 0 ? `, ${queuedCount} queued` : ''})
          </Button>
        ) : (
          <Button type="primary" onClick={startAllPending} disabled={pendingCount === 0} block>
            Upload {pendingCount} file(s)
          </Button>
        )}
      </div>
    </div>
  );
}
