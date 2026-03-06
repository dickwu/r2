import { Dropdown } from 'antd';
import type { MenuProps } from 'antd';
import {
  DownloadOutlined,
  EditOutlined,
  DeleteOutlined,
  CopyOutlined,
  FolderOutlined,
} from '@ant-design/icons';
import { FileItem } from '@/app/hooks/useR2Files';

interface FileContextMenuProps {
  children: React.ReactNode;
  item: FileItem;
  onDownload?: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
}

export default function FileContextMenu({
  children,
  item,
  onDownload,
  onRename,
  onDelete,
  onFolderDownload,
  onFolderRename,
  onFolderDelete,
}: FileContextMenuProps) {
  const handleCopyPath = () => {
    navigator.clipboard.writeText(item.key);
  };

  const menuItems: MenuProps['items'] = item.isFolder
    ? [
        ...(onFolderDownload
          ? [
              {
                key: 'download',
                label: 'Download Folder',
                icon: <DownloadOutlined />,
                onClick: () => onFolderDownload(item),
              },
            ]
          : []),
        ...(onFolderRename
          ? [
              {
                key: 'rename',
                label: 'Rename Folder',
                icon: <EditOutlined />,
                onClick: () => onFolderRename(item),
              },
            ]
          : []),
        {
          key: 'copy-path',
          label: 'Copy Path',
          icon: <CopyOutlined />,
          onClick: handleCopyPath,
        },
        { type: 'divider' as const },
        ...(onFolderDelete
          ? [
              {
                key: 'delete',
                label: 'Delete Folder',
                icon: <DeleteOutlined />,
                danger: true,
                onClick: () => onFolderDelete(item),
              },
            ]
          : []),
      ]
    : [
        ...(onDownload
          ? [
              {
                key: 'download',
                label: 'Download',
                icon: <DownloadOutlined />,
                onClick: () => onDownload(item),
              },
            ]
          : []),
        {
          key: 'rename',
          label: 'Rename',
          icon: <EditOutlined />,
          onClick: () => onRename(item),
        },
        {
          key: 'copy-path',
          label: 'Copy Path',
          icon: <CopyOutlined />,
          onClick: handleCopyPath,
        },
        { type: 'divider' as const },
        {
          key: 'delete',
          label: 'Delete',
          icon: <DeleteOutlined />,
          danger: true,
          onClick: () => onDelete(item),
        },
      ];

  return (
    <Dropdown menu={{ items: menuItems }} trigger={['contextMenu']}>
      {children}
    </Dropdown>
  );
}
