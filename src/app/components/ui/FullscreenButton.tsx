'use client';

import { useTranslation } from 'react-i18next';
import { Tooltip } from 'antd';
import { FullscreenExitOutlined, FullscreenOutlined } from '@ant-design/icons';

interface FullscreenButtonProps {
  fullscreen: boolean;
  onToggle: () => void;
  /** Mentions the `F` shortcut in the tooltip where it works. */
  shortcutHint?: boolean;
}

/** The header button that enters and leaves fullscreen, shared by the preview and the editor. */
export default function FullscreenButton({
  fullscreen,
  onToggle,
  shortcutHint = false,
}: FullscreenButtonProps) {
  const { t } = useTranslation();
  const label = fullscreen ? t('fullscreen.exit') : t('fullscreen.enter');
  const keys = fullscreen ? 'Esc' : shortcutHint ? 'F' : null;
  return (
    <Tooltip title={keys ? `${label} (${keys})` : label} placement="bottom">
      <button
        type="button"
        className="modal-close"
        onClick={onToggle}
        aria-label={label}
        aria-pressed={fullscreen}
      >
        {fullscreen ? (
          <FullscreenExitOutlined style={{ fontSize: 14 }} />
        ) : (
          <FullscreenOutlined style={{ fontSize: 14 }} />
        )}
      </button>
    </Tooltip>
  );
}
