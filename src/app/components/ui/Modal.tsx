'use client';

import { useEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { CloseOutlined } from '@ant-design/icons';

/**
 * For a dialog that can open over anything, antd modals included: above antd's
 * popup layer (zIndexPopupBase 1000, +100 per nested container) and below its
 * message and notification toasts (2010 and up), which stay readable on top.
 * Only for dialogs with no antd popups (Select, Tooltip…) inside — those would
 * render beneath this backdrop.
 */
export const ABOVE_ANTD_MODALS_Z_INDEX = 1500;

interface OpenModal {
  /** A ref to the modal's latest onClose. */
  readonly close: { readonly current: () => void };
  /** Raised above antd's modal layer, so over an antd modal it is the one on screen. */
  readonly elevated: boolean;
}

/**
 * Every open modal, bottom to top. Escape closes only the topmost, so stacked
 * modals (a failure modal over an upload or delete modal) peel off one press
 * at a time. One shared document listener picks the top at the moment of the
 * key press: per-modal listeners could double-close, because React flushes
 * the top modal's close between listener callbacks and a lower modal would
 * then find itself on top.
 */
let openModals: ReadonlyArray<OpenModal> = [];

const closeTopModal = (event: KeyboardEvent) => {
  if (event.key !== 'Escape') return;
  const top = openModals[openModals.length - 1];
  if (!top) return;
  // antd keeps its own Escape stack on window, which runs after this document
  // listener and would close the antd modal beneath as well. A modal raised
  // above antd's layer is the one on screen, so the key stops here.
  if (top.elevated) event.stopPropagation();
  // Overlays inside the body (the file viewer's lightboxes, slideshow and
  // password prompt) listen on document too, registered after this listener,
  // and claim the key with preventDefault. Decide once the dispatch is over
  // so Escape peels off the overlay first and the modal on the next press.
  setTimeout(() => {
    if (!event.defaultPrevented) top.close.current();
  }, 0);
};

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
  /** Backdrop stacking; the stylesheet's `.modal-backdrop` layer (100) when unset. */
  zIndex?: number;
  /** Fills the whole window instead of floating at `width`. */
  fullscreen?: boolean;
  /** Buttons shown in the header, left of the close button. */
  headerActions?: ReactNode;
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
  zIndex,
  fullscreen = false,
  headerActions,
}: ModalProps) {
  // The latest onClose, so a parent passing a fresh arrow on every render
  // neither re-registers this modal nor moves it within the stack.
  const latestOnClose = useRef(onClose);
  useEffect(() => {
    latestOnClose.current = onClose;
  }, [onClose]);

  // Pushed when it opens, removed when it closes or unmounts.
  const elevated = zIndex != null && zIndex >= ABOVE_ANTD_MODALS_Z_INDEX;
  useEffect(() => {
    if (!open) return;
    const entry: OpenModal = { close: latestOnClose, elevated };
    if (openModals.length === 0) document.addEventListener('keydown', closeTopModal);
    openModals = [...openModals, entry];
    return () => {
      openModals = openModals.filter((modal) => modal !== entry);
      if (openModals.length === 0) document.removeEventListener('keydown', closeTopModal);
    };
  }, [open, elevated]);

  if (!open) return null;

  const content = (
    <div
      className={fullscreen ? 'modal-backdrop modal-backdrop-fullscreen' : 'modal-backdrop'}
      style={zIndex != null ? { zIndex } : undefined}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className={fullscreen ? 'modal modal-fullscreen' : 'modal'}
        style={fullscreen ? undefined : { width }}
      >
        <div className="modal-header">
          <div>
            <div className="modal-title">
              {icon}
              {title}
            </div>
            {subtitle && <div className="modal-subtitle">{subtitle}</div>}
          </div>
          <div className="modal-header-actions">
            {headerActions}
            <button className="modal-close" onClick={onClose} aria-label="Close">
              <CloseOutlined style={{ fontSize: 14 }} />
            </button>
          </div>
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
