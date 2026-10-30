/**
 * Public access a bucket should have after its custom domain is edited.
 *
 * Direct links only use a custom domain while the bucket is public. Typing a
 * domain into a bucket that had none therefore turns Public on, so the saved
 * domain drives links at once. Later edits keep whatever the user chose,
 * including an explicit Private with a domain still attached.
 */
export function publicAfterDomainEdit(
  prevHost: string | null | undefined,
  nextHost: string | null | undefined,
  isPublic: boolean | null | undefined
): boolean {
  if (isPublic) return true;
  return !prevHost?.trim() && !!nextHost?.trim();
}
