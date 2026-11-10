'use client';

import { t as i18nT } from '@/i18n';
import { useTranslation } from 'react-i18next';
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type PointerEvent,
} from 'react';
import { Button, Spin, Tooltip } from 'antd';
import { imageEncodingProblem } from '@/app/components/editor/editedContent';
import {
  CheckOutlined,
  CloseOutlined,
  CompressOutlined,
  RotateLeftOutlined,
  RotateRightOutlined,
  ScissorOutlined,
  SwapOutlined,
  UndoOutlined,
} from '@ant-design/icons';
import {
  encoderQuality,
  selectionRect,
  sizeAfter,
  toImagePoint,
  type ImageOp,
  type Point,
  type Rect,
} from '@/app/components/editor/imageOps';

export interface ImageEditorHandle {
  /** The edited image, encoded in its original format. */
  encode: () => Promise<Uint8Array>;
}

interface ImageEditorProps {
  bytes: Uint8Array;
  mime: string;
  height: number;
  /** The number of edits since the last save (0 = clean). */
  onEditsChange: (edits: number) => void;
  handleRef: { current: ImageEditorHandle | null };
  /** Bumped after a save: the current image becomes the new baseline. */
  savedRevision: number;
}

/** Undo steps kept; each is a full-resolution canvas. */
const MAX_HISTORY = 12;

function apply(op: ImageOp, source: HTMLCanvasElement): HTMLCanvasElement {
  const size = sizeAfter(op, { width: source.width, height: source.height });
  const canvas = document.createElement('canvas');
  canvas.width = size.width;
  canvas.height = size.height;
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error(i18nT('editor.image.noCanvas'));
  switch (op.type) {
    case 'rotate':
      ctx.translate(size.width / 2, size.height / 2);
      ctx.rotate((op.degrees * Math.PI) / 180);
      ctx.drawImage(source, -source.width / 2, -source.height / 2);
      break;
    case 'flip':
      if (op.axis === 'horizontal') {
        ctx.translate(size.width, 0);
        ctx.scale(-1, 1);
      } else {
        ctx.translate(0, size.height);
        ctx.scale(1, -1);
      }
      ctx.drawImage(source, 0, 0);
      break;
    case 'crop':
      ctx.drawImage(
        source,
        op.rect.x,
        op.rect.y,
        op.rect.width,
        op.rect.height,
        0,
        0,
        op.rect.width,
        op.rect.height
      );
      break;
  }
  return canvas;
}

