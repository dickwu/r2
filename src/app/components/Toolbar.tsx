'use client';

import { useTranslation } from 'react-i18next';
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
import type { ViewMode } from '@/app/stores/viewModeStore';

interface ToolbarProps {
  searchQuery: string;
  onSearchChange: (v: string) => void;
  viewMode: ViewMode;
  onViewModeChange: (v: ViewMode) => void;
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
  const { t } = useTranslation();
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
                {[bucketSize, bucketCount ? t('toolbar.objects', { count: bucketCount }) : null]
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
            placeholder={t('toolbar.searchPlaceholder')}
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
          />
          {searchQuery && <span className="scope">⌘K</span>}
        </div>

        {/* View mode segmented */}
        <div className="segmented" role="group" aria-label={t('settings.view.title')}>
          <button
            className={viewMode === 'list' ? 'active' : ''}
            onClick={() => onViewModeChange('list')}
            title={t('toolbar.listView')}
            aria-label={t('toolbar.listView')}
            aria-pressed={viewMode === 'list'}
          >
            <BarsOutlined style={{ fontSize: 13 }} />
          </button>
          <button
            className={viewMode === 'grid' ? 'active' : ''}
            onClick={() => onViewModeChange('grid')}
            title={t('toolbar.gridView')}
            aria-label={t('toolbar.gridView')}
            aria-pressed={viewMode === 'grid'}
          >
            <AppstoreOutlined style={{ fontSize: 13 }} />
          </button>
          <button
            className={viewMode === 'column' ? 'active' : ''}
            onClick={() => onViewModeChange('column')}
            title={t('toolbar.columnView')}
            aria-label={t('toolbar.columnView')}
            aria-pressed={viewMode === 'column'}
          >
            {/* Three side-by-side panes, the Finder column-view glyph */}
            <svg
              width="13"
              height="13"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.9"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <rect x="2.5" y="4" width="19" height="16" rx="2.5" />
              <line x1="9" y1="4" x2="9" y2="20" />
              <line x1="15" y1="4" x2="15" y2="20" />
            </svg>
          </button>
        </div>

        {/* Density segmented */}
        <div className="segmented" title={t('settings.density.title')}>
          {DENSITY_OPTIONS.map((d) => (
            <button
              key={d.id}
              className={density === d.id ? 'active' : ''}
              onClick={() => setDensity(d.id)}
              title={t(`settings.density.${d.id}`)}
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
          {t('toolbar.upload')}
        </button>

        {/* Refresh */}
        <button className="btn btn-icon" title={t('common.refresh')} onClick={onRefresh}>
          <ReloadOutlined spin={isRefreshing} style={{ fontSize: 14 }} />
        </button>

        {/* Theme toggle */}
        <button
          className="btn btn-icon"
          title={t('settings.shortcuts.toggleTheme')}
          onClick={toggleTheme}
        >
          {theme === 'dark' ? (
            <SunOutlined style={{ fontSize: 14 }} />
          ) : (
            <MoonOutlined style={{ fontSize: 14 }} />
          )}
        </button>

        {/* Appearance */}
        <button
          className="btn btn-icon"
          title={t('toolbar.customizeAppearance')}
          onClick={onAppearanceOpen}
        >
          <StarOutlined style={{ fontSize: 14 }} />
        </button>

        {/* Settings */}
        <button className="btn btn-icon" title={t('settings.title')} onClick={onSettingsOpen}>
          <SettingOutlined style={{ fontSize: 14 }} />
        </button>
      </div>
    </div>
  );
}
