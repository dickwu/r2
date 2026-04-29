'use client';

import {
  SearchOutlined,
  UploadOutlined,
  ReloadOutlined,
  SunOutlined,
  MoonOutlined,
  BarsOutlined,
  AppstoreOutlined,
  SettingOutlined,
  StarOutlined,
} from '@ant-design/icons';
import { useThemeStore } from '@/app/stores/themeStore';
import { ConnectedCrumbs } from '@/app/components/Crumbs';

interface ToolbarProps {
  searchQuery: string;
  onSearchChange: (v: string) => void;
  viewMode: 'list' | 'grid';
  onViewModeChange: (v: 'list' | 'grid') => void;
  onRefresh: () => void;
  isRefreshing?: boolean;
  onUploadOpen: () => void;
  onSettingsOpen: () => void;
  onAppearanceOpen: () => void;
  bucketName?: string | null;
  bucketSize?: string | null;
  bucketCount?: string | null;
  onNavigate: (newPath: string) => void;
}

const DENSITY_OPTIONS = [
  { id: 'compact' as const, lines: [4, 8, 12, 16, 20] },
  { id: 'default' as const, lines: [5, 10, 15, 20] },
  { id: 'cozy' as const, lines: [6, 14, 22] },
];

export default function Toolbar({
  searchQuery,
  onSearchChange,
  viewMode,
  onViewModeChange,
  onRefresh,
  isRefreshing = false,
  onUploadOpen,
  onSettingsOpen,
  onAppearanceOpen,
  bucketName,
  bucketSize,
  bucketCount,
  onNavigate,
}: ToolbarProps) {
  const toolbarVariant = useThemeStore((s) => s.toolbarVariant);
  const density = useThemeStore((s) => s.density);
  const setDensity = useThemeStore((s) => s.setDensity);
  const theme = useThemeStore((s) => s.theme);
  const toggleTheme = useThemeStore((s) => s.toggleTheme);

  const variantClass =
    toolbarVariant === 'stacked'
      ? ' tb-stacked'
      : toolbarVariant === 'minimal'
        ? ' tb-minimal'
        : '';

  return (
    <div className={`toolbar${variantClass}`}>
      <div className="tb-left">
        {toolbarVariant === 'stacked' && bucketName && (
          <div className="tb-title-row">
            <h1>{bucketName}</h1>
            {(bucketSize || bucketCount) && (
              <span className="tb-meta">
                {[bucketSize, bucketCount ? `${bucketCount} objects` : null]
                  .filter(Boolean)
                  .join(' · ')}
              </span>
            )}
          </div>
        )}
        {bucketName && <ConnectedCrumbs bucket={bucketName} onNavigate={onNavigate} />}
      </div>

      <div className="tb-right">
        {/* Search */}
        <div className="search">
          <SearchOutlined className="search-icon" style={{ fontSize: 13 }} />
          <input
            placeholder="Search all files in bucket…"
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
          />
          {searchQuery && <span className="scope">⌘K</span>}
        </div>

        {/* View mode segmented */}
        <div className="segmented">
          <button
            className={viewMode === 'list' ? 'active' : ''}
            onClick={() => onViewModeChange('list')}
            title="List view"
          >
            <BarsOutlined style={{ fontSize: 13 }} />
          </button>
          <button
            className={viewMode === 'grid' ? 'active' : ''}
            onClick={() => onViewModeChange('grid')}
            title="Grid view"
          >
            <AppstoreOutlined style={{ fontSize: 13 }} />
          </button>
        </div>

        {/* Density segmented */}
        <div className="segmented" title="Density">
          {DENSITY_OPTIONS.map((d) => (
            <button
              key={d.id}
              className={density === d.id ? 'active' : ''}
              onClick={() => setDensity(d.id)}
              title={d.id}
            >
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
              >
                {d.lines.map((y, i) => (
                  <line key={i} x1="4" y1={y + 2} x2="20" y2={y + 2} />
                ))}
              </svg>
            </button>
          ))}
        </div>

        {/* Upload */}
        <button className="btn btn-primary" onClick={onUploadOpen}>
          <UploadOutlined style={{ fontSize: 13 }} />
          Upload
        </button>

        {/* Refresh */}
        <button className="btn btn-icon" title="Refresh" onClick={onRefresh}>
          <ReloadOutlined spin={isRefreshing} style={{ fontSize: 14 }} />
        </button>

        {/* Theme toggle */}
        <button className="btn btn-icon" title="Toggle theme" onClick={toggleTheme}>
          {theme === 'dark' ? (
            <SunOutlined style={{ fontSize: 14 }} />
          ) : (
            <MoonOutlined style={{ fontSize: 14 }} />
          )}
        </button>

        {/* Appearance */}
        <button className="btn btn-icon" title="Customize appearance" onClick={onAppearanceOpen}>
          <StarOutlined style={{ fontSize: 14 }} />
        </button>

        {/* Settings */}
        <button className="btn btn-icon" title="Settings" onClick={onSettingsOpen}>
          <SettingOutlined style={{ fontSize: 14 }} />
        </button>
      </div>
    </div>
  );
}
