import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export type Theme = 'light' | 'dark';

interface ThemeState {
  theme: Theme;
  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
}

export const useThemeStore = create<ThemeState>()(
  persist(
    (set, get) => ({
      theme: 'light',
      setTheme: (theme) => {
        set({ theme });
      },
      toggleTheme: () => {
        const next = get().theme === 'light' ? 'dark' : 'light';
        set({ theme: next });
      },
    }),
    {
      name: 'theme-storage',
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
}
