'use client';

import { useCallback, useEffect, useRef, useState, type PointerEvent } from 'react';
import { Button, Spin, Tooltip } from 'antd';
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
  if (!ctx) throw new Error('Canvas is unavailable');
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
        if (!cancelled) setError('The image could not be decoded, so it cannot be edited.');
      });
    return () => {
      cancelled = true;
    };
  }, [bytes, mime]);

  // A save makes the current state the clean one.
  useEffect(() => {
    if (savedRevision === 0) return;
    setHistory((h) => (h.length ? [h[h.length - 1]] : h));
    setBaseline(0);
  }, [savedRevision]);

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
          if (!current) return reject(new Error('The image is still loading'));
          current.toBlob(
            (blob) => {
              if (!blob) return reject(new Error('The image could not be encoded'));
              // WebKit falls back to PNG for formats it cannot write; that
              // would store PNG bytes under a .webp/.jpg name.
              if (blob.type !== mime) {
                return reject(
                  new Error(
                    `This system cannot write ${mime.replace('image/', '').toUpperCase()} images.`
                  )
                );
              }
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

  // The selection, drawn over the scaled canvas.
  const overlay = (() => {
    const view = viewRef.current;
    const stage = stageRef.current;
    if (!selection || !view || !stage || !current) return null;
    const box = view.getBoundingClientRect();
    const stageBox = stage.getBoundingClientRect();
    const scale = box.width / current.width;
    return {
      left: box.left - stageBox.left + selection.x * scale,
      top: box.top - stageBox.top + selection.y * scale,
      width: selection.width * scale,
      height: selection.height * scale,
    };
  })();

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
      <div className="file-editor-toolbar" role="toolbar" aria-label="Image tools">
        {cropping ? (
          <>
            <span className="file-editor-hint">
              {selection
                ? `${selection.width} × ${selection.height} px selected`
                : 'Drag across the image to choose the area to keep'}
            </span>
            <Button
              size="small"
              type="primary"
              icon={<CheckOutlined />}
              onClick={finishCrop}
              disabled={!selection}
            >
              Crop to selection
            </Button>
            <Button
              size="small"
              icon={<CloseOutlined />}
              onClick={() => {
                setCropping(false);
                setDrag(null);
              }}
            >
              Cancel
            </Button>
          </>
        ) : (
          <>
            <Tooltip title="Rotate left">
              <Button
                size="small"
                icon={<RotateLeftOutlined />}
                onClick={() => run({ type: 'rotate', degrees: -90 })}
                aria-label="Rotate left"
                disabled={!current}
              />
            </Tooltip>
            <Tooltip title="Rotate right">
              <Button
                size="small"
                icon={<RotateRightOutlined />}
                onClick={() => run({ type: 'rotate', degrees: 90 })}
                aria-label="Rotate right"
                disabled={!current}
              />
            </Tooltip>
            <Tooltip title="Flip horizontally">
              <Button
                size="small"
                icon={<SwapOutlined />}
                onClick={() => run({ type: 'flip', axis: 'horizontal' })}
                aria-label="Flip horizontally"
                disabled={!current}
              />
            </Tooltip>
            <Tooltip title="Flip vertically">
              <Button
                size="small"
                icon={<SwapOutlined rotate={90} />}
                onClick={() => run({ type: 'flip', axis: 'vertical' })}
                aria-label="Flip vertically"
                disabled={!current}
              />
            </Tooltip>
            <Button
              size="small"
              icon={<ScissorOutlined />}
              onClick={() => setCropping(true)}
              disabled={!current}
            >
              Crop
            </Button>
            <span className="spacer" />
            <Tooltip title="Undo">
              <Button
                size="small"
                icon={<UndoOutlined />}
                onClick={undo}
                disabled={history.length <= 1}
                aria-label="Undo"
              />
            </Tooltip>
            <Tooltip title="Back to the saved image">
              <Button
                size="small"
                icon={<CompressOutlined />}
                onClick={() => setHistory((h) => h.slice(0, baseline + 1))}
                disabled={edits === 0 || baseline < 0}
              >
                Revert
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
        className={`file-editor-canvas-stage${cropping ? 'is-cropping' : ''}`}
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
        <canvas ref={viewRef} aria-label="Image being edited" hidden={!current} />
        {overlay && <div className="file-editor-crop" style={overlay} />}
      </div>
    </div>
  );
}
