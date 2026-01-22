import { HomeOutlined } from '@ant-design/icons';
import { Breadcrumb } from 'antd';
import { useCallback, useMemo } from 'react';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';

interface PathBreadcrumbProps {
  bucketName?: string;
  onNavigate?: (path: string) => void;
}

export default function PathBreadcrumb({ bucketName = 'Root', onNavigate }: PathBreadcrumbProps) {
  const currentPath = useCurrentPathStore((state) => state.currentPath);
  const setCurrentPath = useCurrentPathStore((state) => state.setCurrentPath);

  const pathParts = useMemo(
    () => (currentPath ? currentPath.replace(/\/$/, '').split('/') : []),
    [currentPath]
  );

  const handleNavigate = useCallback(
    (path: string) => {
      setCurrentPath(path);
      onNavigate?.(path);
    },
    [onNavigate, setCurrentPath]
  );

  const breadcrumbItems = useMemo(
    () => [
      {
        title: (
          <a onClick={() => handleNavigate('')}>
            <HomeOutlined /> {bucketName}
          </a>
        ),
      },
      ...pathParts.map((part, index) => {
        const fullPath = pathParts.slice(0, index + 1).join('/') + '/';
        return {
          title: <a onClick={() => handleNavigate(fullPath)}>{part}</a>,
        };
      }),
    ],
    [bucketName, handleNavigate, pathParts]
  );

  return <Breadcrumb items={breadcrumbItems} />;
}
