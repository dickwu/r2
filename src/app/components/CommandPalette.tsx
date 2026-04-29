'use client';

import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { createPortal } from 'react-dom';
import {
  SearchOutlined,
  DatabaseOutlined,
  UploadOutlined,
  ReloadOutlined,
  BgColorsOutlined,
  UnorderedListOutlined,
  SettingOutlined,
  DownloadOutlined,
  FolderOutlined,
  SwitcherOutlined,
} from '@ant-design/icons';
import { useAccountStore } from '@/app/stores/accountStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';

// ── Action types ──────────────────────────────────────────────────────────────

export type CommandAction =
  | { type: 'bucket'; provider: string; accountId: string; bucket: string; tokenId?: number }
  | { type: 'open'; value: 'upload' | 'settings' | 'dock' }
  | { type: 'refresh' }
  | { type: 'theme' }
  | { type: 'view' }
  | { type: 'path'; value: string };

interface PaletteItem {
  id: string;
  label: string;
  meta?: string;
  icon: React.ReactNode;
  action: CommandAction;
  section: string;
}

// ── Props ─────────────────────────────────────────────────────────────────────

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  onAction: (action: CommandAction) => void;
}

// ── Static action items ───────────────────────────────────────────────────────

const STATIC_ACTIONS: Omit<PaletteItem, 'id'>[] = [
  {
    label: 'Upload files',
    meta: '⌘U',
    icon: <UploadOutlined />,
    action: { type: 'open', value: 'upload' },
    section: 'Actions',
  },
  {
    label: 'Refresh files',
    meta: '⌘R',
    icon: <ReloadOutlined />,
    action: { type: 'refresh' },
    section: 'Actions',
  },
  {
    label: 'Toggle theme',
    meta: '⌘⇧T',
    icon: <BgColorsOutlined />,
    action: { type: 'theme' },
    section: 'Actions',
  },
  {
    label: 'Toggle list / grid',
    meta: '⌘L',
    icon: <UnorderedListOutlined />,
    action: { type: 'view' },
    section: 'Actions',
  },
  {
    label: 'Open settings',
    meta: '⌘,',
    icon: <SettingOutlined />,
    action: { type: 'open', value: 'settings' },
    section: 'Actions',
  },
  {
    label: 'Show transfer dock',
    icon: <DownloadOutlined />,
    action: { type: 'open', value: 'dock' },
    section: 'Actions',
  },
];

// ── Component ─────────────────────────────────────────────────────────────────

