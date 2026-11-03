'use client';

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
    void refresh();
  }, [refresh]);

  const changeLimit = async (bytes: number) => {
    if (busy || stats?.maxBytes === bytes) return;
    setBusy(true);
    try {
      setStats(await setThumbnailCacheLimit(bytes));
    } catch (e) {
      message.error(`Could not change the limit: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const confirmClear = () => {
    const held = stats
      ? `${stats.count.toLocaleString()} thumbnails (${formatBytes(stats.bytes)})`
      : 'all thumbnails';
    modal.confirm({
      title: 'Clear thumbnail cache?',
      content: `Remove ${held} from this computer? Files in your buckets are not touched; thumbnails are made again as you browse.`,
      okText: 'Clear cache',
      okButtonProps: { danger: true },
      onOk: async () => {
        setBusy(true);
        try {
          setStats(await clearThumbnailCache());
          clearThumbnailMemory();
          message.success('Thumbnail cache cleared');
        } catch (e) {
          message.error(`Could not clear the cache: ${String(e)}`);
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
            <h3>Thumbnail cache</h3>
            <p>Small previews of images and videos, kept on this computer so folders open fast.</p>
          </div>
          <div className="settings-mount-head-actions">
            <button
              className="btn btn-sm"
              onClick={() => void refresh()}
              disabled={busy}
              title="Refresh"
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
              <span>of {stats ? formatBytes(stats.maxBytes) : '—'} used</span>
            </div>
            <div className="thumb-cache-figure">
              <strong data-testid="thumbnail-cache-count">
                {stats ? stats.count.toLocaleString() : '—'}
              </strong>
              <span>{stats?.count === 1 ? 'thumbnail' : 'thumbnails'}</span>
            </div>
          </div>
          <div
            className="thumb-cache-meter"
            role="meter"
            aria-label="Thumbnail cache usage"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(percent)}
          >
            <span style={{ width: `${percent}%` }} />
          </div>
          <div className="thumb-cache-foot">
            <span>
              Unused for 30 days, or least recently used past the limit, are removed automatically.
            </span>
            <button
              className="btn btn-sm thumb-cache-clear"
              onClick={confirmClear}
              disabled={busy || !stats || stats.count === 0}
            >
              <DeleteOutlined /> Clear thumbnail cache
            </button>
          </div>
          {error && <div className="thumb-cache-error">{error}</div>}
        </div>
      </section>

      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Cache size limit</h3>
            <p>
              Lowering it removes the least recently used thumbnails now.
              {stats
                ? ` Images over ${formatBytes(stats.imageMaxBytes)} are shown from the original.`
                : ''}
            </p>
          </div>
        </div>
        <div className="option-row-list">
          {THUMBNAIL_LIMIT_OPTIONS.map((option) => (
            <button
              key={option.bytes}
              className={['option-row', stats?.maxBytes === option.bytes && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => void changeLimit(option.bytes)}
              disabled={busy}
            >
              <span className="option-row-text">
                <strong>{option.label}</strong>
                <span>{option.sub}</span>
              </span>
              <span className="option-row-radio" />
            </button>
          ))}
        </div>
      </section>
    </div>
  );
}
