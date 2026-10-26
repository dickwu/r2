/**
 * Saving an edited object without clobbering someone else's write.
 *
 * The editor remembers the ETag of the version it opened. Before writing it
 * asks the server for the current ETag and stops on a mismatch; the write
 * itself carries `If-Match`, so a change landing between the check and the PUT
 * is refused by the server (412) too. Servers that reject the condition
 * outright (501) get a plain PUT, the version check having passed.
 */

/** An ETag without weak marker or quotes, for comparing two of them. */
export function normalizeEtag(etag: string | null | undefined): string | null {
  if (!etag) return null;
  const trimmed = etag
    .trim()
    .replace(/^W\//, '')
    .replace(/^"(.*)"$/, '$1');
  return trimmed === '' ? null : trimmed;
}

/** Whether two ETags name different versions (unknown on either side is not a conflict). */
export function etagsDiffer(a: string | null | undefined, b: string | null | undefined): boolean {
  const left = normalizeEtag(a);
  const right = normalizeEtag(b);
  return left !== null && right !== null && left !== right;
}

export type SaveErrorKind =
  'conflict' | 'forbidden' | 'condition-unsupported' | 'too-large' | 'other';

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return typeof error === 'string' ? error : JSON.stringify(error);
}

/** Sorts a write error (the backend leads with `HTTP <status>`) into what the UI says about it. */
export function classifySaveError(error: unknown): SaveErrorKind {
  const message = errorMessage(error);
  if (/HTTP 412\b|PreconditionFailed|HTTP 409\b|ConditionalRequestConflict/i.test(message)) {
    return 'conflict';
  }
  if (/HTTP 403\b|AccessDenied|\bForbidden\b|not authorized/i.test(message)) return 'forbidden';
  if (/HTTP 501\b|NotImplemented/i.test(message)) return 'condition-unsupported';
  if (/HTTP 413\b|EntityTooLarge/i.test(message)) return 'too-large';
  return 'other';
}

/** What to tell the user about a failed save. */
export function describeSaveError(kind: SaveErrorKind, message: string): string {
  switch (kind) {
    case 'conflict':
      return 'The file changed on the server after you opened it.';
    case 'forbidden':
      return 'These credentials cannot write to this bucket (they may be read-only). Your changes are still in the editor.';
    case 'too-large':
      return 'The server refused the file as too large.';
    case 'condition-unsupported':
      return 'The server does not support conditional writes.';
    default:
      return message;
  }
}

export interface SaveDependencies {
  /** The ETag of the object on the server now (null when it cannot tell). */
  currentEtag: () => Promise<string | null>;
  /** Writes the new content, conditional on `ifMatch` when given; resolves to the new ETag. */
  upload: (ifMatch?: string) => Promise<string>;
}

export type SaveOutcome =
  | { status: 'saved'; etag: string | null }
  | { status: 'conflict'; serverEtag: string | null }
  | { status: 'error'; kind: SaveErrorKind; message: string };

/**
 * Saves over the version `expectedEtag` names. `force` overwrites whatever is
 * there (after the user chose to, on a conflict).
 */
export async function saveWithVersionCheck(
  expectedEtag: string | null,
  deps: SaveDependencies,
  options: { force?: boolean } = {}
): Promise<SaveOutcome> {
  const guard = !options.force && normalizeEtag(expectedEtag) !== null;

  if (guard) {
    let serverEtag: string | null = null;
    try {
      serverEtag = await deps.currentEtag();
    } catch {
      // The conditional PUT below still protects the write.
    }
    if (etagsDiffer(serverEtag, expectedEtag)) return { status: 'conflict', serverEtag };
  }

  const ifMatch = guard ? `"${normalizeEtag(expectedEtag)}"` : undefined;
  try {
    return { status: 'saved', etag: normalizeEtag(await deps.upload(ifMatch)) };
  } catch (error) {
    const kind = classifySaveError(error);
    if (kind === 'conflict') return { status: 'conflict', serverEtag: null };
    if (kind === 'condition-unsupported' && ifMatch) {
      try {
        return { status: 'saved', etag: normalizeEtag(await deps.upload(undefined)) };
      } catch (retryError) {
        const retryKind = classifySaveError(retryError);
        const message = errorMessage(retryError);
        return { status: 'error', kind: retryKind, message: describeSaveError(retryKind, message) };
      }
    }
    const message = errorMessage(error);
    return { status: 'error', kind, message: describeSaveError(kind, message) };
  }
}
