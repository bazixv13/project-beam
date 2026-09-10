/* BEAM service worker — installability + Web Share Target receiver.
 * Kept intentionally dumb: no precaching, no offline shell. Transfers always
 * need a live connection, so the SW only intercepts share-target POSTs,
 * stashes the shared File objects in IndexedDB, and redirects into the app.
 * The page itself (App.jsx) picks the files up and stages them for sending. */

const SHARE_DB = 'beam-share';
const SHARE_STORE = 'files';
const SHARE_KEY = 'shared-files';

function openShareDb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(SHARE_DB, 1);
    req.onupgradeneeded = () => {
      req.result.createObjectStore(SHARE_STORE);
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function stashSharedFiles(files) {
  return openShareDb().then(
    (db) =>
      new Promise((resolve, reject) => {
        const tx = db.transaction(SHARE_STORE, 'readwrite');
        tx.objectStore(SHARE_STORE).put(files, SHARE_KEY);
        tx.oncomplete = () => {
          db.close();
          resolve();
        };
        tx.onerror = () => reject(tx.error);
      })
  );
}

self.addEventListener('install', () => {
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  const isSharePost =
    event.request.method === 'POST' && url.search.includes('share-target');

  if (!isSharePost) return;

  event.respondWith(
    (async () => {
      try {
        const formData = await event.request.formData();
        const files = (formData.getAll('files') || []).filter(
          (entry) =>
            entry &&
            typeof entry === 'object' &&
            typeof entry.name === 'string' &&
            entry.size > 0
        );
        if (files.length > 0) {
          await stashSharedFiles(files);
        }
      } catch (_) {
        // Fall through to the app even if stashing failed — the app will
        // simply show its normal home screen with nothing staged.
      }
      return Response.redirect('/?share-target', 303);
    })()
  );
});
