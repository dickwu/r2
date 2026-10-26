/** A text or binary body for the `upload_*_content` commands. */
export type ContentBody = string | Uint8Array;

export interface UploadContentOptions {
  /** Only overwrite the object if its ETag still matches (S3 `If-Match`). */
  ifMatch?: string;
}

/** Base64 for bytes, built in chunks so large files don't overflow the call stack. */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

/**
 * The command arguments carrying a body: text goes as `content`, bytes (edited
 * images, text with a BOM or in UTF-16) as `contentBase64`, which the backend
 * decodes back to exactly those bytes.
 */
export function contentArgs(
  content: ContentBody,
  options: UploadContentOptions = {}
): { content?: string; contentBase64?: string; ifMatch?: string } {
  const body =
    typeof content === 'string' ? { content } : { contentBase64: bytesToBase64(content) };
  return options.ifMatch ? { ...body, ifMatch: options.ifMatch } : body;
}
