'use client';

import { useState, useEffect } from 'react';
import { App, ConfigProvider, theme } from 'antd';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { useThemeStore, initializeTheme } from '@/app/stores/themeStore';
import { applyAccent } from '@/app/lib/accent';
import { installSessionLog } from '@/app/lib/diagnostics/installSessionLog';
import { useLanguageStore } from '@/app/stores/languageStore';
import { applyLocale } from '@/i18n';
import { systemLocale, type SupportedLocale } from '@/i18n/locales';
import enUS from 'antd/locale/en_US';
import zhCN from 'antd/locale/zh_CN';
import hiIN from 'antd/locale/hi_IN';
import esES from 'antd/locale/es_ES';
import jaJP from 'antd/locale/ja_JP';
import type { Locale } from 'antd/es/locale';

const ANTD_LOCALES: Record<SupportedLocale, Locale> = {
  en: enUS,
  'zh-CN': zhCN,
  hi: hiIN,
  es: esES,
  ja: jaJP,
};

export default function Providers({ children }: { children: React.ReactNode }) {
  const currentTheme = useThemeStore((s) => s.theme);
  const accent = useThemeStore((s) => s.accent);
  const languagePref = useLanguageStore((s) => s.language);
  const [mounted, setMounted] = useState(false);
  const [osLocale, setOsLocale] = useState<SupportedLocale>(() => systemLocale());
  const locale: SupportedLocale = languagePref === 'system' ? osLocale : languagePref;

  // Re-read the OS language when the webview reports a change.
  useEffect(() => {
    const onChange = () => setOsLocale(systemLocale());
    window.addEventListener('languagechange', onChange);
    return () => window.removeEventListener('languagechange', onChange);
  }, []);

  useEffect(() => {
    applyLocale(locale);
  }, [locale]);

  useEffect(() => {
    // Record warnings and errors from the first render on, so a problem
    // report can carry what the app was doing before it went wrong.
    installSessionLog();
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
        locale={ANTD_LOCALES[locale]}
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
