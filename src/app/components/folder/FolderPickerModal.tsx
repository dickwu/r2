'use client';

import { useTranslation } from 'react-i18next';
import { useState } from 'react';
import { Modal } from 'antd';
import { useAntdModalFocusable } from '@/app/hooks/useAntdModalFocusable';
import FolderTreePicker from '@/app/components/folder/FolderTreePicker';

export interface FolderPickerModalProps {
  open: boolean;
  onClose: () => void;
  selectedPath: string;
  onConfirm: (path: string) => void;
  title?: string;
}

export default function FolderPickerModal({
  open,
  onClose,
  selectedPath,
  onConfirm,
  title,
}: FolderPickerModalProps) {
  const [tempPath, setTempPath] = useState(selectedPath);
  const focusable = useAntdModalFocusable();
  const { t } = useTranslation();

  const handleOk = () => {
    onConfirm(tempPath);
    onClose();
  };

  const handleCancel = () => {
    setTempPath(selectedPath); // Reset to original
    onClose();
  };

  return (
    <Modal
      title={title ?? t('folderPicker.title')}
      open={open}
      onOk={handleOk}
      onCancel={handleCancel}
      okText={t('common.select')}
      cancelText={t('common.cancel')}
      width="90%"
      style={{ top: '3%' }}
      destroyOnHidden
      focusable={focusable}
    >
      <div style={{ height: '70vh', overflow: 'auto' }} className="select-none">
        <FolderTreePicker selectedPath={tempPath} onSelect={setTempPath} />
      </div>
    </Modal>
  );
}
