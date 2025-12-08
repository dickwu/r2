import { fetch } from '@tauri-apps/plugin-http';
import type { R2Config } from '../components/ConfigModal';
import {
  generateUploadStateId,
  getUploadState,
  saveUploadState,
  deleteUploadState,
  type UploadState,
} from './indexeddb';

// Multipart upload threshold: 100MB (use single PUT for smaller files)
const MULTIPART_THRESHOLD = 100 * 1024 * 1024;
// Part size: 20MB per chunk (larger = fewer requests, faster for good connections)
const PART_SIZE = 20 * 1024 * 1024;
// Concurrent uploads: 6 parts in parallel
const CONCURRENCY = 6;
// Prepare chunks ahead: keep this many chunks ready in memory
const PREPARE_AHEAD = CONCURRENCY + 2;

export interface UploadProgress {
  percent: number;
  uploadedBytes: number;
  totalBytes: number;
  speed: number; // bytes per second
}

export class UploadCancelledError extends Error {
  constructor() {
    super('Upload cancelled');
    this.name = 'UploadCancelledError';
  }
}

// Upload a file to R2 using Tauri's fetch (bypasses CORS)
// Uses multipart upload for files > 100MB
export async function uploadR2Object(
  config: R2Config,
  key: string,
  file: File,
  onProgress?: (progress: UploadProgress) => void,
  abortSignal?: AbortSignal
): Promise<void> {
  if (!config.accessKeyId || !config.secretAccessKey) {
    throw new Error(
      'S3 credentials required for upload. Please configure Access Key ID and Secret Access Key.'
    );
  }

  const contentType = file.type || 'application/octet-stream';

  if (file.size < MULTIPART_THRESHOLD) {
    // Small file: single PUT request
    await uploadSinglePart(config, key, file, contentType, onProgress, abortSignal);
  } else {
    // Large file: multipart upload with parallel parts
    await uploadMultipart(config, key, file, contentType, onProgress, abortSignal);
  }
}

