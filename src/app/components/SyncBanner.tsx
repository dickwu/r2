'use client';

import { useEffect, useMemo, useRef, useState } from 'react';
import {
  CloudDownloadOutlined,
  DatabaseOutlined,
  BuildOutlined,
  CheckCircleOutlined,
  WarningOutlined,
} from '@ant-design/icons';
import { useSyncStore, type SyncPhase } from '@/app/stores/syncStore';

// How long the banner lingers after sync ends, showing the "Synced" state
// before it auto-hides. Matches the TransferDock auto-dismiss for consistency.
const COMPLETION_LINGER_MS = 2500;

// Rolling window for files/sec calculation on legacy counters.
const RATE_WINDOW_MS = 2500;

interface SyncBannerProps {
  /** Optional click handler to expand into the full SyncOverlay modal. */
  onShowDetails?: () => void;
}

/**
 * Rolling rate (units/sec) of a monotonically-increasing counter, sampled
 * over a fixed time window. Returns 0 until two samples exist or the window
 * has accumulated enough delta to be meaningful.
 */
function useRollingRate(counter: number, windowMs = RATE_WINDOW_MS): number {
  const samplesRef = useRef<{ value: number; time: number }[]>([]);
  const now = Date.now();
  const samples = samplesRef.current;

  if (samples.length === 0 || samples[samples.length - 1].value !== counter) {
    samples.push({ value: counter, time: now });
  }
  // Trim samples outside the window — keep at least the most recent one.
  const cutoff = now - windowMs;
  while (samples.length > 1 && samples[0].time < cutoff) {
    samples.shift();
  }

  if (samples.length < 2) return 0;
  const oldest = samples[0];
  const newest = samples[samples.length - 1];
  const dt = (newest.time - oldest.time) / 1000;
  if (dt <= 0) return 0;
  return Math.max(0, (newest.value - oldest.value) / dt);
}

/**
 * Floating banner that auto-shows whenever any sync pipeline is running.
 * Watches both the new background-sync state and the legacy phase-based
 * state. Reports throughput (files/sec) in the detail line so the user
 * sees real progress even when the total is unknown.
 */
export default function SyncBanner({ onShowDetails }: SyncBannerProps) {
  // Legacy phase-based sync (blocking full sync from older code paths).
  const isSyncing = useSyncStore((s) => s.isSyncing);
  const phase = useSyncStore((s) => s.phase);
  const processedFiles = useSyncStore((s) => s.processedFiles);
  const storedFiles = useSyncStore((s) => s.storedFiles);
  const totalFiles = useSyncStore((s) => s.totalFiles);
  const indexingProgress = useSyncStore((s) => s.indexingProgress);

  // New non-blocking background sync (current default pipeline).
  const backgroundSync = useSyncStore((s) => s.backgroundSync);

  const phaseActive = phase !== 'idle' && phase !== 'complete';
  const bgActive = backgroundSync.isRunning;
  const bgError = !!backgroundSync.error;
  const active = isSyncing || phaseActive || bgActive || bgError;

  // Rate tracking — pick the right counter for the active phase.
  // For background sync, the backend already publishes `speed` so we don't
  // need to recompute it.
  const legacyCounter =
    phase === 'storing'
      ? storedFiles
      : phase === 'indexing'
        ? indexingProgress.current
        : processedFiles;
  const legacyRate = useRollingRate(legacyCounter);
  const bgRate = useRollingRate(backgroundSync.objectsFetched);
  // Prefer the backend-reported speed; fall back to our rolling estimate
  // when the backend is silent (it sometimes reports 0 between events).
  const effectiveBgRate = backgroundSync.speed > 0 ? backgroundSync.speed : bgRate;

  // Linger briefly after sync ends so the user sees a "Synced" confirmation
  // before the banner auto-hides. Restarts the linger window each time a new
  // sync run finishes.
  const [lingering, setLingering] = useState(false);
  const wasActiveRef = useRef(false);
  const lingerTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (active) {
      wasActiveRef.current = true;
      if (lingerTimerRef.current) {
        clearTimeout(lingerTimerRef.current);
        lingerTimerRef.current = null;
      }
      setLingering(false);
      return;
    }
    if (wasActiveRef.current) {
      wasActiveRef.current = false;
      setLingering(true);
      lingerTimerRef.current = setTimeout(() => {
        setLingering(false);
        lingerTimerRef.current = null;
      }, COMPLETION_LINGER_MS);
    }
  }, [active]);

  useEffect(() => {
    return () => {
      if (lingerTimerRef.current) clearTimeout(lingerTimerRef.current);
    };
  }, []);

  const visible = active || lingering;

  const view = useMemo(
    () =>
      computeBannerView({
        phase,
        processedFiles,
        storedFiles,
        totalFiles,
        indexingProgress,
        backgroundSync,
        legacyActive: isSyncing || phaseActive,
        lingering: !active && lingering,
        legacyRate,
        bgRate: effectiveBgRate,
      }),
    [
      phase,
      processedFiles,
      storedFiles,
      totalFiles,
      indexingProgress,
      backgroundSync,
      isSyncing,
      phaseActive,
      active,
      lingering,
      legacyRate,
      effectiveBgRate,
    ]
  );

  if (!visible) return null;

  const isSynced = !active && lingering && view.tone !== 'error';
  const bannerClass = ['sync-banner', view.tone === 'error' && 'error', isSynced && 'synced']
    .filter(Boolean)
    .join(' ');

  // No progress bar — count + rate in the detail line are the signal. The
  // bar was effectively only ever showing 0% or 100% in practice because
  // the backend reports `estimatedTotal` late and then jumps once fetch
  // completes.
  return (
    <div className={bannerClass} role="status" aria-live="polite">
      <span className="sync-banner-dot" />
      <span className="sync-banner-icon">{view.icon}</span>
      <span className="sync-banner-text">
        <span className="sync-banner-phase">{view.title}</span>
        {view.detail && <span className="sync-banner-detail">{view.detail}</span>}
      </span>

      {onShowDetails && (
        <button
          type="button"
          className="sync-banner-link"
          onClick={onShowDetails}
          title="Open full sync details"
        >
          Details
        </button>
      )}
    </div>
  );
}

