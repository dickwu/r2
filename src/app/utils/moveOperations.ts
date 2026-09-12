/** A batch must never contain two writers for the same destination key. */
export function buildMoveOperations(keys: Iterable<string>, targetDirectory: string) {
  const prefix = targetDirectory.replace(/^\/+|\/+$/g, '');
  const destinations = new Set<string>();
  return Array.from(keys, (key) => {
    const filename = key.split('/').pop() || key;
    const destination = prefix ? `${prefix}/${filename}` : filename;
    if (destinations.has(destination)) {
      throw new Error(
        `Multiple selected files would become ${destination}. Move them separately to different folders.`
      );
    }
    destinations.add(destination);
    return { source_key: key, dest_key: destination, overwrite: false };
  });
}