// Single PUT upload for small files
async function uploadSinglePart(
  config: R2Config,
  key: string,
  file: File,
  contentType: string,
  onProgress?: (progress: UploadProgress) => void,
  abortSignal?: AbortSignal
): Promise<void> {
  if (abortSignal?.aborted) throw new UploadCancelledError();

  const { S3Client, PutObjectCommand } = await import('@aws-sdk/client-s3');
  const { getSignedUrl } = await import('@aws-sdk/s3-request-presigner');

  const client = new S3Client({
    region: 'auto',
    endpoint: `https://${config.accountId}.r2.cloudflarestorage.com`,
    credentials: {
      accessKeyId: config.accessKeyId!,
      secretAccessKey: config.secretAccessKey!,
    },
  });

  const command = new PutObjectCommand({
    Bucket: config.bucket,
    Key: key,
    ContentType: contentType,
  });

  const presignedUrl = await getSignedUrl(client, command, { expiresIn: 3600 });

  if (abortSignal?.aborted) throw new UploadCancelledError();

  const startTime = Date.now();

  // Use Tauri's fetch (bypasses CORS)
  const arrayBuffer = await file.arrayBuffer();
  if (abortSignal?.aborted) throw new UploadCancelledError();

  const response = await fetch(presignedUrl, {
    method: 'PUT',
    headers: { 'Content-Type': contentType },
    body: arrayBuffer,
    signal: abortSignal,
  });

  if (abortSignal?.aborted) throw new UploadCancelledError();

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Upload failed: ${response.status} - ${text}`);
  }

  const elapsed = (Date.now() - startTime) / 1000;
  const speed = elapsed > 0 ? file.size / elapsed : 0;

  onProgress?.({
    percent: 100,
    uploadedBytes: file.size,
    totalBytes: file.size,
    speed,
  });
}

// Prepared chunk ready for upload
interface PreparedChunk {
  partNumber: number;
  buffer: ArrayBuffer;
  size: number;
}

// Multipart upload for large files with parallel part uploads
// Uses chunk preparation pipeline to avoid upload stalls
// Supports resumable uploads via IndexedDB
async function uploadMultipart(
  config: R2Config,
  key: string,
  file: File,
  contentType: string,
  onProgress?: (progress: UploadProgress) => void,
  abortSignal?: AbortSignal
): Promise<void> {
  if (abortSignal?.aborted) throw new UploadCancelledError();

  const {
    S3Client,
    CreateMultipartUploadCommand,
    UploadPartCommand,
    CompleteMultipartUploadCommand,
    AbortMultipartUploadCommand,
    ListPartsCommand,
  } = await import('@aws-sdk/client-s3');
  const { getSignedUrl } = await import('@aws-sdk/s3-request-presigner');

  const client = new S3Client({
    region: 'auto',
    endpoint: `https://${config.accountId}.r2.cloudflarestorage.com`,
    credentials: {
      accessKeyId: config.accessKeyId!,
      secretAccessKey: config.secretAccessKey!,
    },
  });

  const totalParts = Math.ceil(file.size / PART_SIZE);

  // Generate unique ID for this upload
  const stateId = generateUploadStateId(
    config.accountId,
    config.bucket,
    key,
    file.name,
    file.size,
    file.lastModified
  );

  // Check for existing upload state (resumable)
  let uploadId = '';
  let completedParts: { ETag: string; PartNumber: number }[] = [];
  let isResume = false;

  const existingState = await getUploadState(stateId);

  if (existingState) {
    // Try to resume existing upload
    try {
      // Verify the upload still exists on server
      const listCommand = new ListPartsCommand({
        Bucket: config.bucket,
        Key: key,
        UploadId: existingState.uploadId,
      });
      const listUrl = await getSignedUrl(client, listCommand, { expiresIn: 3600 });
      const listResponse = await fetch(listUrl, { method: 'GET' });

      if (listResponse.ok) {
        // Upload exists, use it
        uploadId = existingState.uploadId;
        completedParts = existingState.completedParts.map((p) => ({
          ETag: p.ETag,
          PartNumber: p.PartNumber,
        }));
        isResume = true;
        console.log(`Resuming upload: ${completedParts.length}/${totalParts} parts completed`);
      } else {
        // Upload expired or invalid, start fresh
        await deleteUploadState(stateId);
      }
    } catch {
      // Can't resume, start fresh
      await deleteUploadState(stateId);
    }
  }

  // Create new multipart upload if not resuming
  if (!uploadId) {
    const createCommand = new CreateMultipartUploadCommand({
      Bucket: config.bucket,
      Key: key,
      ContentType: contentType,
    });
    const createUrl = await getSignedUrl(client, createCommand, { expiresIn: 3600 });

    const createResponse = await fetch(createUrl, { method: 'POST' });
    if (!createResponse.ok) {
      throw new Error(`Failed to initiate multipart upload: ${createResponse.status}`);
    }

    const createXml = await createResponse.text();
    const uploadIdMatch = createXml.match(/<UploadId>(.+?)<\/UploadId>/);
    if (!uploadIdMatch) {
      throw new Error('Failed to parse UploadId from response');
    }
    uploadId = uploadIdMatch[1];

    // Save initial state
    const initialState: UploadState = {
      id: stateId,
      uploadId,
      bucket: config.bucket,
      accountId: config.accountId,
      key,
      fileName: file.name,
      fileSize: file.size,
      fileLastModified: file.lastModified,
      contentType,
      totalParts,
      completedParts: [],
      createdAt: Date.now(),
      updatedAt: Date.now(),
    };
    await saveUploadState(initialState);
  }

  // Helper to abort the multipart upload and clean up state
  async function abortUpload(keepState: boolean = false) {
    try {
      const abortCommand = new AbortMultipartUploadCommand({
        Bucket: config.bucket,
        Key: key,
        UploadId: uploadId,
      });
      const abortUrl = await getSignedUrl(client, abortCommand, { expiresIn: 3600 });
      await fetch(abortUrl, { method: 'DELETE' });
    } catch {
      // Ignore abort errors
    }
    if (!keepState) {
      await deleteUploadState(stateId);
    }
  }

  if (abortSignal?.aborted) {
    // Keep state for resume if user cancelled
    throw new UploadCancelledError();
  }

  // Calculate already uploaded bytes for progress
  const completedPartNumbers = new Set(completedParts.map((p) => p.PartNumber));
  let uploadedBytes = 0;
  for (const partNum of completedPartNumbers) {
    const start = (partNum - 1) * PART_SIZE;
    const end = Math.min(start + PART_SIZE, file.size);
    uploadedBytes += end - start;
  }

  const startTime = Date.now();
  let cancelled = false;

  // Speed tracking with sliding window for real-time speed
  const speedSamples: { time: number; bytes: number }[] = [];
  const SPEED_WINDOW_MS = 2000; // 2 second window for speed calculation

  function calculateSpeed(currentBytes: number): number {
    const now = Date.now();
    speedSamples.push({ time: now, bytes: currentBytes });

    // Remove samples older than window
    while (speedSamples.length > 0 && now - speedSamples[0].time > SPEED_WINDOW_MS) {
      speedSamples.shift();
    }

    if (speedSamples.length < 2) return 0;

    const oldest = speedSamples[0];
    const newest = speedSamples[speedSamples.length - 1];
    const timeDiff = (newest.time - oldest.time) / 1000;
    const bytesDiff = newest.bytes - oldest.bytes;

    return timeDiff > 0 ? bytesDiff / timeDiff : 0;
  }

  abortSignal?.addEventListener('abort', () => {
    cancelled = true;
  });

  // Report initial progress if resuming
  if (isResume && uploadedBytes > 0) {
    onProgress?.({
      percent: Math.round((uploadedBytes / file.size) * 100),
      uploadedBytes,
      totalBytes: file.size,
      speed: 0,
    });
  }

  // Pre-generate all presigned URLs
  const partUrls: string[] = [];
  for (let partNumber = 1; partNumber <= totalParts; partNumber++) {
    const uploadPartCommand = new UploadPartCommand({
      Bucket: config.bucket,
      Key: key,
      UploadId: uploadId,
      PartNumber: partNumber,
    });
    partUrls.push(await getSignedUrl(client, uploadPartCommand, { expiresIn: 3600 }));
  }

  // Chunk preparation pipeline
  const preparedChunks: Map<number, PreparedChunk> = new Map();
  let prepareIndex = 1;
  let preparingCount = 0;
  const prepareWaiters: Map<
    number,
    { resolve: (chunk: PreparedChunk) => void; reject: (e: Error) => void }[]
  > = new Map();

  async function prepareChunk(partNumber: number): Promise<void> {
    if (cancelled || preparedChunks.has(partNumber)) return;

    const start = (partNumber - 1) * PART_SIZE;
    const end = Math.min(start + PART_SIZE, file.size);
    const partBlob = file.slice(start, end);
    const buffer = await partBlob.arrayBuffer();

    if (cancelled) return;

    const chunk: PreparedChunk = { partNumber, buffer, size: end - start };
    preparedChunks.set(partNumber, chunk);

    const waiters = prepareWaiters.get(partNumber);
    if (waiters) {
      waiters.forEach((w) => w.resolve(chunk));
      prepareWaiters.delete(partNumber);
    }
  }

  function getChunk(partNumber: number): Promise<PreparedChunk> {
    const existing = preparedChunks.get(partNumber);
    if (existing) {
      preparedChunks.delete(partNumber);
      return Promise.resolve(existing);
    }

    return new Promise((resolve, reject) => {
      const waiters = prepareWaiters.get(partNumber) || [];
      waiters.push({ resolve, reject });
      prepareWaiters.set(partNumber, waiters);
    });
  }

  async function runPreparer(): Promise<void> {
    while (prepareIndex <= totalParts && !cancelled) {
      // Skip already completed parts
      if (completedPartNumbers.has(prepareIndex)) {
        prepareIndex++;
        continue;
      }

      while (preparingCount >= PREPARE_AHEAD && !cancelled) {
        await new Promise((r) => setTimeout(r, 10));
      }

      if (cancelled) break;

      const partNum = prepareIndex++;
      preparingCount++;

      prepareChunk(partNum).finally(() => {
        preparingCount--;
      });

      await new Promise((r) => setTimeout(r, 0));
    }
  }

  // Save progress to IndexedDB periodically
  let lastSaveTime = Date.now();
  async function saveProgress() {
    const now = Date.now();
    if (now - lastSaveTime < 1000) return; // Save at most once per second
    lastSaveTime = now;

    const state = await getUploadState(stateId);
    if (state) {
      state.completedParts = completedParts.map((p) => ({
        PartNumber: p.PartNumber,
        ETag: p.ETag,
      }));
      await saveUploadState(state);
    }
  }

  async function uploadPart(
    partNumber: number
  ): Promise<{ ETag: string; PartNumber: number } | null> {
    // Skip already completed parts
    if (completedPartNumbers.has(partNumber)) {
      return null;
    }

    if (cancelled) throw new UploadCancelledError();

    const chunk = await getChunk(partNumber);

    if (cancelled) throw new UploadCancelledError();

    const partResponse = await fetch(partUrls[partNumber - 1], {
      method: 'PUT',
      body: chunk.buffer,
      signal: abortSignal,
    });

    if (cancelled) throw new UploadCancelledError();

    if (!partResponse.ok) {
      throw new Error(`Failed to upload part ${partNumber}: ${partResponse.status}`);
    }

    const etag = partResponse.headers.get('ETag');
    if (!etag) {
      throw new Error(`No ETag returned for part ${partNumber}`);
    }

    const result = { ETag: etag, PartNumber: partNumber };
    completedParts.push(result);
    completedPartNumbers.add(partNumber);

    // Update progress
    uploadedBytes += chunk.size;
    const speed = calculateSpeed(uploadedBytes);

    onProgress?.({
      percent: Math.round((uploadedBytes / file.size) * 100),
      uploadedBytes,
      totalBytes: file.size,
      speed,
    });

    // Save progress to IndexedDB
    saveProgress();

    return result;
  }

  try {
    const preparerPromise = runPreparer();

    // Upload remaining parts
    const partNumbers = Array.from({ length: totalParts }, (_, i) => i + 1);
    await promisePool(partNumbers, CONCURRENCY, uploadPart, abortSignal);

    await preparerPromise;

    if (cancelled) throw new UploadCancelledError();

    // Complete multipart upload
    const completeCommand = new CompleteMultipartUploadCommand({
      Bucket: config.bucket,
      Key: key,
      UploadId: uploadId,
      MultipartUpload: {
        Parts: completedParts.sort((a, b) => a.PartNumber - b.PartNumber),
      },
    });
    const completeUrl = await getSignedUrl(client, completeCommand, { expiresIn: 3600 });

    const partsXml = completedParts
      .sort((a, b) => a.PartNumber - b.PartNumber)
      .map((p) => `<Part><PartNumber>${p.PartNumber}</PartNumber><ETag>${p.ETag}</ETag></Part>`)
      .join('');
    const completeXml = `<CompleteMultipartUpload>${partsXml}</CompleteMultipartUpload>`;

    const completeResponse = await fetch(completeUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/xml' },
      body: completeXml,
    });

    if (!completeResponse.ok) {
      const text = await completeResponse.text();
      throw new Error(`Failed to complete multipart upload: ${completeResponse.status} - ${text}`);
    }

    // Success - delete upload state
    await deleteUploadState(stateId);

    const elapsed = (Date.now() - startTime) / 1000;
    onProgress?.({
      percent: 100,
      uploadedBytes: file.size,
      totalBytes: file.size,
      speed: elapsed > 0 ? file.size / elapsed : 0,
    });
  } catch (error) {
    if (error instanceof UploadCancelledError) {
      // User cancelled - keep state for resume
      console.log('Upload cancelled, state saved for resume');
    } else {
      // Error - abort and clean up
      await abortUpload(false);
    }
    throw error;
  }
}

// Simple promise pool for concurrent execution with cancellation support
async function promisePool<T, R>(
  items: T[],
  concurrency: number,
  fn: (item: T) => Promise<R>,
  abortSignal?: AbortSignal
): Promise<R[]> {
  const results: R[] = [];
  const executing: Promise<void>[] = [];
  let error: Error | null = null;

  for (const item of items) {
    // Check abort before starting new work
    if (abortSignal?.aborted || error) break;

    const p = fn(item)
      .then((result) => {
        results.push(result);
      })
      .catch((e) => {
        if (!error) error = e;
      });

    const e: Promise<void> = p.finally(() => {
      const idx = executing.indexOf(e);
      if (idx !== -1) executing.splice(idx, 1);
    });
    executing.push(e);

    if (executing.length >= concurrency) {
      await Promise.race(executing);
      // Check for errors after waiting
      if (error) break;
    }
  }

  // Wait for remaining tasks
  await Promise.all(executing);

  // Throw the first error if any occurred
  if (error) throw error;
  return results;
}
