import { FolderOutlined, CloudOutlined, UploadOutlined, PlusOutlined } from '@ant-design/icons';
import { useThemeStore } from '@/app/stores/themeStore';

interface EmptyStateProps {
  search?: string;
  onUpload?: () => void;
  onNewFolder?: () => void;
}

function IllustratedArt() {
  return (
    <svg width="80" height="60" viewBox="0 0 80 60" fill="none">
      <rect
        x="6"
        y="14"
        width="68"
        height="40"
        rx="4"
        fill="var(--accent-soft)"
        stroke="var(--accent)"
        strokeWidth="1.5"
      />
      <path d="M6 22h68" stroke="var(--accent)" strokeWidth="1.5" />
      <circle cx="14" cy="18" r="1.5" fill="var(--accent)" />
      <circle cx="20" cy="18" r="1.5" fill="var(--accent)" />
    </svg>
  );
}

export default function EmptyState({ search, onUpload, onNewFolder }: EmptyStateProps) {
  const emptyStyle = useThemeStore((s) => s.emptyStyle);

  return (
    <div className={`empty empty-${emptyStyle}`}>
      <div className="empty-art">
        {emptyStyle === 'blueprint' && (
          <FolderOutlined style={{ fontSize: 32, color: 'var(--accent)' }} />
        )}
        {emptyStyle === 'illustrated' && <IllustratedArt />}
        {emptyStyle === 'minimal' && (
          <CloudOutlined style={{ fontSize: 28, color: 'var(--text-subtle)' }} />
        )}
      </div>

      <div className="empty-title">
        {search ? `No results for "${search}"` : 'This folder is empty'}
      </div>

      <div className="empty-desc">
        {search
          ? 'Try a different keyword, or expand search across the bucket.'
          : 'Drop files here or upload from your machine. Multipart resumable transfers handle files up to 5 GB out of the box.'}
      </div>

      {!search && (
        <div className="empty-actions">
          {onUpload && (
            <button className="btn btn-primary" onClick={onUpload}>
              <UploadOutlined style={{ fontSize: 13 }} /> Upload files
            </button>
          )}
          {onNewFolder && (
            <button className="btn" onClick={onNewFolder}>
              <PlusOutlined style={{ fontSize: 13 }} /> New folder
            </button>
          )}
        </div>
      )}
    </div>
  );
}
