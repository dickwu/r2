"use client";

import { Modal, Button, Space, Typography, App, Image } from "antd";
import { LinkOutlined, CopyOutlined, FileOutlined, FileImageOutlined, PlaySquareOutlined } from "@ant-design/icons";
import { openUrl } from "@tauri-apps/plugin-opener";
import { FileItem } from "../hooks/useR2Files";

const { Text, Paragraph } = Typography;

const IMAGE_EXTENSIONS = ["jpg", "jpeg", "png", "gif", "webp", "svg", "bmp", "ico"];
const VIDEO_EXTENSIONS = ["mp4", "webm", "ogg", "mov", "m4v"];

function getFileExtension(filename: string): string {
  return filename.split(".").pop()?.toLowerCase() || "";
}

function isImageFile(filename: string): boolean {
  return IMAGE_EXTENSIONS.includes(getFileExtension(filename));
}

function isVideoFile(filename: string): boolean {
  return VIDEO_EXTENSIONS.includes(getFileExtension(filename));
}

interface FilePreviewModalProps {
  open: boolean;
  onClose: () => void;
  file: FileItem | null;
  publicDomain?: string;
}

export default function FilePreviewModal({
  open: isOpen,
  onClose,
  file,
  publicDomain,
}: FilePreviewModalProps) {
  const { message } = App.useApp();

  if (!file) return null;

  const fileUrl = publicDomain
    ? `${publicDomain.replace(/\/$/, "")}/${file.key}`
    : null;

  const isImage = isImageFile(file.name);
  const isVideo = isVideoFile(file.name);

  async function handleOpenUrl() {
    if (!fileUrl) {
      message.warning("No public domain configured");
      return;
    }
    try {
      await openUrl(fileUrl);
    } catch (e) {
      console.error("Failed to open URL:", e);
      message.error("Failed to open URL");
    }
  }

  async function handleCopyUrl() {
    if (!fileUrl) {
      message.warning("No public domain configured");
      return;
    }
    try {
      await navigator.clipboard.writeText(fileUrl);
      message.success("URL copied to clipboard");
    } catch (e) {
      console.error("Failed to copy URL:", e);
      message.error("Failed to copy URL");
    }
  }

  return (
    <Modal
      open={isOpen}
      onCancel={onClose}
      title={file.name}
      footer={null}
      width={480}
      centered
      destroyOnHidden
    >
      <div style={{ textAlign: "center", marginBottom: 24 }}>
        {isImage && fileUrl ? (
          <Image
            src={fileUrl}
            alt={file.name}
            style={{ maxHeight: 300, objectFit: "contain" }}
          />
        ) : isVideo && fileUrl ? (
          <video
            src={fileUrl}
            controls
            style={{ maxWidth: "100%", maxHeight: 300, borderRadius: 8 }}
          />
        ) : isImage ? (
          <FileImageOutlined style={{ fontSize: 40, color: "#f6821f" }} />
        ) : isVideo ? (
          <PlaySquareOutlined style={{ fontSize: 40, color: "#f6821f" }} />
        ) : (
          <FileOutlined style={{ fontSize: 40, color: "#f6821f" }} />
        )}
      </div>

      {fileUrl ? (
        <Paragraph
          style={{
            background: "var(--ant-color-fill-tertiary)",
            padding: "12px 16px",
            borderRadius: 8,
            marginBottom: 24,
            wordBreak: "break-all",
          }}
        >
          <Text copyable={{ text: fileUrl }}>{fileUrl}</Text>
        </Paragraph>
      ) : (
        <Paragraph
          type="secondary"
          style={{ textAlign: "center", marginBottom: 24 }}
        >
          Configure a public domain in settings to get file URLs
        </Paragraph>
      )}

      <Space style={{ width: "100%", justifyContent: "center" }}>
        <Button
          icon={<LinkOutlined />}
          onClick={handleOpenUrl}
          disabled={!fileUrl}
        >
          Open URL
        </Button>
        <Button
          type="primary"
          icon={<CopyOutlined />}
          onClick={handleCopyUrl}
          disabled={!fileUrl}
        >
          Copy URL
        </Button>
      </Space>
    </Modal>
  );
}
