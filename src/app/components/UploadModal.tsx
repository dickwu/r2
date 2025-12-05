"use client";

import { Modal, Upload, Typography, App } from "antd";
import { InboxOutlined } from "@ant-design/icons";

const { Dragger } = Upload;
const { Text } = Typography;

interface UploadModalProps {
  open: boolean;
  onClose: () => void;
  currentPath: string;
}

export default function UploadModal({ open, onClose, currentPath }: UploadModalProps) {
  const { message } = App.useApp();

  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      title="Upload Files"
      width={500}
      centered
      destroyOnHidden
    >
      <div style={{ marginBottom: 16 }}>
        <Text type="secondary">
          Upload to: <Text code>{currentPath || "/"}</Text>
        </Text>
      </div>

      <Dragger
        multiple
        showUploadList
        style={{ padding: "20px 0" }}
        beforeUpload={() => {
          message.info("Upload functionality coming soon");
          return false;
        }}
      >
        <p className="ant-upload-drag-icon">
          <InboxOutlined style={{ color: "#f6821f", fontSize: 48 }} />
        </p>
        <p className="ant-upload-text">Click or drag files to upload</p>
        <p className="ant-upload-hint">
          Support single or multiple file upload
        </p>
      </Dragger>
    </Modal>
  );
}

