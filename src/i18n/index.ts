import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import en from './locales/en.json';
import zhCN from './locales/zh-CN.json';
import hi from './locales/hi.json';
import es from './locales/es.json';
import ja from './locales/ja.json';
import { systemLocale, type SupportedLocale } from './locales';

export const resources = {
  en: { translation: en },
  'zh-CN': { translation: zhCN },
  hi: { translation: hi },
  es: { translation: es },
  ja: { translation: ja },
} as const;

if (!i18n.isInitialized) {
  void i18n.use(initReactI18next).init({
    resources,
    lng: systemLocale(),
    fallbackLng: 'en',
    interpolation: { escapeValue: false },
    returnNull: false,
  });
}

export function applyLocale(locale: SupportedLocale) {
  if (i18n.language !== locale) void i18n.changeLanguage(locale);
  if (typeof document !== 'undefined') document.documentElement.lang = locale;
}

/** Translate outside React (stores, libs, toasts). */
export const t = i18n.t.bind(i18n);
export default i18n;
