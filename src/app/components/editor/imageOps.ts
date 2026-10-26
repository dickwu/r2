/** Geometry for the image editor's rotate, flip and crop, kept free of the DOM. */

export interface Size {
  width: number;
  height: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

export type ImageOp =
  | { type: 'rotate'; degrees: 90 | -90 | 180 }
  | { type: 'flip'; axis: 'horizontal' | 'vertical' }
  | { type: 'crop'; rect: Rect };

/** The canvas size after an operation. */
export function sizeAfter(op: ImageOp, size: Size): Size {
  switch (op.type) {
    case 'rotate':
      return op.degrees === 180 ? size : { width: size.height, height: size.width };
    case 'flip':
      return size;
    case 'crop':
      return { width: op.rect.width, height: op.rect.height };
  }
}

/**
 * The selection a drag from `start` to `end` makes, in image pixels: either
 * direction works, and it stays inside the image. Null when it is too small to
 * be meant as a crop (a click rather than a drag).
 */
export function selectionRect(start: Point, end: Point, bounds: Size, minSize = 4): Rect | null {
  const clampX = (v: number) => Math.min(Math.max(v, 0), bounds.width);
  const clampY = (v: number) => Math.min(Math.max(v, 0), bounds.height);
  const x1 = Math.round(clampX(Math.min(start.x, end.x)));
  const y1 = Math.round(clampY(Math.min(start.y, end.y)));
  const x2 = Math.round(clampX(Math.max(start.x, end.x)));
  const y2 = Math.round(clampY(Math.max(start.y, end.y)));
  const width = x2 - x1;
  const height = y2 - y1;
  if (width < minSize || height < minSize) return null;
  return { x: x1, y: y1, width, height };
}

/** A point on the displayed (scaled) canvas, in the image's own pixels. */
export function toImagePoint(client: Point, displayed: Rect, image: Size): Point {
  const scaleX = displayed.width > 0 ? image.width / displayed.width : 1;
  const scaleY = displayed.height > 0 ? image.height / displayed.height : 1;
  return { x: (client.x - displayed.x) * scaleX, y: (client.y - displayed.y) * scaleY };
}

/** The canvas encoders the editor can rely on to write the same format back. */
export function encoderQuality(mime: string): number | undefined {
  return mime === 'image/jpeg' || mime === 'image/webp' ? 0.92 : undefined;
}
