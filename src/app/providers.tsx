'use client';

import { useState, useEffect } from 'react';
import { App, ConfigProvider, theme } from 'antd';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { useThemeStore, initializeTheme } from '@/app/stores/themeStore';
import { applyAccent } from '@/app/lib/accent';

export default function Providers({ children }: { children: React.ReactNode }) {
  const currentTheme = useThemeStore((s) => s.theme);
  const accent = useThemeStore((s) => s.accent);
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    initializeTheme();
    setMounted(true);
  }, []);

  // Theme: keep `html.dark` (legacy) and `[data-theme="dark|light"]`
  // (handoff) in lockstep. Old CSS continues to react to `.dark`;
  // the new design tokens key off the data attribute.
  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle('dark', currentTheme === 'dark');
    root.setAttribute('data-theme', currentTheme);
  }, [currentTheme]);

  // Accent: write the design's --accent* CSS variables on every change so
  // every component that styles against them updates at once.
  useEffect(() => {
    applyAccent(accent);
  }, [accent]);

  const [queryClient] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: 60 * 1000,
            refetchOnWindowFocus: false,
          },
        },
      })
  );

  // Prevent hydration mismatch
  if (!mounted) {
    return null;
  }

  return (
    <QueryClientProvider client={queryClient}>
      <ConfigProvider
        theme={{
          algorithm: currentTheme === 'dark' ? theme.darkAlgorithm : theme.defaultAlgorithm,
          token: {
            colorPrimary: accent,
          },
        }}
      >
        <App>
          <div className="user-select-none cursor-default select-none">{children}</div>
        </App>
      </ConfigProvider>
      <ReactQueryDevtools initialIsOpen={false} />
    </QueryClientProvider>
  );
}
