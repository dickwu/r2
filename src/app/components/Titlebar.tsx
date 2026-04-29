'use client';

import React from 'react';
import { SearchOutlined } from '@ant-design/icons';
import { useAccountStore } from '@/app/stores/accountStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';

interface TitlebarProps {
  /**
   * Triggered when the user clicks the "Search anything ⌘K" pill.
   * Phase 1 wires this to a no-op; Phase 6 connects it to the
   * CommandPalette.
   */
  onOpenPalette?: () => void;
}

/**
 * App titlebar (handoff R2 Client.html).
 *
 * Renders the 36-px tall bar with the project name, the active
 * provider tag (R2 / S3 / MinIO / RustFS), and the bucket + path,
 * plus a Cmd-K pill on the right.
 *
 * NOTE: macOS traffic-light buttons are intentionally NOT rendered.
 * Tauri v2 supplies native window decorations; the design's traffic
 * lights are decorative-only.
 */
export default function Titlebar({ onOpenPalette }: TitlebarProps) {
  const currentConfig = useAccountStore((s) => s.currentConfig);
  const currentPath = useCurrentPathStore((s) => s.currentPath);

  const provider = currentConfig?.provider ?? null;
  const bucket = currentConfig?.bucket ?? null;

  const providerTag = provider
    ? provider === 'r2'
      ? { className: 'tag-r2', label: 'R2' }
      : provider === 'aws'
        ? { className: 'tag-aws', label: 'S3' }
        : provider === 'minio'
          ? { className: 'tag-info', label: 'MinIO' }
          : provider === 'rustfs'
            ? { className: 'tag-info', label: 'RustFS' }
            : null
    : null;

  return (
    <div className="titlebar">
      <div className="tl-title">
        <div className="sb-logo" aria-label="R2 Client" title="R2 Client">
          R
        </div>
        {providerTag && (
          <>
            <span className="sep">·</span>
            <span className={`tag ${providerTag.className}`}>{providerTag.label}</span>
          </>
        )}
        {bucket && (
          <span style={{ marginLeft: 4 }}>
            {bucket}
            {currentPath || '/'}
          </span>
        )}
      </div>
      <button
        type="button"
        className="tl-cmd"
        onClick={onOpenPalette}
        aria-label="Open command palette"
      >
        <SearchOutlined style={{ fontSize: 11 }} />
        Search anything
        <span className="kbd">⌘K</span>
      </button>
    </div>
  );
}
