'use client';

import { DatabaseOutlined, FolderOutlined } from '@ant-design/icons';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';

interface CrumbsProps {
  bucket: string;
  path: string;
  onNavigate: (newPath: string) => void;
}

export default function Crumbs({ bucket, path, onNavigate }: CrumbsProps) {
  const parts = path.split('/').filter(Boolean);

  return (
    <div className="crumbs">
      <button
        className={['crumb', parts.length === 0 && 'current'].filter(Boolean).join(' ')}
        onClick={() => onNavigate('')}
        title={bucket}
      >
        <DatabaseOutlined className="crumb-icon" style={{ fontSize: 13 }} />
        <span>{bucket}</span>
      </button>
      {parts.map((part, i) => {
        const isLast = i === parts.length - 1;
        const upTo = parts.slice(0, i + 1).join('/') + '/';
        return (
          <span key={i} style={{ display: 'inline-flex', alignItems: 'center', gap: 2 }}>
            <span className="crumb-sep">/</span>
            <button
              className={['crumb', isLast && 'current'].filter(Boolean).join(' ')}
              onClick={() => onNavigate(upTo)}
              title={part}
            >
              {isLast && <FolderOutlined className="crumb-icon" style={{ fontSize: 13 }} />}
              <span>{part}</span>
            </button>
          </span>
        );
      })}
    </div>
  );
}

/**
 * Connected version that reads path from currentPathStore automatically.
 * Use this when you don't want to thread path as a prop.
 */
export function ConnectedCrumbs({
  bucket,
  onNavigate,
}: {
  bucket: string;
  onNavigate: (newPath: string) => void;
}) {
  const currentPath = useCurrentPathStore((s) => s.currentPath);
  return <Crumbs bucket={bucket} path={currentPath} onNavigate={onNavigate} />;
}
