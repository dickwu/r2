'use client';

import { useEffect, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { CloseOutlined } from '@ant-design/icons';

export interface ModalProps {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  subtitle?: ReactNode;
  icon?: ReactNode;
  width?: number;
  footer?: ReactNode;
  children: ReactNode;
  bodyPadding?: number | string;
}

export default function Modal({
  open,
  onClose,
  title,
  subtitle,
  icon,
  width = 560,
  footer,
  children,
  bodyPadding,
}: ModalProps) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [open, onClose]);

  if (!open) return null;

  const content = (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="modal" style={{ width }}>
        <div className="modal-header">
          <div>
            <div className="modal-title">
              {icon}
              {title}
            </div>
            {subtitle && <div className="modal-subtitle">{subtitle}</div>}
          </div>
          <button className="modal-close" onClick={onClose} aria-label="Close">
            <CloseOutlined style={{ fontSize: 14 }} />
          </button>
        </div>
        <div
          className="modal-body"
          style={bodyPadding != null ? { padding: bodyPadding } : undefined}
        >
          {children}
        </div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>
  );

  if (typeof document === 'undefined') return content;
  return createPortal(content, document.body);
}
