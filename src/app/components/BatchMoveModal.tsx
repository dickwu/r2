'use client';

import { useTranslation } from 'react-i18next';
import { useState, useEffect, useCallback, useMemo } from 'react';
import { App } from 'antd';
import { ArrowRightOutlined, SendOutlined } from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import type { StorageConfig, StorageProvider } from '@/app/lib/r2cache';
import FolderPickerModal from '@/app/components/folder/FolderPickerModal';
import { useAccountStore } from '@/app/stores/accountStore';
import { buildMoveOperations } from '@/app/utils/moveOperations';
import { loadAllActiveMoves } from '@/app/stores/moveStore';
import Modal from '@/app/components/ui/Modal';

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
  const { t } = useTranslation();

  const selectedCount = selectedKeys.size;

  useEffect(() => {
    loadAccounts().catch(() => {
      /* silently handle */
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
            tokenLabel: token.token.name || t('batchMove.tokenFallback', { id: token.token.id }),
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
        if (option.provider === 'r2') return option.accessKeyId === config.accessKeyId;
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
      message.error(t('batchMove.needSource'));
      return;
    }
    if (config.provider === 'aws' && !config.region) {
      message.error(t('batchMove.needRegion'));
      return;
    }
    if (
      (config.provider === 'minio' || config.provider === 'rustfs') &&
      (!config.endpointScheme || !config.endpointHost)
    ) {
      message.error(t('batchMove.needEndpoint'));
      return;
    }

    let operations: ReturnType<typeof buildMoveOperations>;
    try {
      operations = buildMoveOperations(selectedKeys, targetDirectory);
    } catch (error) {
      message.error(error instanceof Error ? error.message : String(error));
      return;
    }

    setIsStartingQueue(true);
    message.loading({
      content: deleteOriginal
        ? t('batchMove.queuingMove', { count: operations.length })
        : t('batchMove.queuingCopy', { count: operations.length }),
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

      await loadAllActiveMoves();
      message.success({
        content: deleteOriginal
          ? t('batchMove.moveStarted', { count: operations.length })
          : t('batchMove.copyStarted', { count: operations.length }),
        key: 'batch-move',
      });
      onClose();
      onSuccess();
    } catch (e) {
      message.error({
        content: t('batchMove.startFailed', {
          error: e instanceof Error ? e.message : t('common.unknownError'),
        }),
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

  const footer = (
    <>
      <span
        className="left"
        style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 12.5 }}
      >
        <button
          type="button"
          role="switch"
          aria-checked={deleteOriginal}
          aria-labelledby="batch-move-delete-original-label"
          className={deleteOriginal ? 'switch on' : 'switch'}
          onClick={() => setDeleteOriginal((v) => !v)}
        />
        <span
          id="batch-move-delete-original-label"
          style={{ cursor: 'pointer' }}
          onClick={() => setDeleteOriginal((v) => !v)}
        >
          {t('batchMove.deleteOriginal')}
        </span>
      </span>
      <button className="btn" onClick={onClose} disabled={isStartingQueue}>
        {t('common.cancel')}
      </button>
      <button
        className="btn btn-primary"
        onClick={handleMove}
        disabled={!selectedDestination || !selectedBucket || isStartingQueue}
      >
        {isStartingQueue ? (
          t('batchMove.starting')
        ) : (
          <>
            {deleteOriginal ? <ArrowRightOutlined /> : <SendOutlined />}{' '}
            {deleteOriginal ? t('batchMove.moveHere') : t('batchMove.copyHere')}
          </>
        )}
      </button>
    </>
  );

  return (
    <>
      <Modal
        open={open}
        onClose={onClose}
        title={
          deleteOriginal
            ? t('batchMove.titleMove', { count: selectedCount })
            : t('batchMove.titleCopy', { count: selectedCount })
        }
        subtitle={t('batchMove.subtitle')}
        icon={<ArrowRightOutlined style={{ fontSize: 18 }} />}
        width={640}
        footer={footer}
      >
        <p style={{ color: 'var(--text-secondary)', fontSize: 12 }}>{t('batchMove.preserved')}</p>
        {/* Destination account + bucket */}
        <div className="field">
          <div className="field-label">{t('batchMove.destAccount')}</div>
          <select
            className="select"
            value={selectedDestinationId ?? ''}
            onChange={(e) => setSelectedDestinationId(e.target.value || null)}
          >
            <option value="">{t('batchMove.selectAccount')}</option>
            {destinationOptions.map((option) => (
              <option key={option.id} value={option.id}>
                {option.provider === 'r2'
                  ? `R2 · ${option.accountLabel} · ${option.tokenLabel}`
                  : `${option.provider.toUpperCase()} · ${option.accountLabel}`}
              </option>
            ))}
          </select>
        </div>

        <div className="field">
          <div className="field-label">{t('batchMove.destBucket')}</div>
          <select
            className="select"
            value={selectedBucket}
            onChange={(e) => setSelectedBucket(e.target.value)}
            disabled={!selectedDestination}
          >
            <option value="">{t('batchMove.selectBucket')}</option>
            {(selectedDestination?.buckets || []).map((bucket) => (
              <option key={bucket.name} value={bucket.name}>
                {bucket.name}
              </option>
            ))}
          </select>
        </div>

        {/* Target folder */}
        <div className="field">
          <div className="field-label">{t('batchMove.targetFolder')}</div>
          {isSameDestination ? (
            <div
              style={{
                padding: '8px 12px',
                background: 'var(--bg-sunken)',
                borderRadius: 8,
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
                gap: 8,
              }}
            >
              <span
                style={{
                  fontFamily: 'var(--font-mono)',
                  fontSize: 12,
                  color: 'var(--text)',
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                  flex: 1,
                }}
              >
                {targetDirectory ? `/${targetDirectory}/` : t('batchMove.root')}
              </span>
              <button
                className="btn"
                style={{ height: 26, fontSize: 11.5, padding: '0 10px' }}
                onClick={() => setFolderPickerOpen(true)}
              >
                {t('batchMove.change')}
              </button>
            </div>
          ) : (
            <input
              className="input mono"
              placeholder={t('batchMove.folderPh')}
              value={targetDirectory}
              onChange={(e) => setTargetDirectory(e.target.value.replace(/^\/+/, ''))}
            />
          )}
          <div className="field-hint">{t('batchMove.streamHint')}</div>
        </div>
      </Modal>

      {folderPickerOpen && (
        <FolderPickerModal
          open={true}
          onClose={() => setFolderPickerOpen(false)}
          selectedPath={targetDirectory}
          onConfirm={setTargetDirectory}
          title={t('batchMove.selectFolder')}
        />
      )}
    </>
  );
}