/* ── view derivation ─────────────────────────────────────────────── */
interface BannerView {
  title: string;
  detail: string | null;
  icon: React.ReactNode;
  tone: 'progress' | 'error';
}

function formatRate(ratePerSec: number): string {
  if (!isFinite(ratePerSec) || ratePerSec <= 0) return '';
  if (ratePerSec >= 100) return `${Math.round(ratePerSec).toLocaleString()}/s`;
  if (ratePerSec >= 10) return `${ratePerSec.toFixed(0)}/s`;
  return `${ratePerSec.toFixed(1)}/s`;
}

function withRate(label: string, rate: number): string {
  const r = formatRate(rate);
  return r ? `${label} · ${r}` : label;
}

function computeBannerView(args: {
  phase: SyncPhase;
  processedFiles: number;
  storedFiles: number;
  totalFiles: number;
  indexingProgress: { current: number; total: number };
  backgroundSync: {
    isRunning: boolean;
    objectsFetched: number;
    estimatedTotal: number | null;
    error: string | null;
  };
  legacyActive: boolean;
  lingering: boolean;
  legacyRate: number;
  bgRate: number;
}): BannerView {
  const {
    phase,
    processedFiles,
    storedFiles,
    totalFiles,
    indexingProgress,
    backgroundSync,
    legacyActive,
    lingering,
    legacyRate,
    bgRate,
  } = args;

  // Linger view — shown briefly after sync finishes before the banner hides.
  if (lingering) {
    const fetched = backgroundSync.objectsFetched;
    return {
      title: 'Synced',
      detail: fetched > 0 ? `${fetched.toLocaleString()} objects up to date` : 'Bucket up to date',
      icon: <CheckCircleOutlined />,
      tone: 'progress',
    };
  }

  // Error trumps everything.
  if (backgroundSync.error) {
    return {
      title: 'Sync failed',
      detail: backgroundSync.error,
      icon: <WarningOutlined />,
      tone: 'error',
    };
  }

  // Legacy phase-based sync wins when active because it carries richer state.
  if (legacyActive) {
    if (phase === 'fetching') {
      const label =
        processedFiles > 0 ? `${processedFiles.toLocaleString()} found` : 'Listing bucket…';
      return {
        title: 'Fetching files',
        detail: withRate(label, legacyRate),
        icon: <CloudDownloadOutlined />,
        tone: 'progress',
      };
    }
    if (phase === 'storing') {
      const total = totalFiles > 0 ? totalFiles : processedFiles;
      const label =
        total > 0
          ? `${storedFiles.toLocaleString()} / ${total.toLocaleString()}`
          : `${storedFiles.toLocaleString()} cached`;
      return {
        title: 'Caching',
        detail: withRate(label, legacyRate),
        icon: <DatabaseOutlined />,
        tone: 'progress',
      };
    }
    if (phase === 'indexing') {
      const { current, total } = indexingProgress;
      const label =
        total > 0
          ? `${current.toLocaleString()} / ${total.toLocaleString()} folders`
          : `${current.toLocaleString()} folders`;
      return {
        title: 'Indexing',
        detail: withRate(label, legacyRate),
        icon: <BuildOutlined />,
        tone: 'progress',
      };
    }
    if (phase === 'complete') {
      return {
        title: 'Up to date',
        detail: null,
        icon: <CheckCircleOutlined />,
        tone: 'progress',
      };
    }
    return {
      title: 'Preparing sync',
      detail: null,
      icon: <CloudDownloadOutlined />,
      tone: 'progress',
    };
  }

  // Background-sync pipeline.
  const fetched = backgroundSync.objectsFetched;
  const est = backgroundSync.estimatedTotal;

  // Fetch has reached the estimated total but the backend pipeline is still
  // wrapping up (writing the cache, building the directory tree, etc). Show
  // a clean "Finalizing" state with no rate, since the rate would echo the
  // last fetch sample and read misleadingly.
  if (est && est > 0 && fetched >= est) {
    return {
      title: 'Finalizing sync',
      detail: `${fetched.toLocaleString()} objects ready`,
      icon: <DatabaseOutlined />,
      tone: 'progress',
    };
  }

  if (est && est > 0) {
    const label = `${fetched.toLocaleString()} / ~${est.toLocaleString()} objects`;
    return {
      title: 'Syncing bucket',
      detail: withRate(label, bgRate),
      icon: <CloudDownloadOutlined />,
      tone: 'progress',
    };
  }
  const label = fetched > 0 ? `${fetched.toLocaleString()} objects fetched` : 'Listing objects…';
  return {
    title: 'Syncing bucket',
    detail: withRate(label, bgRate),
    icon: <CloudDownloadOutlined />,
    tone: 'progress',
  };
}
