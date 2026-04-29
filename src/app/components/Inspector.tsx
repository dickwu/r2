import {
  CloseOutlined,
  DownloadOutlined,
  LinkOutlined,
  MoreOutlined,
  FolderOutlined,
  FileImageOutlined,
  VideoCameraOutlined,
  CodeOutlined,
  FileTextOutlined,
  FileZipOutlined,
  FileOutlined,
} from '@ant-design/icons';
import { FileItem } from '@/app/hooks/useR2Files';
import { formatBytes } from '@/app/utils/formatBytes';
import dayjs from 'dayjs';

interface InspectorProps {
  item: FileItem;
  bucket: string;
  path: string;
  onClose: () => void;
  onDownload?: (item: FileItem) => void;
}

function itemType(item: FileItem): string {
  if (item.isFolder) return 'folder';
  const ext = item.name.toLowerCase().split('.').pop() || '';
  if (['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'].includes(ext)) return 'image';
  if (['mp4', 'avi', 'mkv', 'mov', 'wmv', 'flv', 'webm', 'm4v'].includes(ext)) return 'video';
  if (
    [
      'js',
      'jsx',
      'ts',
      'tsx',
      'py',
      'java',
      'c',
      'cpp',
      'h',
      'cs',
      'php',
      'rb',
      'go',
      'rs',
      'swift',
      'kt',
      'html',
      'htm',
      'xml',
      'json',
      'yaml',
      'yml',
      'toml',
      'ini',
      'conf',
    ].includes(ext)
  )
    return 'code';
  if (
    [
      'pdf',
      'doc',
      'docx',
      'xls',
      'xlsx',
      'csv',
      'ppt',
      'pptx',
      'txt',
      'log',
      'md',
      'markdown',
    ].includes(ext)
  )
    return 'doc';
  if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz'].includes(ext)) return 'archive';
  return 'file';
}

function ThumbIcon({ type, size = 42 }: { type: string; size?: number }) {
  const style = { fontSize: size };
  switch (type) {
    case 'folder':
      return <FolderOutlined style={style} />;
    case 'image':
      return <FileImageOutlined style={style} />;
    case 'video':
      return <VideoCameraOutlined style={style} />;
    case 'code':
      return <CodeOutlined style={style} />;
    case 'doc':
      return <FileTextOutlined style={style} />;
    case 'archive':
      return <FileZipOutlined style={style} />;
    default:
      return <FileOutlined style={style} />;
  }
}

function thumbBg(type: string): string {
  switch (type) {
    case 'folder':
      return 'linear-gradient(135deg, var(--accent-soft), transparent)';
    case 'image':
      return 'linear-gradient(135deg, rgba(120,170,90,0.18), rgba(120,170,90,0.04))';
    case 'video':
      return 'linear-gradient(135deg, rgba(173,90,170,0.18), rgba(173,90,170,0.04))';
    case 'code':
      return 'linear-gradient(135deg, rgba(70,130,200,0.18), rgba(70,130,200,0.04))';
    default:
      return 'var(--bg-sunken)';
  }
}

function KV({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="kv">
      <span className="kv-k">{label}</span>
      <span className="kv-v">{value}</span>
    </div>
  );
}

export default function Inspector({ item, bucket, path, onClose, onDownload }: InspectorProps) {
  const type = itemType(item);

  const sizeLabel = item.isFolder ? '—' : formatBytes(item.size || 0);
  const modLabel = item.lastModified ? dayjs(item.lastModified).format('YYYY-MM-DD HH:mm') : '—';

  return (
    <div className="inspector">
      <div className="inspector-header">
        <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 8 }}>
          <button
            className="fl-act-btn"
            title="Close inspector"
            onClick={onClose}
            style={{ width: 24, height: 24 }}
          >
            <CloseOutlined style={{ fontSize: 12 }} />
          </button>
        </div>

        <div className="inspector-thumb" style={{ background: thumbBg(type) }}>
          <ThumbIcon type={type} />
        </div>

        <div className="inspector-name">{item.name}</div>
        <div className="inspector-path">
          {bucket}/{path}
          {item.name}
        </div>
      </div>

      <div className="inspector-body">
        {/* Action buttons */}
        <div style={{ display: 'flex', gap: 6, marginBottom: 14 }}>
          {onDownload && !item.isFolder && (
            <button className="btn btn-sm" onClick={() => onDownload(item)}>
              <DownloadOutlined style={{ fontSize: 12 }} /> Download
            </button>
          )}
          <button className="btn btn-sm">
            <LinkOutlined style={{ fontSize: 12 }} /> Copy URL
          </button>
          <button className="btn btn-sm" style={{ padding: '0 8px' }}>
            <MoreOutlined style={{ fontSize: 12 }} />
          </button>
        </div>

        {/* KV pairs */}
        <KV label="Size" value={sizeLabel} />
        <KV label="Modified" value={modLabel} />
        <KV label="Type" value={type} />
        {item.isFolder && <KV label="Items" value="—" />}
        {!item.isFolder && (
          <>
            <KV label="ETag" value="—" />
            <KV label="Storage class" value="STANDARD" />
            <KV label="Encryption" value="AES-256" />
            <KV label="Visibility" value="private" />
          </>
        )}
      </div>
    </div>
  );
}
