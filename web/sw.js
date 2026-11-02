/* A minimal service worker so Android/Chrome can offer "Add to Home Screen".
   TagNest is a LAN app; data is intentionally not cached, so tag edits and
   scans always reflect the server state. */
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));
self.addEventListener('fetch', () => {
  // Let the network handle every request. This worker only provides the
  // installable PWA hook; it must never serve stale API or audio responses.
});
