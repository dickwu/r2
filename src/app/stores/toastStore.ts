import { create } from 'zustand';

export type ToastKind = 'success' | 'error' | 'info';

export interface Toast {
  id: string;
  text: string;
  kind: ToastKind;
}

interface ToastStore {
  toasts: Toast[];
  pushToast: (text: string, kind?: ToastKind) => void;
  dismissToast: (id: string) => void;
}

let counter = 0;

export const useToastStore = create<ToastStore>((set, get) => ({
  toasts: [],

  pushToast: (text, kind = 'info') => {
    const id = `toast-${Date.now()}-${++counter}`;
    set((state) => ({ toasts: [...state.toasts, { id, text, kind }] }));
    setTimeout(() => get().dismissToast(id), 2400);
  },

  dismissToast: (id) => {
    set((state) => ({ toasts: state.toasts.filter((t) => t.id !== id) }));
  },
}));
