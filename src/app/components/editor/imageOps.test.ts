import { describe, expect, test } from 'bun:test';
import { encoderQuality, selectionRect, sizeAfter, toImagePoint } from './imageOps';

describe('imageOps', () => {
  test('rotating a quarter turn swaps the sides; flips and half turns keep them', () => {
    const size = { width: 400, height: 300 };
    expect(sizeAfter({ type: 'rotate', degrees: 90 }, size)).toEqual({ width: 300, height: 400 });
    expect(sizeAfter({ type: 'rotate', degrees: -90 }, size)).toEqual({ width: 300, height: 400 });
    expect(sizeAfter({ type: 'rotate', degrees: 180 }, size)).toEqual(size);
    expect(sizeAfter({ type: 'flip', axis: 'vertical' }, size)).toEqual(size);
    expect(
      sizeAfter({ type: 'crop', rect: { x: 10, y: 10, width: 50, height: 40 } }, size)
    ).toEqual({ width: 50, height: 40 });
  });

  test('a drag in any direction selects inside the image', () => {
    const bounds = { width: 100, height: 100 };
    expect(selectionRect({ x: 80, y: 90 }, { x: 20, y: 10 }, bounds)).toEqual({
      x: 20,
      y: 10,
      width: 60,
      height: 80,
    });
    expect(selectionRect({ x: -20, y: -5 }, { x: 150, y: 50 }, bounds)).toEqual({
      x: 0,
      y: 0,
      width: 100,
      height: 50,
    });
  });

  test('a click is not a selection', () => {
    expect(
      selectionRect({ x: 10, y: 10 }, { x: 12, y: 11 }, { width: 100, height: 100 })
    ).toBeNull();
  });

  test('screen points map to image pixels through the display scale', () => {
    const point = toImagePoint(
      { x: 150, y: 75 },
      { x: 100, y: 50, width: 200, height: 100 },
      {
        width: 2000,
        height: 1000,
      }
    );
    expect(point).toEqual({ x: 500, y: 250 });
  });

  test('lossy formats are written at high quality; PNG has no quality', () => {
    expect(encoderQuality('image/jpeg')).toBe(0.92);
    expect(encoderQuality('image/webp')).toBe(0.92);
    expect(encoderQuality('image/png')).toBeUndefined();
  });
});
