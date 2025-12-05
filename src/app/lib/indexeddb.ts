import { R2Object } from "./r2api";

const DB_NAME = "r2-uploader";
const DB_VERSION = 2;
const FILES_STORE = "files";
const META_STORE = "meta";

export interface StoredFile {
  key: string;
  size: number;
  lastModified: string;
}

let dbPromise: Promise<IDBDatabase> | null = null;

function openDB(): Promise<IDBDatabase> {
  if (dbPromise) return dbPromise;

  dbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);

    request.onupgradeneeded = (event) => {
      const db = (event.target as IDBOpenDBRequest).result;
      
      // Delete old stores if upgrading
      if (db.objectStoreNames.contains("files")) {
        db.deleteObjectStore("files");
      }
      
      // Files store: flat list of all files, keyed by full path
      const filesStore = db.createObjectStore(FILES_STORE, { keyPath: "key" });
      filesStore.createIndex("key", "key", { unique: true });
      
      // Meta store: for tracking sync state
      if (!db.objectStoreNames.contains(META_STORE)) {
        db.createObjectStore(META_STORE, { keyPath: "id" });
      }
    };
  });

  return dbPromise;
}

// Store all files from a bucket (replaces existing data)
export async function storeAllFiles(files: R2Object[]): Promise<void> {
  const db = await openDB();
  const tx = db.transaction(FILES_STORE, "readwrite");
  const store = tx.objectStore(FILES_STORE);
  
  // Clear existing data
  store.clear();
  
  // Add all files
  for (const file of files) {
    if (!file.key.endsWith("/")) {
      store.put({
        key: file.key,
        size: file.size,
        lastModified: file.last_modified,
      } as StoredFile);
    }
  }
  
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

// Get all files (for full list)
export async function getAllFiles(): Promise<StoredFile[]> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(FILES_STORE, "readonly");
    const store = tx.objectStore(FILES_STORE);
    const request = store.getAll();
    request.onsuccess = () => resolve(request.result ?? []);
    request.onerror = () => reject(request.error);
  });
}

// Calculate folder size from IndexedDB (all files with prefix)
export async function calculateFolderSize(folderPrefix: string): Promise<number> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(FILES_STORE, "readonly");
    const store = tx.objectStore(FILES_STORE);
    const request = store.openCursor();
    
    let totalSize = 0;
    
    request.onsuccess = (event) => {
      const cursor = (event.target as IDBRequest<IDBCursorWithValue>).result;
      if (cursor) {
        const file = cursor.value as StoredFile;
        if (file.key.startsWith(folderPrefix)) {
          totalSize += file.size;
        }
        cursor.continue();
      } else {
        resolve(totalSize);
      }
    };
    
    request.onerror = () => reject(request.error);
  });
}

// Clear all cached data
export async function clearCache(): Promise<void> {
  const db = await openDB();
  const tx = db.transaction([FILES_STORE, META_STORE], "readwrite");
  tx.objectStore(FILES_STORE).clear();
  tx.objectStore(META_STORE).clear();
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}
