export const SUPPORTED_LOCALES = ['en', 'zh-CN', 'hi', 'es', 'ja'] as const;
export type SupportedLocale = (typeof SUPPORTED_LOCALES)[number];

/** Native names, shown untranslated in the language picker. */
export const LOCALE_NATIVE_NAMES: Record<SupportedLocale, string> = {
  en: 'English',
  'zh-CN': '简体中文',
  hi: 'हिन्दी',
  es: 'Español',
  ja: '日本語',
};

/** Map an OS / browser language tag (e.g. `zh-Hans-CN`, `ja-JP`) to a supported locale. */
export function resolveLocale(tag: string | null | undefined): SupportedLocale {
  const t = (tag ?? '').toLowerCase();
  if (t.startsWith('zh')) return 'zh-CN';
  if (t.startsWith('ja')) return 'ja';
  if (t.startsWith('es')) return 'es';
  if (t.startsWith('hi')) return 'hi';
  return 'en';
}

export function systemLocale(): SupportedLocale {
  if (typeof navigator === 'undefined') return 'en';
  return resolveLocale(navigator.languages?.[0] ?? navigator.language);
}
