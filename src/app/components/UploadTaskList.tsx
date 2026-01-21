'use client';

import { Button, Typography } from 'antd';
import { StopOutlined } from '@ant-design/icons';
import {
  useUploadStore,
  selectPendingCount,
  selectUploadingCount,
  selectFinishedCount,
  selectHasActiveUploads,
} from '@/app/stores/uploadStore';
import UploadTaskItem from '@/app/components/UploadTaskItem';

const { Text } = Typography;

export default function UploadTaskList() {
  const tasks = useUploadStore((s) => s.tasks);
  const startAllPending = useUploadStore((s) => s.startAllPending);
  const clearFinished = useUploadStore((s) => s.clearFinished);

  const pendingCount = useUploadStore(selectPendingCount);
  const uploadingCount = useUploadStore(selectUploadingCount);
  const finishedCount = useUploadStore(selectFinishedCount);
  const hasActiveUploads = useUploadStore(selectHasActiveUploads);

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
        {finishedCount > 0 && !hasActiveUploads && (
          <Button type="link" size="small" onClick={clearFinished}>
            Clear finished
          </Button>
        )}
      </div>

      <div style={{ maxHeight: 200, overflow: 'auto' }}>
        {tasks.map((task) => (
          <UploadTaskItem key={task.id} task={task} />
        ))}
      </div>

      <div style={{ marginTop: 16, display: 'flex', gap: 8 }}>
        {hasActiveUploads ? (
          <Button disabled icon={<StopOutlined />} block>
            Uploading... ({uploadingCount} active)
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
