'use client';

import { useTranslation } from 'react-i18next';
import { useCallback, useEffect, useState } from 'react';
import { App } from 'antd';
import { DeleteOutlined, ReloadOutlined } from '@ant-design/icons';
import { formatBytes } from '@/app/utils/formatBytes';
import { clearThumbnailMemory } from '@/app/hooks/useThumbnail';
import {
  cacheUsagePercent,
  clearThumbnailCache,
  getThumbnailCacheStats,
  setThumbnailCacheLimit,
  THUMBNAIL_LIMIT_OPTIONS,
  type ThumbnailCacheStats,
} from '@/app/lib/thumbnailCacheApi';

/**
 * Settings → Storage: the on-disk thumbnail cache. Shows what it holds against
 * its limit, lets the limit change (older thumbnails are evicted to fit) and
 * empties it on request.
 */
export default function SettingsStoragePanel() {
  const { message, modal } = App.useApp();
  const { t } = useTranslation();
  const [stats, setStats] = useState<ThumbnailCacheStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStats(await getThumbnailCacheStats());
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    getThumbnailCacheStats()
      .then((next) => {
        if (cancelled) return;
        setStats(next);
        setError(null);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const changeLimit = async (bytes: number) => {
    if (busy || stats?.maxBytes === bytes) return;
    setBusy(true);
    try {
      setStats(await setThumbnailCacheLimit(bytes));
    } catch (e) {
      message.error(t('storage.changeLimitFailed', { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const confirmClear = () => {
    const held = stats
      ? t('storage.heldThumbnails', {
          count: stats.count,
          formatted: stats.count.toLocaleString(),
          size: formatBytes(stats.bytes),
        })
      : t('storage.allThumbnails');
    modal.confirm({
      title: t('storage.clearConfirmTitle'),
      content: t('storage.clearConfirmContent', { held }),
      okText: t('storage.clearCache'),
      cancelText: t('common.cancel'),
      okButtonProps: { danger: true },
      onOk: async () => {
        setBusy(true);
        try {
          setStats(await clearThumbnailCache());
          clearThumbnailMemory();
          message.success(t('storage.cleared'));
        } catch (e) {
          message.error(t('storage.clearFailed', { error: String(e) }));
          throw e;
        } finally {
          setBusy(false);
        }
      },
    });
  };

  const percent = stats ? cacheUsagePercent(stats) : 0;

  return (
    <div className="settings-section-stack">
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>{t('storage.title')}</h3>
            <p>{t('storage.desc')}</p>
          </div>
          <div className="settings-mount-head-actions">
            <button
              className="btn btn-sm"
              onClick={() => void refresh()}
              disabled={busy}
              title={t('common.refresh')}
            >
              <ReloadOutlined />
            </button>
          </div>
        </div>

        <div className="thumb-cache-card">
          <div className="thumb-cache-figures">
            <div className="thumb-cache-figure">
              <strong data-testid="thumbnail-cache-size">
                {stats ? formatBytes(stats.bytes) : '—'}
              </strong>
              <span>{t('storage.ofUsed', { max: stats ? formatBytes(stats.maxBytes) : '—' })}</span>
            </div>
            <div className="thumb-cache-figure">
              <strong data-testid="thumbnail-cache-count">
                {stats ? stats.count.toLocaleString() : '—'}
              </strong>
              <span>{t('storage.thumbnailsNoun', { count: stats?.count ?? 0 })}</span>
            </div>
          </div>
          <div className="thumb-cache-meter" aria-hidden="true">
            <span style={{ width: `${percent}%` }} />
          </div>
          <meter className="sr-only" min={0} max={100} value={Math.round(percent)}>
            {t('storage.usageAria')}
          </meter>
          <div className="thumb-cache-foot">
            <span>{t('storage.evictionNote')}</span>
            <button
              className="btn btn-sm thumb-cache-clear"
              onClick={confirmClear}
              disabled={busy || !stats || stats.count === 0}
            >
              <DeleteOutlined /> {t('storage.clearThumbnailCache')}
            </button>
          </div>
          {error && <div className="thumb-cache-error">{error}</div>}
        </div>
      </section>

      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>{t('storage.limitTitle')}</h3>
            <p>
              {t('storage.limitDesc')}
              {stats
                ? ` ${t('storage.imageMaxNote', { size: formatBytes(stats.imageMaxBytes) })}`
                : ''}
            </p>
          </div>
        </div>
        <div className="option-row-list">
          {THUMBNAIL_LIMIT_OPTIONS.map((option, index) => (
            <button
              type="button"
              key={option.bytes}
              className={['option-row', stats?.maxBytes === option.bytes && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => void changeLimit(option.bytes)}
              disabled={busy}
              aria-label={option.label}
            >
              <span className="option-row-text">
                <strong>{option.label}</strong>
                <span>{t(`storage.limitSub.${index}`, { defaultValue: option.sub })}</span>
              </span>
              <span className="option-row-radio" />
            </button>
          ))}
        </div>
      </section>
    </div>
  );
}
