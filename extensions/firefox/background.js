// SwiftFetch background (MV3 service worker + Firefox background page):
// routes capture requests through the native-messaging host, sniffs media
// traffic into per-tab "videos found" lists, and intercepts large/binary
// downloads per the shared policy (extensions/shared/sniff.js).
//
// Canonical source — packaged copies in chrome/ and firefox/ are synced by
// scripts/sync-extensions.mjs. In Chrome this file runs as the MV3 worker
// (sniff.js arrives via importScripts); in Firefox both files are listed in
// manifest background.scripts.
if (typeof importScripts === "function") {
  try {
    importScripts("sniff.js");
  } catch (e) {
    console.warn("SwiftFetch: sniff.js unavailable", e);
  }
}

const HOST_ID = "com.swiftfetch.host";
const Sniff =
  typeof globalThis !== "undefined" && globalThis.SwiftFetchSniff
    ? globalThis.SwiftFetchSniff
    : null;

let port = null;
let pending = new Map(); // reqId -> resolve
let listeners = new Set(); // tabs interested in event pushes
let reqSeq = 0;
let tabVideos = new Map(); // tabId -> [{url, kind}]
let interceptConfig = { autoSend: "ask", minBytes: 1024 * 1024 };

try {
  const store = (globalThis.browser ?? globalThis.chrome)?.storage?.local;
  if (store) {
    store.get(["autoSend", "minBytes"]).then((cfg) => {
      if (cfg.autoSend) interceptConfig.autoSend = cfg.autoSend;
      if (cfg.minBytes) interceptConfig.minBytes = cfg.minBytes;
    }).catch(() => {});
  }
} catch (e) {
  void e;
}

function ensurePort() {
  if (port) return port;
  const api = globalThis.browser ?? globalThis.chrome;
  port = api.runtime.connectNative(HOST_ID);
  port.onMessage.addListener((msg) => {
    if (msg && msg.type === "event") {
      for (const tabId of listeners) {
        api.tabs.sendMessage(tabId, msg).catch(() => {});
      }
      return;
    }
    const reqId = msg && msg.reqId;
    const entry = pending.get(reqId);
    if (entry) {
      pending.delete(reqId);
      entry(msg);
    }
  });
  port.onDisconnect.addListener(() => {
    port = null;
    for (const [, entry] of pending) entry({ ok: false, error: "host disconnected" });
    pending = new Map();
  });
  return port;
}

function callHost(payload) {
  const p = ensurePort();
  const reqId = String(++reqSeq);
  return new Promise((resolve) => {
    pending.set(reqId, resolve);
    p.postMessage({ ...payload, reqId });
    setTimeout(() => {
      if (pending.has(reqId)) {
        pending.delete(reqId);
        resolve({ ok: false, error: "host timeout" });
      }
    }, 15000);
  });
}

function noteVideo(tabId, url, kind) {
  if (tabId === undefined || tabId < 0 || !url) return;
  const list = tabVideos.get(tabId) || [];
  if (!list.some((e) => e.url === url)) {
    list.push({ url, kind });
    if (list.length > 50) list.shift();
    tabVideos.set(tabId, list);
  }
}

// Media sniffing: watch media-ish responses into per-tab video lists.
(function armSniffer() {
  const api = (globalThis.browser ?? globalThis.chrome) || {};
  const webRequest = api.webRequest;
  if (!webRequest || !webRequest.onHeadersReceived || !Sniff) return;
  webRequest.onHeadersReceived.addListener(
    (details) => {
      let mime = "";
      for (const h of details.responseHeaders || []) {
        if (/^content-type$/i.test(h.name || "")) mime = h.value || "";
      }
      const kind = Sniff.classifyMedia({ url: details.url, mime });
      if (kind) noteVideo(details.tabId, details.url, kind);
    },
    { urls: ["<all_urls>"] },
    ["responseHeaders"],
  );
})();

// Messages from content scripts / popup.
(globalThis.browser ?? globalThis.chrome).runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg && msg.type === "watch-events") {
    if (sender.tab) listeners.add(sender.tab.id);
    sendResponse({ ok: true });
    return false;
  }
  if (msg && msg.type === "list-videos") {
    const tabId = (sender.tab && sender.tab.id) ?? msg.tabId;
    sendResponse({ ok: true, videos: tabVideos.get(tabId) || [] });
    return false;
  }
  if (msg && (msg.type === "add-download" || msg.type === "youtube-qualities" || msg.type === "youtube-one-click")) {
    callHost(msg).then((reply) => {
      // Track newly staged captures so their events reach this tab.
      if (reply && reply.ok && reply.stagedId && sender.tab) listeners.add(sender.tab.id);
      sendResponse(reply);
    });
    return true; // async response
  }
  if (msg && (msg.type === "send-links" || msg.type === "download-all") && Array.isArray(msg.urls)) {
    const queue = msg.queue || null;
    Promise.all(msg.urls.map((url) => callHost({ type: "add-download", url, queue }))).then(
      (replies) => {
        sendResponse({ ok: replies.every((r) => r && r.ok), count: replies.length });
      },
    );
    return true;
  }
  return false;
});

// Context menu: download any link with SwiftFetch.
(globalThis.browser ?? globalThis.chrome).runtime.onInstalled.addListener(() => {
  (globalThis.browser ?? globalThis.chrome).contextMenus.create({
    id: "swiftfetch-download",
    title: "Download with SwiftFetch",
    contexts: ["link"],
  });
});

(globalThis.browser ?? globalThis.chrome).contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId !== "swiftfetch-download" || !info.linkUrl) return;
  callHost({ type: "add-download", url: info.linkUrl }).then((reply) => {
    if (tab && tab.id) {
      (globalThis.browser ?? globalThis.chrome).tabs
        .sendMessage(tab.id, { type: "notify", ok: !!reply.ok, message: reply.ok ? "added to SwiftFetch" : String(reply.error) })
        .catch(() => {});
    }
  });
});

// Download interception: large/attachment/binary items go to SwiftFetch.
// 'auto' cancels the browser item silently; 'ask' forwards it too but tells
// the tab what happened (MV3 offers no blocking prompt surface, so the
// tab notice doubles as the prompt receipt).
(function armInterception() {
  const api = (globalThis.browser ?? globalThis.chrome) || {};
  const downloads = api.downloads;
  if (!downloads || !downloads.onCreated || !Sniff) return;
  downloads.onCreated.addListener((item) => {
    const verdict = Sniff.shouldIntercept(
      {
        url: item.url,
        mime: item.mime,
        contentDisposition: item.contentDisposition,
        fileSize: item.fileSize,
      },
      interceptConfig,
    );
    if (verdict === "ignore" || verdict === "off") return;
    const forward = () =>
      callHost({ type: "add-download", url: item.url }).then((reply) => {
        if (verdict === "ask" && item.id !== undefined) {
          api.tabs
            .query({ active: true, currentWindow: true })
            .then((tabs) => {
              for (const t of tabs) {
                if (t.id !== undefined) {
                  api.tabs
                    .sendMessage(t.id, {
                      type: "notify",
                      ok: !!reply.ok,
                      message: reply.ok ? "Sent to SwiftFetch ✓" : String(reply.error),
                    })
                    .catch(() => {});
                }
              }
            })
            .catch(() => {});
        }
      });
    if (item.id !== undefined && downloads.cancel) {
      downloads.cancel(item.id).then(forward).catch(forward);
    } else {
      forward();
    }
  });
})();
