'use client';

import { useState, useEffect } from 'react';
import {
  SunOutlined,
  AppstoreOutlined,
  SettingOutlined,
  UnorderedListOutlined,
  CheckOutlined,
} from '@ant-design/icons';
import { useThemeStore } from '@/app/stores/themeStore';
import { ACCENT_LIST } from '@/app/lib/accent';
import Modal from '@/app/components/ui/Modal';
import SettingsAccountPanel from '@/app/components/SettingsAccountPanel';

export type SettingsTab = 'appearance' | 'layout' | 'account' | 'shortcuts';

interface SettingsModalProps {
  open: boolean;
  onClose: () => void;
  initialTab?: SettingsTab;
  initialAccountId?: string;
  /** @deprecated kept for compatibility — no longer used by this modal */
  onOpenAccountSettings?: () => void;
}

/* ── Appearance panel ───────────────────────────────────────────── */
function AppearancePanel() {
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  const accent = useThemeStore((s) => s.accent);
  const setAccent = useThemeStore((s) => s.setAccent);
  const emptyStyle = useThemeStore((s) => s.emptyStyle);
  const setEmptyStyle = useThemeStore((s) => s.setEmptyStyle);

  const themes = [
    { id: 'light' as const, label: 'Light', sub: 'Crisp & bright' },
    { id: 'dark' as const, label: 'Dark', sub: 'Easy on the eyes' },
  ];

  const emptyStyles = [
    { id: 'blueprint' as const, label: 'Blueprint grid', sub: 'Technical, dotted backdrop' },
    { id: 'illustrated' as const, label: 'Soft illustration', sub: 'Friendly empty box' },
    { id: 'minimal' as const, label: 'Minimal', sub: 'Just the essentials' },
  ];

  const handleSetAccent = (hex: string) => {
    setAccent(hex);
  };

  return (
    <div className="settings-section-stack">
      {/* Theme */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Theme</h3>
            <p>Pick a base palette. Switches the entire app.</p>
          </div>
        </div>
        <div className="theme-cards">
          {themes.map((t) => (
            <button
              key={t.id}
              className={['theme-card', `theme-${t.id}`, theme === t.id && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => setTheme(t.id)}
            >
              <div className="theme-card-art" data-theme={t.id}>
                <div className="tc-titlebar">
                  <span />
                  <span />
                  <span />
                </div>
                <div className="tc-body">
                  <div className="tc-side">
                    <span />
                    <span />
                    <span />
                  </div>
                  <div className="tc-main">
                    <div className="tc-tool">
                      <span />
                      <span />
                    </div>
                    <div className="tc-row">
                      <span className="dot" />
                      <span className="line" />
                    </div>
                    <div className="tc-row">
                      <span className="dot" />
                      <span className="line short" />
                    </div>
                    <div className="tc-row">
                      <span className="dot" />
                      <span className="line" />
                    </div>
                  </div>
                </div>
              </div>
              <div className="theme-card-meta">
                <strong>{t.label}</strong>
                <span>{t.sub}</span>
              </div>
              {theme === t.id && (
                <div className="theme-card-check">
                  <CheckOutlined style={{ fontSize: 11 }} />
                </div>
              )}
            </button>
          ))}
        </div>
      </section>

      {/* Accent */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Accent color</h3>
            <p>Used for primary buttons, selection state, and focus rings.</p>
          </div>
          <span className="settings-pill mono">{accent}</span>
        </div>
        <div className="accent-grid">
          {ACCENT_LIST.map(({ hex, meta }) => (
            <button
              key={hex}
              className={['accent-chip', accent === hex && 'active'].filter(Boolean).join(' ')}
              onClick={() => handleSetAccent(hex)}
              title={meta.name}
            >
              <span className="accent-swatch" style={{ background: hex }}>
                {accent === hex && <CheckOutlined style={{ fontSize: 12, color: 'white' }} />}
              </span>
              <span className="accent-meta">
                <strong>{meta.name}</strong>
                <span className="mono">{hex}</span>
              </span>
            </button>
          ))}
        </div>
      </section>

      {/* Empty state */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Empty state</h3>
            <p>What you see when a folder has no files yet.</p>
          </div>
        </div>
        <div className="option-row-list">
          {emptyStyles.map((s) => (
            <button
              key={s.id}
              className={['option-row', emptyStyle === s.id && 'active'].filter(Boolean).join(' ')}
              onClick={() => setEmptyStyle(s.id)}
            >
              <span className="option-row-text">
                <strong>{s.label}</strong>
                <span>{s.sub}</span>
              </span>
              <span className="option-row-radio" />
            </button>
          ))}
        </div>
      </section>
    </div>
  );
}

/* ── Layout panel ────────────────────────────────────────────────── */
function LayoutPanel() {
  const sidebarStyle = useThemeStore((s) => s.sidebarStyle);
  const setSidebarStyle = useThemeStore((s) => s.setSidebarStyle);
  const density = useThemeStore((s) => s.density);
  const setDensity = useThemeStore((s) => s.setDensity);
  const defaultView = useThemeStore((s) => s.defaultView);
  const setDefaultView = useThemeStore((s) => s.setDefaultView);
  const toolbarVariant = useThemeStore((s) => s.toolbarVariant);
  const setToolbarVariant = useThemeStore((s) => s.setToolbarVariant);
  const showInspector = useThemeStore((s) => s.showInspector);
  const setShowInspector = useThemeStore((s) => s.setShowInspector);

  const sidebars = [
    { id: 'full' as const, label: 'Full', sub: '240px tree' },
    { id: 'collapsed' as const, label: 'Collapsed', sub: '60px icons' },
  ];

  const densities = [
    { id: 'compact' as const, label: 'Compact', sub: 'Linear / Notion-like' },
    { id: 'default' as const, label: 'Default', sub: 'Balanced' },
    { id: 'cozy' as const, label: 'Cozy', sub: 'Finder-like, generous' },
  ];

  const toolbars = [
    { id: 'compact' as const, label: 'Compact', sub: 'Crumbs + actions in one row' },
    { id: 'stacked' as const, label: 'Stacked title', sub: 'Bucket name above breadcrumbs' },
    { id: 'minimal' as const, label: 'Minimal', sub: 'Crumbs only, hide secondary actions' },
  ];

  const views = [
    { id: 'list' as const, label: 'List', sub: 'Table with sortable columns' },
    { id: 'grid' as const, label: 'Grid', sub: 'Card thumbnails' },
  ];

  return (
    <div className="settings-section-stack">
      {/* Sidebar */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Sidebar</h3>
            <p>How the navigation rail behaves.</p>
          </div>
        </div>
        <div className="visual-cards two">
          {sidebars.map((s) => (
            <button
              key={s.id}
              className={['visual-card', sidebarStyle === s.id && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => setSidebarStyle(s.id)}
            >
              <div className={`vc-art vc-sidebar vc-sidebar-${s.id}`}>
                <div className="vc-rail" />
                <div className="vc-canvas">
                  <span />
                  <span />
                  <span />
                  <span />
                </div>
              </div>
              <div className="vc-meta">
                <strong>{s.label}</strong>
                <span>{s.sub}</span>
              </div>
            </button>
          ))}
        </div>
      </section>

      {/* Density */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Density</h3>
            <p>Row height and spacing in the file list.</p>
          </div>
        </div>
        <div className="visual-cards three">
          {densities.map((d) => (
            <button
              key={d.id}
              className={['visual-card', density === d.id && 'active'].filter(Boolean).join(' ')}
              onClick={() => setDensity(d.id)}
            >
              <div className={`vc-art vc-density vc-density-${d.id}`}>
                {Array.from({ length: d.id === 'compact' ? 7 : d.id === 'default' ? 5 : 4 }).map(
                  (_, i) => (
                    <span key={i} className="vc-row" />
                  )
                )}
              </div>
              <div className="vc-meta">
                <strong>{d.label}</strong>
                <span>{d.sub}</span>
              </div>
            </button>
          ))}
        </div>
      </section>

      {/* Default view */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Default view</h3>
            <p>What loads when you open a bucket.</p>
          </div>
        </div>
        <div className="visual-cards two">
          {views.map((v) => (
            <button
              key={v.id}
              className={['visual-card', defaultView === v.id && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => setDefaultView(v.id)}
            >
              <div className={`vc-art vc-view vc-view-${v.id}`}>
                {v.id === 'list'
                  ? Array.from({ length: 5 }).map((_, i) => <span key={i} className="vc-row" />)
                  : Array.from({ length: 6 }).map((_, i) => <span key={i} className="vc-tile" />)}
              </div>
              <div className="vc-meta">
                <strong>{v.label}</strong>
                <span>{v.sub}</span>
              </div>
            </button>
          ))}
        </div>
      </section>

      {/* Toolbar */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Toolbar</h3>
            <p>How the top action bar lays out.</p>
          </div>
        </div>
        <div className="option-row-list">
          {toolbars.map((t) => (
            <button
              key={t.id}
              className={['option-row', toolbarVariant === t.id && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => setToolbarVariant(t.id)}
            >
              <span className="option-row-text">
                <strong>{t.label}</strong>
                <span>{t.sub}</span>
              </span>
              <span className="option-row-radio" />
            </button>
          ))}
        </div>
      </section>

      {/* Inspector */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Inspector</h3>
            <p>Side drawer with metadata when a file is focused.</p>
          </div>
        </div>
        <button
          className={['toggle-row', showInspector && 'on'].filter(Boolean).join(' ')}
          onClick={() => setShowInspector(!showInspector)}
        >
          <span className="option-row-text">
            <strong>Show inspector</strong>
            <span>Slides in from the right when you focus a file.</span>
          </span>
          <span className="toggle-switch">
            <span className="toggle-knob" />
          </span>
        </button>
      </section>
    </div>
  );
}

/* ── Shortcuts panel ─────────────────────────────────────────────── */
function ShortcutsPanel() {
  const groups = [
    {
      title: 'Navigation',
      items: [
        ['Open command palette', '⌘ K'],
        ['Toggle theme', '⌘ ⇧ T'],
        ['Toggle list / grid', '⌘ L'],
        ['Open settings', '⌘ ,'],
      ],
    },
    {
      title: 'Files',
      items: [
        ['Upload', '⌘ U'],
        ['Refresh', '⌘ R'],
        ['Select all', '⌘ A'],
        ['Delete selected', '⌫'],
        ['Rename', 'Enter'],
        ['Preview', 'Space'],
      ],
    },
    {
      title: 'Selection',
      items: [
        ['Range select', '⇧ Click'],
        ['Toggle one', '⌘ Click'],
        ['Clear selection', 'Esc'],
      ],
    },
  ];

  return (
    <div className="settings-section-stack">
      {groups.map((g) => (
        <section className="settings-section" key={g.title}>
          <div className="settings-section-head">
            <div>
              <h3>{g.title}</h3>
            </div>
          </div>
          <div className="kbd-list">
            {g.items.map(([label, key]) => (
              <div key={label} className="kbd-row">
                <span>{label}</span>
                <span className="kbd-keys">
                  {key.split(' ').map((k, i) => (
                    <kbd key={i}>{k}</kbd>
                  ))}
                </span>
              </div>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

/* ── Account panel is now the full SettingsAccountPanel ─────────── */

/* ── SettingsModal ───────────────────────────────────────────────── */
export default function SettingsModal({
  open,
  onClose,
  initialTab = 'appearance',
  initialAccountId,
  onOpenAccountSettings: _onOpenAccountSettings,
}: SettingsModalProps) {
  const [tab, setTab] = useState<SettingsTab>(initialTab);

  useEffect(() => {
    if (open) setTab(initialTab);
  }, [open, initialTab]);

  const tabs: Array<{ id: SettingsTab; label: string; icon: React.ReactNode }> = [
    { id: 'appearance', label: 'Appearance', icon: <SunOutlined /> },
    { id: 'layout', label: 'Layout', icon: <AppstoreOutlined /> },
    { id: 'account', label: 'Account', icon: <SettingOutlined /> },
    { id: 'shortcuts', label: 'Shortcuts', icon: <UnorderedListOutlined /> },
  ];

  const footer =
    tab === 'account' ? (
      <>
        <button className="btn btn-primary" onClick={onClose}>
          Done
        </button>
      </>
    ) : (
      <>
        <span style={{ marginRight: 'auto', fontSize: 11.5, color: 'var(--text-subtle)' }}>
          Changes save automatically
        </span>
        <button className="btn btn-primary" onClick={onClose}>
          Done
        </button>
      </>
    );

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Settings"
      subtitle="Personalize the workspace, theme, and connected accounts"
      icon={<SettingOutlined style={{ fontSize: 18 }} />}
      width={780}
      bodyPadding={0}
      footer={footer}
    >
      <div className="settings-shell">
        {/* Side nav */}
        <nav className="settings-nav">
          {tabs.map((t) => (
            <button
              key={t.id}
              className={['settings-nav-item', tab === t.id && 'active'].filter(Boolean).join(' ')}
              onClick={() => setTab(t.id)}
            >
              <span className="settings-nav-icon">{t.icon}</span>
              <span>{t.label}</span>
            </button>
          ))}
          <div className="settings-nav-spacer" />
          <div className="settings-nav-foot">
            <div className="settings-nav-version">R2 Client</div>
            <a
              className="settings-nav-link"
              href="https://github.com/dickwu/r2"
              target="_blank"
              rel="noreferrer"
            >
              Documentation ↗
            </a>
          </div>
        </nav>

        {/* Body */}
        <div className="settings-body">
          {tab === 'appearance' && <AppearancePanel />}
          {tab === 'layout' && <LayoutPanel />}
          {tab === 'shortcuts' && <ShortcutsPanel />}
          {tab === 'account' && <SettingsAccountPanel initialAccountId={initialAccountId} />}
        </div>
      </div>
    </Modal>
  );
}
