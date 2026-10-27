/**
 * Split the current folder path into display segments for the titlebar,
 * e.g. `photos/2024/` → `['photos', '2024']`. The bucket root (`''` or `'/'`)
 * has no segments, so the titlebar shows the bucket name alone.
 */
export function titlebarPathSegments(path: string | null | undefined): string[] {
  if (!path) return [];
  return path.split('/').filter((segment) => segment.length > 0);
}

/** Full `bucket/folder/` path, used as the titlebar tooltip. */
export function titlebarPathTitle(bucket: string, path: string | null | undefined): string {
  const segments = titlebarPathSegments(path);
  return segments.length === 0 ? `${bucket}/` : `${bucket}/${segments.join('/')}/`;
}
