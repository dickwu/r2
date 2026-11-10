'use client';

import { useTranslation } from 'react-i18next';
import { useState } from 'react';
import {
  SunOutlined,
  AppstoreOutlined,
  SettingOutlined,
  UnorderedListOutlined,
  CheckOutlined,
  LinkOutlined,
  DatabaseOutlined,
} from '@ant-design/icons';
import { useThemeStore } from '@/app/stores/themeStore';
import { ACCENT_LIST } from '@/app/lib/accent';
import Modal from '@/app/components/ui/Modal';
import SettingsAccountPanel from '@/app/components/SettingsAccountPanel';
import SettingsMountsPanel from '@/app/components/SettingsMountsPanel';
import SettingsStoragePanel from '@/app/components/SettingsStoragePanel';
import { useShallow } from 'zustand/react/shallow';
import { useUpdateStore } from '@/app/stores/updateStore';
import { updateStatusView } from '@/app/lib/updates/updateStatus';
import { useLanguageStore, type LanguagePreference } from '@/app/stores/languageStore';
import { LOCALE_NATIVE_NAMES, SUPPORTED_LOCALES, systemLocale } from '@/i18n/locales';

export type SettingsTab = 'appearance' | 'layout' | 'account' | 'mounts' | 'storage' | 'shortcuts';

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
  const { t } = useTranslation();

  const themes = [
    { id: 'light' as const, label: t('settings.theme.light'), sub: t('settings.theme.lightSub') },
    { id: 'dark' as const, label: t('settings.theme.dark'), sub: t('settings.theme.darkSub') },
  ];

  const emptyStyles = [
    {
      id: 'blueprint' as const,
      label: t('settings.empty.blueprint'),
      sub: t('settings.empty.blueprintSub'),
    },
    {
      id: 'illustrated' as const,
      label: t('settings.empty.illustrated'),
      sub: t('settings.empty.illustratedSub'),
    },
    {
      id: 'minimal' as const,
      label: t('settings.empty.minimal'),
      sub: t('settings.empty.minimalSub'),
    },
  ];

  const handleSetAccent = (hex: string) => {
    setAccent(hex);
  };

  return (
    <div className="settings-section-stack">
      <LanguageSection />

      {/* Theme */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>{t('settings.theme.title')}</h3>
            <p>{t('settings.theme.desc')}</p>
          </div>
        </div>
        <div className="theme-cards">
          {themes.map((th) => (
            <button
              key={th.id}
              className={['theme-card', `theme-${th.id}`, theme === th.id && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => setTheme(th.id)}
            >
              <div className="theme-card-art" data-theme={th.id}>
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
                <strong>{th.label}</strong>
                <span>{th.sub}</span>
              </div>
              {theme === th.id && (
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
            <h3>{t('settings.accent.title')}</h3>
            <p>{t('settings.accent.desc')}</p>
          </div>
          <span className="settings-pill mono">{accent}</span>
        </div>
        <div className="accent-grid">
          {ACCENT_LIST.map(({ hex, meta }) => (
            <button
              key={hex}
              className={['accent-chip', accent === hex && 'active'].filter(Boolean).join(' ')}
              onClick={() => handleSetAccent(hex)}
              title={t(`settings.accent.names.${hex.slice(1)}`, { defaultValue: meta.name })}
            >
              <span className="accent-swatch" style={{ background: hex }}>
                {accent === hex && <CheckOutlined style={{ fontSize: 12, color: 'white' }} />}
              </span>
              <span className="accent-meta">
                <strong>
                  {t(`settings.accent.names.${hex.slice(1)}`, { defaultValue: meta.name })}
                </strong>
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
            <h3>{t('settings.empty.title')}</h3>
            <p>{t('settings.empty.desc')}</p>
          </div>
        </div>
        <div className="option-row-list">
          {emptyStyles.map((s) => (
            <button
              type="button"
              key={s.id}
              className={['option-row', emptyStyle === s.id && 'active'].filter(Boolean).join(' ')}
              onClick={() => setEmptyStyle(s.id)}
              aria-label={s.label}
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

/* ── Language section ───────────────────────────────────────────── */
function LanguageSection() {
  const { t } = useTranslation();
  const language = useLanguageStore((s) => s.language);
  const setLanguage = useLanguageStore((s) => s.setLanguage);
  const detected = LOCALE_NATIVE_NAMES[systemLocale()];

  const options: Array<{ id: LanguagePreference; label: string; sub: string }> = [
    {
      id: 'system',
      label: t('settings.language.system'),
      sub: t('settings.language.systemSub', { language: detected }),
    },
    ...SUPPORTED_LOCALES.map((id) => ({
      id,
      label: LOCALE_NATIVE_NAMES[id],
      sub: t(`settings.language.names.${id}`),
    })),
  ];

  return (
    <section className="settings-section">
      <div className="settings-section-head">
        <div>
          <h3>{t('settings.language.title')}</h3>
          <p>{t('settings.language.desc')}</p>
        </div>
      </div>
      <div className="option-row-list" role="radiogroup" aria-label={t('settings.language.title')}>
        {options.map((o) => (
          <label
            key={o.id}
            className={['option-row', language === o.id && 'active'].filter(Boolean).join(' ')}
            aria-label={o.label}
          >
            <input
              type="radio"
              className="sr-only"
              name="settings-language"
              value={o.id}
              checked={language === o.id}
              onChange={() => setLanguage(o.id)}
            />
            <span className="option-row-text">
              <strong>{o.label}</strong>
              <span>{o.sub}</span>
            </span>
            <span className="option-row-radio" aria-hidden="true" />
          </label>
        ))}
      </div>
    </section>
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
  const { t } = useTranslation();

  const sidebars = [
    { id: 'full' as const, label: t('settings.sidebar.full'), sub: t('settings.sidebar.fullSub') },
    {
      id: 'collapsed' as const,
      label: t('settings.sidebar.collapsed'),
      sub: t('settings.sidebar.collapsedSub'),
    },
  ];

  const densities = [
    {
      id: 'compact' as const,
      label: t('settings.density.compact'),
      sub: t('settings.density.compactSub'),
    },
    {
      id: 'default' as const,
      label: t('settings.density.default'),
      sub: t('settings.density.defaultSub'),
    },
    { id: 'cozy' as const, label: t('settings.density.cozy'), sub: t('settings.density.cozySub') },
  ];

  const toolbars = [
    {
      id: 'compact' as const,
      label: t('settings.toolbar.compact'),
      sub: t('settings.toolbar.compactSub'),
    },
    {
      id: 'stacked' as const,
      label: t('settings.toolbar.stacked'),
      sub: t('settings.toolbar.stackedSub'),
    },
    {
      id: 'minimal' as const,
      label: t('settings.toolbar.minimal'),
      sub: t('settings.toolbar.minimalSub'),
    },
  ];

  const views = [
    { id: 'list' as const, label: t('settings.view.list'), sub: t('settings.view.listSub') },
    { id: 'grid' as const, label: t('settings.view.grid'), sub: t('settings.view.gridSub') },
  ];

  return (
    <div className="settings-section-stack">
      {/* Sidebar */}
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>{t('settings.sidebar.title')}</h3>
            <p>{t('settings.sidebar.desc')}</p>
          </div>
        </div>
        <div className="visual-cards two">
          {sidebars.map((s) => (
            <button
              type="button"
              key={s.id}
              className={['visual-card', sidebarStyle === s.id && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => setSidebarStyle(s.id)}
              aria-label={s.label}
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
            <h3>{t('settings.density.title')}</h3>
            <p>{t('settings.density.desc')}</p>
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
            <h3>{t('settings.view.title')}</h3>
            <p>{t('settings.view.desc')}</p>
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
            <h3>{t('settings.toolbar.title')}</h3>
            <p>{t('settings.toolbar.desc')}</p>
          </div>
        </div>
        <div className="option-row-list">
          {toolbars.map((tb) => (
            <button
              type="button"
              key={tb.id}
              className={['option-row', toolbarVariant === tb.id && 'active']
                .filter(Boolean)
                .join(' ')}
              onClick={() => setToolbarVariant(tb.id)}
              aria-label={tb.label}
            >
              <span className="option-row-text">
                <strong>{tb.label}</strong>
                <span>{tb.sub}</span>
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
            <h3>{t('settings.inspector.title')}</h3>
            <p>{t('settings.inspector.desc')}</p>
          </div>
        </div>
        <button
          type="button"
          className={['toggle-row', showInspector && 'on'].filter(Boolean).join(' ')}
          onClick={() => setShowInspector(!showInspector)}
          aria-pressed={showInspector}
          aria-label={t('settings.inspector.show')}
        >
          <span className="option-row-text">
            <strong>{t('settings.inspector.show')}</strong>
            <span>{t('settings.inspector.showSub')}</span>
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
  const { t } = useTranslation();
  const click = t('settings.shortcuts.keyClick');
  const groups = [
    {
      title: t('settings.shortcuts.navigation'),
      items: [
        [t('settings.shortcuts.commandPalette'), '⌘ K'],
        [t('settings.shortcuts.toggleTheme'), '⌘ ⇧ T'],
        [t('settings.shortcuts.toggleView'), '⌘ L'],
        [t('settings.shortcuts.openSettings'), '⌘ ,'],
      ],
    },
    {
      title: t('settings.shortcuts.files'),
      items: [
        [t('settings.shortcuts.upload'), '⌘ U'],
        [t('settings.shortcuts.refresh'), '⌘ R'],
        [t('settings.shortcuts.selectAll'), '⌘ A'],
        [t('settings.shortcuts.deleteSelected'), '⌫'],
        [t('settings.shortcuts.rename'), 'Enter'],
        [t('settings.shortcuts.preview'), t('settings.shortcuts.keySpace')],
      ],
    },
    {
      title: t('settings.shortcuts.selection'),
      items: [
        [t('settings.shortcuts.rangeSelect'), `⇧ ${click}`],
        [t('settings.shortcuts.toggleOne'), `⌘ ${click}`],
        [t('settings.shortcuts.clearSelection'), 'Esc'],
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

/* ── Version, release notes, and update check (nav foot) ────────── */
function SettingsVersionFoot() {
  const s = useUpdateStore(
    useShallow((state) => ({
      phase: state.phase,
      manualCheck: state.manualCheck,
      currentVersion: state.currentVersion,
      update: state.update,
      progress: state.progress,
      failure: state.failure,
      checkForUpdates: state.checkForUpdates,
      openDialog: state.openDialog,
      retry: state.retry,
      openReleaseNotes: state.openReleaseNotes,
    }))
  );
  const { t } = useTranslation();
  const view = updateStatusView(s);
  // At rest the status bar shows the version; here the link names its action.
  const label =
    view.action === 'check' && s.phase !== 'upToDate' ? t('settings.checkForUpdates') : view.label;

  const onUpdateClick = () => {
    if (view.action === 'check') void s.checkForUpdates({ manual: true });
    else if (view.action === 'retryCheck') void s.retry();
    else if (view.action === 'openDialog') s.openDialog();
  };

  return (
    <>
      <div className="settings-nav-version">
        R2 Client{s.currentVersion ? ` ${s.currentVersion}` : ''}
      </div>
      <button type="button" className="settings-nav-link" onClick={s.openReleaseNotes}>
        {t('settings.releaseNotes')}
      </button>
      <button
        type="button"
        className={
          view.tone === 'accent'
            ? 'settings-nav-link settings-nav-link-accent'
            : 'settings-nav-link'
        }
        onClick={onUpdateClick}
        disabled={view.action === 'none'}
        title={view.hint}
        aria-live="polite"
      >
        {label}
      </button>
    </>
  );
}

/* ── SettingsModal ───────────────────────────────────────────────── */
export default function SettingsModal({
  open,
  onClose,
  initialTab = 'appearance',
  initialAccountId,
  onOpenAccountSettings: _onOpenAccountSettings,
}: SettingsModalProps) {
  const [tab, setTab] = useState<SettingsTab>(initialTab);
  const [wasOpen, setWasOpen] = useState(open);
  const { t } = useTranslation();

  if (open !== wasOpen) {
    setWasOpen(open);
    if (open) setTab(initialTab);
  }

  const tabs: Array<{ id: SettingsTab; label: string; icon: React.ReactNode }> = [
    { id: 'appearance', label: t('settings.tabs.appearance'), icon: <SunOutlined /> },
    { id: 'layout', label: t('settings.tabs.layout'), icon: <AppstoreOutlined /> },
    { id: 'account', label: t('settings.tabs.account'), icon: <SettingOutlined /> },
    { id: 'mounts', label: t('settings.tabs.mounts'), icon: <LinkOutlined /> },
    { id: 'storage', label: t('settings.tabs.storage'), icon: <DatabaseOutlined /> },
    { id: 'shortcuts', label: t('settings.tabs.shortcuts'), icon: <UnorderedListOutlined /> },
  ];

  // Only the preference panels save as you go; accounts, mounts and storage act on click.
  const footer =
    tab === 'account' || tab === 'mounts' || tab === 'storage' ? (
      <>
        <button className="btn btn-primary" onClick={onClose}>
          {t('common.done')}
        </button>
      </>
    ) : (
      <>
        <span style={{ marginRight: 'auto', fontSize: 11.5, color: 'var(--text-subtle)' }}>
          {t('settings.autoSave')}
        </span>
        <button className="btn btn-primary" onClick={onClose}>
          {t('common.done')}
        </button>
      </>
    );

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t('settings.title')}
      subtitle={t('settings.subtitle')}
      icon={<SettingOutlined style={{ fontSize: 18 }} />}
      width={780}
      bodyPadding={0}
      footer={footer}
    >
      <div className="settings-shell">
        {/* Side nav */}
        <nav className="settings-nav">
          {tabs.map((tb) => (
            <button
              key={tb.id}
              className={['settings-nav-item', tab === tb.id && 'active'].filter(Boolean).join(' ')}
              onClick={() => setTab(tb.id)}
            >
              <span className="settings-nav-icon">{tb.icon}</span>
              <span>{tb.label}</span>
            </button>
          ))}
          <div className="settings-nav-spacer" />
          <div className="settings-nav-foot">
            <SettingsVersionFoot />
            <a
              className="settings-nav-link"
              href="https://github.com/dickwu/r2"
              target="_blank"
              rel="noreferrer"
            >
              {t('settings.documentation')} ↗
            </a>
          </div>
        </nav>

        {/* Body */}
        <div className="settings-body">
          {tab === 'appearance' && <AppearancePanel />}
          {tab === 'layout' && <LayoutPanel />}
          {tab === 'shortcuts' && <ShortcutsPanel />}
          {tab === 'account' && <SettingsAccountPanel initialAccountId={initialAccountId} />}
          {tab === 'mounts' && <SettingsMountsPanel />}
          {tab === 'storage' && <SettingsStoragePanel />}
        </div>
      </div>
    </Modal>
  );
}
