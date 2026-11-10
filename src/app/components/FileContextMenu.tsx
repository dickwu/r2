import { Dropdown } from 'antd';
import { useTranslation } from 'react-i18next';
import type { MenuProps } from 'antd';
import { DownloadOutlined, EditOutlined, DeleteOutlined, CopyOutlined } from '@ant-design/icons';
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
  const { t } = useTranslation();
  const handleCopyPath = () => {
    navigator.clipboard.writeText(item.key);
  };

  const menuItems: MenuProps['items'] = item.isFolder
    ? [
        ...(onFolderDownload
          ? [
              {
                key: 'download',
                label: t('files.menu.downloadFolder'),
                icon: <DownloadOutlined />,
                onClick: () => onFolderDownload(item),
              },
            ]
          : []),
        ...(onFolderRename
          ? [
              {
                key: 'rename',
                label: t('files.menu.renameFolder'),
                icon: <EditOutlined />,
                onClick: () => onFolderRename(item),
              },
            ]
          : []),
        {
          key: 'copy-path',
          label: t('files.menu.copyPath'),
          icon: <CopyOutlined />,
          onClick: handleCopyPath,
        },
        { type: 'divider' as const },
        ...(onFolderDelete
          ? [
              {
                key: 'delete',
                label: t('files.menu.deleteFolder'),
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
                label: t('files.menu.download'),
                icon: <DownloadOutlined />,
                onClick: () => onDownload(item),
              },
            ]
          : []),
        {
          key: 'rename',
          label: t('files.menu.rename'),
          icon: <EditOutlined />,
          onClick: () => onRename(item),
        },
        {
          key: 'copy-path',
          label: t('files.menu.copyPath'),
          icon: <CopyOutlined />,
          onClick: handleCopyPath,
        },
        { type: 'divider' as const },
        {
          key: 'delete',
          label: t('files.menu.delete'),
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
