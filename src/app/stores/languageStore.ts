import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { SupportedLocale } from '@/i18n/locales';

/** `system` follows the OS language; anything else is a manual override. */
export type LanguagePreference = 'system' | SupportedLocale;

interface LanguageState {
  language: LanguagePreference;
  setLanguage: (language: LanguagePreference) => void;
}

export const useLanguageStore = create<LanguageState>()(
  persist(
    (set) => ({
      language: 'system',
      setLanguage: (language) => set({ language }),
    }),
    { name: 'language-storage', version: 1 }
  )
);
