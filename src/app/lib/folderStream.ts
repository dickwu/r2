import {
  createFolderPageAccumulator,
  type FolderPage,
  type FolderRequestScope,
  type FolderSnapshot,
} from '@/app/utils/folderItems';

interface FolderStreamTransport {
  listen: (receive: (page: FolderPage) => void) => Promise<() => void>;
  start: () => Promise<unknown>;
  cancel: () => Promise<unknown>;
}

/** Subscribe before invoking, and stop both event acceptance and native work on navigation. */
export async function readFolderStream(
  scope: FolderRequestScope,
  transport: FolderStreamTransport,
  onUpdate: (snapshot: FolderSnapshot) => void,
  signal?: AbortSignal
): Promise<FolderSnapshot> {
  signal?.throwIfAborted();
  const accumulator = createFolderPageAccumulator(scope);
  let active = true;
  let unlisten: (() => void) | undefined;
  let deliveryTimer: ReturnType<typeof setTimeout> | undefined;
  let resolveFinal!: (snapshot: FolderSnapshot) => void;
  let rejectFinal!: (reason: unknown) => void;
  const finalPage = new Promise<FolderSnapshot>((resolve, reject) => {
    resolveFinal = resolve;
    rejectFinal = reject;
  });
  // A callback can reject before listener installation or invoke resolves.
  void finalPage.catch(() => {});
  const cancel = () => {
    active = false;
    rejectFinal(signal?.reason ?? new DOMException('Folder listing cancelled', 'AbortError'));
    void transport.cancel().catch(() => {});
  };
  signal?.addEventListener('abort', cancel, { once: true });

  try {
    unlisten = await transport.listen((page) => {
      if (!active || signal?.aborted) return;
      try {
        const snapshot = accumulator.accept(page);
        if (!snapshot) return;
        onUpdate(snapshot);
        if (snapshot.complete) resolveFinal(snapshot);
      } catch (error) {
        active = false;
        rejectFinal(error);
        void transport.cancel().catch(() => {});
      }
    });
    signal?.throwIfAborted();
    const started = transport.start();
    // Race against cancellation/protocol errors while the invoke is in flight.
    await Promise.race([started, finalPage.then(() => started)]);
    signal?.throwIfAborted();
    deliveryTimer = setTimeout(
      () => rejectFinal(new Error('Folder listing completed without its final page')),
      5000
    );
    return await finalPage;
  } finally {
    active = false;
    if (deliveryTimer) clearTimeout(deliveryTimer);
    signal?.removeEventListener('abort', cancel);
    unlisten?.();
  }
}