/** Rotate, flip and crop on a canvas; saved back in the same format. */
export default function ImageEditor({
  bytes,
  mime,
  height,
  onEditsChange,
  handleRef,
  savedRevision,
}: ImageEditorProps) {
  const { t } = useTranslation();
  const [history, setHistory] = useState<HTMLCanvasElement[]>([]);
  const [baseline, setBaseline] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [cropping, setCropping] = useState(false);
  const [drag, setDrag] = useState<{ start: Point; end: Point } | null>(null);
  const viewRef = useRef<HTMLCanvasElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const current = history[history.length - 1] ?? null;

  // Decode once.
  useEffect(() => {
    let cancelled = false;
    createImageBitmap(new Blob([bytes as BlobPart], { type: mime }))
      .then((bitmap) => {
        if (cancelled) return;
        const canvas = document.createElement('canvas');
        canvas.width = bitmap.width;
        canvas.height = bitmap.height;
        canvas.getContext('2d')?.drawImage(bitmap, 0, 0);
        bitmap.close();
        setHistory([canvas]);
        setBaseline(0);
      })
      .catch(() => {
        if (!cancelled) setError(i18nT('editor.image.decodeFailed'));
      });
    return () => {
      cancelled = true;
    };
  }, [bytes, mime]);

  // A save makes the current state the clean one.
  const [appliedRevision, setAppliedRevision] = useState(savedRevision);
  if (savedRevision !== appliedRevision) {
    setAppliedRevision(savedRevision);
    if (savedRevision !== 0) {
      setHistory((h) => (h.length ? [h[h.length - 1]] : h));
      setBaseline(0);
    }
  }

  const edits = Math.max(history.length - 1 - baseline, 0);
  useEffect(() => onEditsChange(edits), [edits, onEditsChange]);

  // Show the current canvas.
  useEffect(() => {
    const view = viewRef.current;
    if (!view || !current) return;
    view.width = current.width;
    view.height = current.height;
    view.getContext('2d')?.drawImage(current, 0, 0);
  }, [current]);

  useEffect(() => {
    handleRef.current = {
      encode: () =>
        new Promise<Uint8Array>((resolve, reject) => {
          if (!current) return reject(new Error(i18nT('editor.modal.imageStillLoading')));
          current.toBlob(
            (blob) => {
              if (!blob) return reject(new Error(i18nT('editor.image.encodeFailed')));
              const problem = imageEncodingProblem(blob.type, mime);
              if (problem) return reject(new Error(problem));
              blob.arrayBuffer().then((buffer) => resolve(new Uint8Array(buffer)), reject);
            },
            mime,
            encoderQuality(mime)
          );
        }),
    };
    return () => {
      handleRef.current = null;
    };
  }, [current, mime, handleRef]);

  const run = useCallback(
    (op: ImageOp) => {
      if (!current) return;
      const next = apply(op, current);
      const grown = [...history, next];
      if (grown.length > MAX_HISTORY) {
        // The oldest step falls off; the saved state may fall off with it.
        setBaseline(baseline - 1);
        grown.shift();
      }
      setHistory(grown);
    },
    [current, history, baseline]
  );

  const undo = () => setHistory((h) => (h.length > 1 ? h.slice(0, -1) : h));

  const imagePoint = (event: PointerEvent): Point | null => {
    const view = viewRef.current;
    if (!view || !current) return null;
    const box = view.getBoundingClientRect();
    return toImagePoint(
      { x: event.clientX, y: event.clientY },
      { x: box.left, y: box.top, width: box.width, height: box.height },
      { width: current.width, height: current.height }
    );
  };

  const selection: Rect | null =
    drag && current
      ? selectionRect(drag.start, drag.end, { width: current.width, height: current.height })
      : null;

  // Position the crop overlay from layout measurements, not during render.
  const cropRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = cropRef.current;
    const view = viewRef.current;
    const stage = stageRef.current;
    if (!el) return;
    if (!selection || !view || !stage || !current) {
      el.hidden = true;
      return;
    }
    const box = view.getBoundingClientRect();
    const stageBox = stage.getBoundingClientRect();
    const scale = box.width / current.width;
    el.hidden = false;
    el.style.left = `${box.left - stageBox.left + selection.x * scale}px`;
    el.style.top = `${box.top - stageBox.top + selection.y * scale}px`;
    el.style.width = `${selection.width * scale}px`;
    el.style.height = `${selection.height * scale}px`;
  }, [selection, current]);

  const finishCrop = () => {
    if (selection) run({ type: 'crop', rect: selection });
    setCropping(false);
    setDrag(null);
  };

  if (error) {
    return <p className="file-editor-message">{error}</p>;
  }

  return (
    <div className="file-editor" style={{ height }}>
      <div className="file-editor-toolbar" role="toolbar" aria-label={t('editor.image.tools')}>
        {cropping ? (
          <>
            <span className="file-editor-hint">
              {selection
                ? t('editor.image.selected', { width: selection.width, height: selection.height })
                : t('editor.image.dragHint')}
            </span>
            <Button
              size="small"
              type="primary"
              icon={<CheckOutlined />}
              onClick={finishCrop}
              disabled={!selection}
            >
              {t('editor.image.cropToSelection')}
            </Button>
            <Button
              size="small"
              icon={<CloseOutlined />}
              onClick={() => {
                setCropping(false);
                setDrag(null);
              }}
            >
              {t('common.cancel')}
            </Button>
          </>
        ) : (
          <>
            <Tooltip title={t('editor.image.rotateLeft')}>
              <Button
                size="small"
                icon={<RotateLeftOutlined />}
                onClick={() => run({ type: 'rotate', degrees: -90 })}
                aria-label={t('editor.image.rotateLeft')}
                disabled={!current}
              />
            </Tooltip>
            <Tooltip title={t('editor.image.rotateRight')}>
              <Button
                size="small"
                icon={<RotateRightOutlined />}
                onClick={() => run({ type: 'rotate', degrees: 90 })}
                aria-label={t('editor.image.rotateRight')}
                disabled={!current}
              />
            </Tooltip>
            <Tooltip title={t('editor.image.flipH')}>
              <Button
                size="small"
                icon={<SwapOutlined />}
                onClick={() => run({ type: 'flip', axis: 'horizontal' })}
                aria-label={t('editor.image.flipH')}
                disabled={!current}
              />
            </Tooltip>
            <Tooltip title={t('editor.image.flipV')}>
              <Button
                size="small"
                icon={<SwapOutlined rotate={90} />}
                onClick={() => run({ type: 'flip', axis: 'vertical' })}
                aria-label={t('editor.image.flipV')}
                disabled={!current}
              />
            </Tooltip>
            <Button
              size="small"
              icon={<ScissorOutlined />}
              onClick={() => setCropping(true)}
              disabled={!current}
            >
              {t('editor.image.crop')}
            </Button>
            <span className="spacer" />
            <Tooltip title={t('editor.image.undo')}>
              <Button
                size="small"
                icon={<UndoOutlined />}
                onClick={undo}
                disabled={history.length <= 1}
                aria-label={t('editor.image.undo')}
              />
            </Tooltip>
            <Tooltip title={t('editor.image.revertHint')}>
              <Button
                size="small"
                icon={<CompressOutlined />}
                onClick={() => setHistory((h) => h.slice(0, baseline + 1))}
                disabled={edits === 0 || baseline < 0}
              >
                {t('editor.image.revert')}
              </Button>
            </Tooltip>
          </>
        )}
        {current && (
          <span className="file-editor-status">
            {current.width} × {current.height} px
          </span>
        )}
      </div>
      <div
        ref={stageRef}
        className={cropping ? 'file-editor-canvas-stage is-cropping' : 'file-editor-canvas-stage'}
        style={{ flex: 1 }}
        onPointerDown={(event) => {
          if (!cropping) return;
          const point = imagePoint(event);
          if (!point) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          setDrag({ start: point, end: point });
        }}
        onPointerMove={(event) => {
          if (!cropping || !drag || event.buttons === 0) return;
          const point = imagePoint(event);
          if (point) setDrag({ start: drag.start, end: point });
        }}
      >
        {!current && <Spin />}
        <canvas ref={viewRef} aria-label={t('editor.image.canvas')} hidden={!current} />
        <div ref={cropRef} className="file-editor-crop" hidden />
      </div>
    </div>
  );
}