export default function CommandPalette({ open, onClose, onAction }: CommandPaletteProps) {
  const [query, setQuery] = useState('');
  const [activeIdx, setActiveIdx] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const accounts = useAccountStore((s) => s.accounts);
  const currentPath = useCurrentPathStore((s) => s.currentPath);

  // Reset state when palette opens
  useEffect(() => {
    if (open) {
      setQuery('');
      setActiveIdx(0);
      setTimeout(() => inputRef.current?.focus(), 30);
    }
  }, [open]);

  // Build bucket items from accounts
  const bucketItems = useMemo<PaletteItem[]>(() => {
    const items: PaletteItem[] = [];
    for (const acct of accounts) {
      if (acct.provider === 'r2') {
        for (const twb of acct.tokens) {
          for (const bucket of twb.buckets) {
            items.push({
              id: `bucket-r2-${acct.account.id}-${twb.token.id}-${bucket.name}`,
              label: bucket.name,
              meta: 'R2',
              icon: <DatabaseOutlined />,
              action: {
                type: 'bucket',
                provider: 'r2',
                accountId: acct.account.id,
                bucket: bucket.name,
                tokenId: twb.token.id,
              },
              section: 'Buckets',
            });
          }
        }
      } else if (acct.provider === 'aws') {
        for (const bucket of acct.buckets) {
          items.push({
            id: `bucket-aws-${acct.account.id}-${bucket.name}`,
            label: bucket.name,
            meta: 'S3',
            icon: <DatabaseOutlined />,
            action: {
              type: 'bucket',
              provider: 'aws',
              accountId: acct.account.id,
              bucket: bucket.name,
            },
            section: 'Buckets',
          });
        }
      } else if (acct.provider === 'minio') {
        for (const bucket of acct.buckets) {
          items.push({
            id: `bucket-minio-${acct.account.id}-${bucket.name}`,
            label: bucket.name,
            meta: 'MinIO',
            icon: <DatabaseOutlined />,
            action: {
              type: 'bucket',
              provider: 'minio',
              accountId: acct.account.id,
              bucket: bucket.name,
            },
            section: 'Buckets',
          });
        }
      } else if (acct.provider === 'rustfs') {
        for (const bucket of acct.buckets) {
          items.push({
            id: `bucket-rustfs-${acct.account.id}-${bucket.name}`,
            label: bucket.name,
            meta: 'RustFS',
            icon: <DatabaseOutlined />,
            action: {
              type: 'bucket',
              provider: 'rustfs',
              accountId: acct.account.id,
              bucket: bucket.name,
            },
            section: 'Buckets',
          });
        }
      }
    }
    return items;
  }, [accounts]);

  // Build navigate items from current path segments
  const navigateItems = useMemo<PaletteItem[]>(() => {
    if (!currentPath) return [];
    const parts = currentPath.split('/').filter(Boolean);
    const items: PaletteItem[] = [];
    let accumulated = '';
    for (const part of parts) {
      accumulated += part + '/';
      const path = accumulated;
      items.push({
        id: `nav-${path}`,
        label: part,
        meta: path,
        icon: <FolderOutlined />,
        action: { type: 'path', value: path },
        section: 'Navigate',
      });
    }
    // Also add root
    items.unshift({
      id: 'nav-root',
      label: 'Root',
      meta: '/',
      icon: <SwitcherOutlined />,
      action: { type: 'path', value: '' },
      section: 'Navigate',
    });
    return items;
  }, [currentPath]);

  // All items merged
  const allItems = useMemo<PaletteItem[]>(() => {
    const actions = STATIC_ACTIONS.map((a, i) => ({ ...a, id: `action-${i}` }));
    return [...bucketItems, ...actions, ...navigateItems];
  }, [bucketItems, navigateItems]);

  // Filtered items
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return allItems;
    return allItems.filter(
      (item) =>
        item.label.toLowerCase().includes(q) ||
        item.meta?.toLowerCase().includes(q) ||
        item.section.toLowerCase().includes(q)
    );
  }, [allItems, query]);

  // Sections
  const sections = useMemo(() => {
    const map = new Map<string, PaletteItem[]>();
    for (const item of filtered) {
      if (!map.has(item.section)) map.set(item.section, []);
      map.get(item.section)!.push(item);
    }
    return Array.from(map.entries());
  }, [filtered]);

  // Flat list for keyboard navigation
  const flatItems = useMemo(() => filtered, [filtered]);

  // Reset active index on query change
  useEffect(() => {
    setActiveIdx(0);
  }, [query]);

  // Scroll active item into view
  useEffect(() => {
    if (!listRef.current) return;
    const active = listRef.current.querySelector('.cmdk-item.active');
    active?.scrollIntoView({ block: 'nearest' });
  }, [activeIdx]);

  const handleSelect = useCallback(
    (item: PaletteItem) => {
      onAction(item.action);
      onClose();
    },
    [onAction, onClose]
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setActiveIdx((i) => Math.min(i + 1, flatItems.length - 1));
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        setActiveIdx((i) => Math.max(i - 1, 0));
      } else if (e.key === 'Enter') {
        e.preventDefault();
        const item = flatItems[activeIdx];
        if (item) handleSelect(item);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    },
    [flatItems, activeIdx, handleSelect, onClose]
  );

  if (!open || typeof window === 'undefined') return null;

  // Global flat index tracker across sections
  let globalIdx = 0;

  return createPortal(
    <div className="cmdk-backdrop" onClick={onClose}>
      <div className="cmdk" onClick={(e) => e.stopPropagation()} onKeyDown={handleKeyDown}>
        {/* Input */}
        <div className="cmdk-input">
          <SearchOutlined style={{ color: 'var(--text-muted)', fontSize: 16 }} />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search buckets, actions, paths…"
          />
          <span className="kbd">Esc</span>
        </div>

        {/* List */}
        <div className="cmdk-list" ref={listRef}>
          {sections.length === 0 && (
            <div
              style={{
                padding: '20px 10px',
                textAlign: 'center',
                color: 'var(--text-subtle)',
                fontSize: 13,
              }}
            >
              No results
            </div>
          )}
          {sections.map(([section, items]) => (
            <div key={section}>
              <div className="cmdk-section">{section}</div>
              {items.map((item) => {
                const idx = globalIdx++;
                const isActive = idx === activeIdx;
                return (
                  <div
                    key={item.id}
                    className={['cmdk-item', isActive && 'active'].filter(Boolean).join(' ')}
                    onClick={() => handleSelect(item)}
                    onMouseEnter={() => setActiveIdx(idx)}
                  >
                    <span className="cmdk-item-icon">{item.icon}</span>
                    <span className="cmdk-item-label">{item.label}</span>
                    {item.meta && <span className="cmdk-item-meta">{item.meta}</span>}
                  </div>
                );
              })}
            </div>
          ))}
        </div>

        {/* Footer */}
        <div className="cmdk-footer">
          <span>
            <span className="kbd">↑</span>
            <span className="kbd">↓</span> navigate
          </span>
          <span>
            <span className="kbd">↵</span> select
          </span>
          <span>
            <span className="kbd">⌘K</span> toggle
          </span>
        </div>
      </div>
    </div>,
    document.body
  );
}
