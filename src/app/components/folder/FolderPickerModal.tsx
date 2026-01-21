'use client';

import { useState, useEffect } from 'react';
import { Modal } from 'antd';
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
  title = 'Select Folder',
}: FolderPickerModalProps) {
  const [tempPath, setTempPath] = useState(selectedPath);

  // Sync temp path when modal opens or selectedPath changes
  useEffect(() => {
    if (open) {
      setTempPath(selectedPath);
    }
  }, [open, selectedPath]);

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
      title={title}
      open={open}
      onOk={handleOk}
      onCancel={handleCancel}
      okText="Select"
      cancelText="Cancel"
      width="90%"
      style={{ top: '3%' }}
      destroyOnHidden
    >
      <div style={{ height: '70vh', overflow: 'auto' }} className="select-none">
        <FolderTreePicker selectedPath={tempPath} onSelect={setTempPath} />
      </div>
    </Modal>
  );
}
