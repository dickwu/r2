'use client';

import React from 'react';
import { useTranslation } from 'react-i18next';
import { createPortal } from 'react-dom';
import { CheckOutlined, CloseOutlined, ArrowRightOutlined } from '@ant-design/icons';
import { useToastStore, type ToastKind } from '@/app/stores/toastStore';

function ToastIcon({ kind }: { kind: ToastKind }) {
  if (kind === 'success') return <CheckOutlined />;
  if (kind === 'error') return <CloseOutlined />;
  return <ArrowRightOutlined />;
}

export default function Toast() {
  const { t } = useTranslation();
  const toasts = useToastStore((s) => s.toasts);
  const dismissToast = useToastStore((s) => s.dismissToast);

  if (typeof window === 'undefined') return null;

  return createPortal(
    <div className="toast-stack">
      {toasts.map((toast) => (
        <div key={toast.id} className={`toast ${toast.kind}`} role="alert">
          <span className="toast-icon">
            <ToastIcon kind={toast.kind} />
          </span>
          {toast.text}
          <button
            type="button"
            className="hit-target"
            onClick={() => dismissToast(toast.id)}
            aria-label={t('a11y.dismissNotification')}
          />
        </div>
      ))}
    </div>,
    document.body
  );
}
