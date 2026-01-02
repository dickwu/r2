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
        document.documentElement.classList.toggle('dark', theme === 'dark');
      },
      toggleTheme: () => {
        const next = get().theme === 'light' ? 'dark' : 'light';
        set({ theme: next });
        document.documentElement.classList.toggle('dark', next === 'dark');
      },
    }),
    {
      name: 'theme-storage',
      onRehydrateStorage: () => (state) => {
        // Apply theme class on rehydration
        if (state) {
          document.documentElement.classList.toggle('dark', state.theme === 'dark');
        }
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
}
