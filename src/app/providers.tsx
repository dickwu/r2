'use client';

import { useState, useEffect } from 'react';
import { App, ConfigProvider, theme } from 'antd';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { useThemeStore, initializeTheme } from './stores/themeStore';

export default function Providers({ children }: { children: React.ReactNode }) {
  const currentTheme = useThemeStore((s) => s.theme);
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    initializeTheme();
    setMounted(true);
  }, []);

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
            colorPrimary: '#f6821f',
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
