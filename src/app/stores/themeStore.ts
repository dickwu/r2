import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { DEFAULT_ACCENT, type AccentHex } from '@/app/lib/accent';

export type Theme = 'light' | 'dark';
export type SidebarStyle = 'full' | 'collapsed' | 'floating';
export type Density = 'compact' | 'default' | 'cozy';
export type FileView = 'list' | 'grid';
export type ToolbarVariant = 'compact' | 'stacked' | 'minimal';
export type EmptyStyle = 'blueprint' | 'illustrated' | 'minimal';

interface ThemeState {
  theme: Theme;
  accent: AccentHex;
  sidebarStyle: SidebarStyle;
  density: Density;
  defaultView: FileView;
  toolbarVariant: ToolbarVariant;
  emptyStyle: EmptyStyle;
  showInspector: boolean;

  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
  setAccent: (accent: AccentHex) => void;
  setSidebarStyle: (style: SidebarStyle) => void;
  cycleSidebarStyle: () => void;
  setDensity: (density: Density) => void;
  setDefaultView: (view: FileView) => void;
  setToolbarVariant: (variant: ToolbarVariant) => void;
  setEmptyStyle: (style: EmptyStyle) => void;
  setShowInspector: (show: boolean) => void;
}

// Toggle only swaps full ↔ collapsed. Floating is reachable from Settings
// only — it's no longer part of the cycle so the collapse icon round-trips
// directly back to full.
const SIDEBAR_CYCLE: Record<SidebarStyle, SidebarStyle> = {
  full: 'collapsed',
  collapsed: 'full',
  floating: 'full',
};

export const useThemeStore = create<ThemeState>()(
  persist(
    (set, get) => ({
      theme: 'light',
      accent: DEFAULT_ACCENT,
      sidebarStyle: 'full',
      density: 'default',
      defaultView: 'list',
      toolbarVariant: 'compact',
      emptyStyle: 'blueprint',
      showInspector: false,

      setTheme: (theme) => set({ theme }),
      toggleTheme: () => set({ theme: get().theme === 'light' ? 'dark' : 'light' }),
      setAccent: (accent) => set({ accent }),
      setSidebarStyle: (sidebarStyle) => set({ sidebarStyle }),
      cycleSidebarStyle: () => set({ sidebarStyle: SIDEBAR_CYCLE[get().sidebarStyle] }),
      setDensity: (density) => set({ density }),
      setDefaultView: (defaultView) => set({ defaultView }),
      setToolbarVariant: (toolbarVariant) => set({ toolbarVariant }),
      setEmptyStyle: (emptyStyle) => set({ emptyStyle }),
      setShowInspector: (showInspector) => set({ showInspector }),
    }),
    {
      name: 'theme-storage',
      version: 2,
      // Persist v1 only stored `theme`. Merge defaults for the new redesign keys
      // when loading older payloads so existing users don't see a broken UI.
      migrate: (persistedState, version) => {
        const incoming = (persistedState ?? {}) as Partial<ThemeState>;
        if (version < 2) {
          return {
            theme: incoming.theme ?? 'light',
            accent: incoming.accent ?? DEFAULT_ACCENT,
            sidebarStyle: incoming.sidebarStyle ?? 'full',
            density: incoming.density ?? 'default',
            defaultView: incoming.defaultView ?? 'list',
            toolbarVariant: incoming.toolbarVariant ?? 'compact',
            emptyStyle: incoming.emptyStyle ?? 'blueprint',
            showInspector: incoming.showInspector ?? false,
          } as ThemeState;
        }
        return incoming as ThemeState;
      },
    }
  )
);

// Initialize theme from system preference if no stored value
export function initializeTheme() {
  const stored = localStorage.getItem('theme-storage');
  if (!stored) {
    const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    useThemeStore.getState().setTheme(prefersDark ? 'dark' : 'light');
  }
  // Floating sidebar mode was removed from the UI — coerce any persisted
  // `floating` back to `full` so users with old preferences land in a
  // controllable state.
  const { sidebarStyle, setSidebarStyle } = useThemeStore.getState();
  if (sidebarStyle === 'floating') {
    setSidebarStyle('full');
  }
}
