"use client";

import { useState, useEffect } from "react";
import { Store } from "@tauri-apps/plugin-store";
import { Form, Input, Button, Modal, App } from "antd";
import { CloudOutlined } from "@ant-design/icons";

export interface R2Config {
  accountId: string;
  token: string;
  bucket: string;
  publicDomain?: string;
}

interface ConfigModalProps {
  open: boolean;
  onClose: () => void;
  onSave: (config: R2Config) => void;
  initialConfig?: R2Config | null;
}

export default function ConfigModal({ open, onClose, onSave, initialConfig }: ConfigModalProps) {
  const [saving, setSaving] = useState(false);
  const [form] = Form.useForm<R2Config>();
  const { message } = App.useApp();

  useEffect(() => {
    if (open && initialConfig) {
      form.setFieldsValue(initialConfig);
    }
  }, [open, initialConfig, form]);

  async function handleSubmit(values: R2Config) {
    setSaving(true);
    try {
      const store = await Store.load("r2-config.json");
      await store.set("config", values);
      await store.save();
      message.success("Configuration saved");
      onSave(values);
      onClose();
    } catch (e) {
      console.error("Failed to save config:", e);
      message.error("Failed to save configuration");
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      width={400}
      centered
      destroyOnHidden
    >
      <div style={{ textAlign: "center", marginBottom: 24 }}>
        <CloudOutlined style={{ fontSize: 40, color: "#f6821f" }} />
        <h3 style={{ marginTop: 12, marginBottom: 4 }}>Cloudflare R2</h3>
      </div>

      <Form form={form} layout="vertical" onFinish={handleSubmit} autoComplete="off">
        <Form.Item
          label="Account ID"
          name="accountId"
          rules={[{ required: true, message: "Required" }]}
        >
          <Input placeholder="Cloudflare Account ID" />
        </Form.Item>

        <Form.Item
          label="API Token"
          name="token"
          rules={[{ required: true, message: "Required" }]}
          extra="R2 read/write token"
        >
          <Input.Password placeholder="API Token" />
        </Form.Item>

        <Form.Item
          label="Bucket"
          name="bucket"
          rules={[{ required: true, message: "Required" }]}
        >
          <Input placeholder="Bucket name" />
        </Form.Item>

        <Form.Item label="Public Domain" name="publicDomain">
          <Input placeholder="https://cdn.example.com (optional)" />
        </Form.Item>

        <Form.Item style={{ marginBottom: 0 }}>
          <Button type="primary" htmlType="submit" loading={saving} block>
            Save
          </Button>
        </Form.Item>
      </Form>
    </Modal>
  );
}
