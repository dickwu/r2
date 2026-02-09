'use client';

import { useState, useEffect, useCallback, useMemo } from 'react';
import { Modal, App, Button, Select, Input, Checkbox } from 'antd';
import { FolderOutlined, SwapOutlined } from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import type { StorageConfig, StorageProvider } from '@/app/lib/r2cache';
import FolderPickerModal from '@/app/components/folder/FolderPickerModal';
import { useAccountStore } from '@/app/stores/accountStore';

interface DestinationBucket {
  name: string;
}

interface DestinationOption {
  id: string;
  provider: StorageProvider;
  accountId: string;
  accountLabel: string;
  tokenId?: number;
  tokenLabel?: string;
  accessKeyId: string;
  secretAccessKey: string;
  region?: string;
  endpointScheme?: string;
  endpointHost?: string;
  forcePathStyle?: boolean;
  buckets: DestinationBucket[];
}

interface BatchMoveModalProps {
  open: boolean;
  selectedKeys: Set<string>;
  config: StorageConfig | null | undefined;
  onClose: () => void;
  onSuccess: () => void;
  onMovingChange?: (isMoving: boolean) => void;
}

export default function BatchMoveModal({
  open,
  selectedKeys,
  config,
  onClose,
  onSuccess,
}: BatchMoveModalProps) {
  const [targetDirectory, setTargetDirectory] = useState('');
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [deleteOriginal, setDeleteOriginal] = useState(true);
  const [selectedDestinationId, setSelectedDestinationId] = useState<string | null>(null);
  const [selectedBucket, setSelectedBucket] = useState('');
  const [isStartingQueue, setIsStartingQueue] = useState(false);
  const accounts = useAccountStore((state) => state.accounts);
  const loadAccounts = useAccountStore((state) => state.loadAccounts);
  const { message } = App.useApp();

  const selectedCount = selectedKeys.size;

  // Reset state when modal opens/closes
  useEffect(() => {
    if (!open) {
      setTargetDirectory('');
      setDeleteOriginal(true);
      setSelectedDestinationId(null);
      setSelectedBucket('');
      setIsStartingQueue(false);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    loadAccounts().catch((e) => {
      console.error('Failed to load accounts:', e);
    });
  }, [open, loadAccounts]);

  const destinationOptions = useMemo<DestinationOption[]>(() => {
    const options: DestinationOption[] = [];
    accounts.forEach((account) => {
      if (account.provider === 'r2') {
        account.tokens.forEach((token) => {
          options.push({
            id: `r2:${account.account.id}:${token.token.id}`,
            provider: 'r2',
            accountId: account.account.id,
            accountLabel: account.account.name || account.account.id,
            tokenId: token.token.id,
            tokenLabel: token.token.name || `Token ${token.token.id}`,
            accessKeyId: token.token.access_key_id,
            secretAccessKey: token.token.secret_access_key,
            buckets: token.buckets.map((bucket) => ({ name: bucket.name })),
          });
        });
        return;
      }
      if (account.provider === 'aws') {
        options.push({
          id: `aws:${account.account.id}`,
          provider: 'aws',
          accountId: account.account.id,
          accountLabel: account.account.name || account.account.id,
          accessKeyId: account.account.access_key_id,
          secretAccessKey: account.account.secret_access_key,
          region: account.account.region,
          endpointScheme: account.account.endpoint_scheme,
          endpointHost: account.account.endpoint_host || undefined,
          forcePathStyle: account.account.force_path_style,
          buckets: account.buckets.map((bucket) => ({ name: bucket.name })),
        });
        return;
      }
      if (account.provider === 'minio') {
        options.push({
          id: `minio:${account.account.id}`,
          provider: 'minio',
          accountId: account.account.id,
          accountLabel: account.account.name || account.account.id,
          accessKeyId: account.account.access_key_id,
          secretAccessKey: account.account.secret_access_key,
          endpointScheme: account.account.endpoint_scheme,
          endpointHost: account.account.endpoint_host,
          forcePathStyle: account.account.force_path_style,
          buckets: account.buckets.map((bucket) => ({ name: bucket.name })),
        });
        return;
      }
      options.push({
        id: `rustfs:${account.account.id}`,
        provider: 'rustfs',
        accountId: account.account.id,
        accountLabel: account.account.name || account.account.id,
        accessKeyId: account.account.access_key_id,
        secretAccessKey: account.account.secret_access_key,
        endpointScheme: account.account.endpoint_scheme,
        endpointHost: account.account.endpoint_host,
        forcePathStyle: true,
        buckets: account.buckets.map((bucket) => ({ name: bucket.name })),
      });
    });
    return options;
  }, [accounts]);

  const selectedDestination = useMemo(
    () => destinationOptions.find((option) => option.id === selectedDestinationId) || null,
    [destinationOptions, selectedDestinationId]
  );

  useEffect(() => {
    if (!open || !config || destinationOptions.length === 0) return;

    const initialOption =
      destinationOptions.find((option) => {
        if (option.provider !== config.provider) return false;
        if (option.accountId !== config.accountId) return false;
        if (option.provider === 'r2') {
          return option.accessKeyId === config.accessKeyId;
        }
        return true;
      }) || null;

    if (initialOption) {
      setSelectedDestinationId(initialOption.id);
      setSelectedBucket(config.bucket);
    }
  }, [open, config, destinationOptions]);

  useEffect(() => {
    if (!selectedDestination) return;
    if (!selectedBucket || !selectedDestination.buckets.some((b) => b.name === selectedBucket)) {
      setSelectedBucket(selectedDestination.buckets[0]?.name || '');
    }
  }, [selectedDestination, selectedBucket]);

  const isSameDestination =
    !!config &&
    !!selectedDestination &&
    selectedDestination.provider === config.provider &&
    selectedDestination.accountId === config.accountId &&
    selectedBucket === config.bucket;

  const handleMove = useCallback(async () => {
    if (isStartingQueue) return;
    if (!config || selectedKeys.size === 0 || !selectedDestination || !selectedBucket) return;
    if (!config.accessKeyId || !config.secretAccessKey) {
      message.error('Source credentials are required to move files');
      return;
    }
    if (config.provider === 'aws' && !config.region) {
      message.error('AWS region is required to move files');
      return;
    }
    if (
      (config.provider === 'minio' || config.provider === 'rustfs') &&
      (!config.endpointScheme || !config.endpointHost)
    ) {
      message.error('Endpoint configuration is required to move files');
      return;
    }

    const keys = Array.from(selectedKeys);
    const operations = keys.map((key) => {
      const filename = key.split('/').pop() || key;
      const newPath = targetDirectory ? `${targetDirectory}/${filename}` : filename;
      return { source_key: key, dest_key: newPath };
    });

    const actionLabel = deleteOriginal ? 'Move' : 'Copy';
    setIsStartingQueue(true);
    message.loading({
      content: `Queuing ${operations.length} file${operations.length > 1 ? 's' : ''} for ${actionLabel.toLowerCase()}...`,
      key: 'batch-move',
    });

    try {
      await invoke('start_batch_move', {
        sourceConfig: {
          provider: config.provider,
          account_id: config.accountId,
          bucket: config.bucket,
          access_key_id: config.accessKeyId,
          secret_access_key: config.secretAccessKey,
          region: config.provider === 'aws' ? config.region : null,
          endpoint_scheme: config.provider === 'r2' ? null : config.endpointScheme,
          endpoint_host: config.provider === 'r2' ? null : config.endpointHost,
          force_path_style: config.provider === 'r2' ? null : config.forcePathStyle,
        },
        destConfig: {
          provider: selectedDestination.provider,
          account_id: selectedDestination.accountId,
          bucket: selectedBucket,
          access_key_id: selectedDestination.accessKeyId,
          secret_access_key: selectedDestination.secretAccessKey,
          region: selectedDestination.provider === 'aws' ? selectedDestination.region : null,
          endpoint_scheme:
            selectedDestination.provider === 'r2' ? null : selectedDestination.endpointScheme,
          endpoint_host:
            selectedDestination.provider === 'r2' ? null : selectedDestination.endpointHost,
          force_path_style:
            selectedDestination.provider === 'r2' ? null : selectedDestination.forcePathStyle,
        },
        operations,
        deleteOriginal,
      });

      message.success({
        content: `${actionLabel} started for ${operations.length} file${operations.length > 1 ? 's' : ''}`,
        key: 'batch-move',
      });
      onClose();
      onSuccess();
    } catch (e) {
      console.error('Batch move error:', e);
      message.error({
        content: `Failed to start move: ${e instanceof Error ? e.message : 'Unknown error'}`,
        key: 'batch-move',
      });
    } finally {
      setIsStartingQueue(false);
    }
  }, [
    config,
    selectedKeys,
    targetDirectory,
    selectedDestination,
    selectedBucket,
    deleteOriginal,
    isStartingQueue,
    message,
    onClose,
    onSuccess,
  ]);

  return (
    <>
      <Modal
        title="Move Files"
        open={open}
        onCancel={onClose}
        onOk={handleMove}
        okText="Move"
        okButtonProps={{
          disabled: !selectedDestination || !selectedBucket || isStartingQueue,
          loading: isStartingQueue,
        }}
        cancelButtonProps={{ disabled: isStartingQueue }}
        width={480}
        centered
      >
        <div>
          <p style={{ marginBottom: 16 }}>
            Move <strong>{selectedCount}</strong> file{selectedCount > 1 ? 's' : ''} to:
          </p>

          <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
            <div>
              <div style={{ fontSize: 12, color: '#666', marginBottom: 6 }}>
                Destination account
              </div>
              <Select
                value={selectedDestinationId ?? undefined}
                onChange={(value) => setSelectedDestinationId(value)}
                placeholder="Select destination account"
                style={{ width: '100%' }}
                options={destinationOptions.map((option) => ({
                  value: option.id,
                  label:
                    option.provider === 'r2'
                      ? `R2 · ${option.accountLabel} · ${option.tokenLabel}`
                      : `${option.provider.toUpperCase()} · ${option.accountLabel}`,
                }))}
              />
            </div>

            <div>
              <div style={{ fontSize: 12, color: '#666', marginBottom: 6 }}>Destination bucket</div>
              <Select
                value={selectedBucket || undefined}
                onChange={(value) => setSelectedBucket(value)}
                placeholder="Select destination bucket"
                style={{ width: '100%' }}
                disabled={!selectedDestination}
                options={(selectedDestination?.buckets || []).map((bucket) => ({
                  value: bucket.name,
                  label: bucket.name,
                }))}
              />
            </div>

            <div>
              <div style={{ fontSize: 12, color: '#666', marginBottom: 6 }}>Destination folder</div>
              {isSameDestination ? (
                <div
                  style={{
                    padding: '8px 12px',
                    background: 'var(--bg-secondary)',
                    borderRadius: 6,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    gap: 8,
                  }}
                >
                  <div
                    style={{ display: 'flex', alignItems: 'center', gap: 8, flex: 1, minWidth: 0 }}
                  >
                    <FolderOutlined style={{ color: 'var(--text-secondary)', flexShrink: 0 }} />
                    <span
                      style={{
                        fontFamily: 'monospace',
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        whiteSpace: 'nowrap',
                      }}
                    >
                      {targetDirectory ? `/${targetDirectory}/` : '/ (root)'}
                    </span>
                  </div>
                  <Button
                    size="small"
                    icon={<SwapOutlined />}
                    onClick={() => setFolderPickerOpen(true)}
                  >
                    Change...
                  </Button>
                </div>
              ) : (
                <Input
                  placeholder="Enter destination folder (optional)"
                  value={targetDirectory}
                  onChange={(event) => setTargetDirectory(event.target.value.replace(/^\/+/, ''))}
                  allowClear
                />
              )}
            </div>

            <Checkbox
              checked={deleteOriginal}
              onChange={(event) => setDeleteOriginal(event.target.checked)}
            >
              Delete original after move
            </Checkbox>
          </div>
        </div>
      </Modal>

      {/* Folder Picker Modal */}
      <FolderPickerModal
        open={folderPickerOpen}
        onClose={() => setFolderPickerOpen(false)}
        selectedPath={targetDirectory}
        onConfirm={setTargetDirectory}
        title="Select Target Folder"
      />
    </>
  );
}
