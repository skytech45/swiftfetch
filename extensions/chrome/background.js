// SwiftFetch MV3 service worker: routes capture requests from content
// scripts through the native-messaging host (framed JSON over the port).
const HOST_ID = "com.swiftfetch.host";

let port = null;
let pending = new Map(); // reqId -> {resolve, reject}
let listeners = new Set(); // tabs interested in event pushes
let reqSeq = 0;

function ensurePort() {
  if (port) return port;
  port = chrome.runtime.connectNative(HOST_ID);
  port.onMessage.addListener((msg) => {
    if (msg && msg.type === "event") {
      for (const tabId of listeners) {
        chrome.tabs.sendMessage(tabId, msg).catch(() => {});
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

// Messages from content scripts / popup.
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg && msg.type === "watch-events") {
    if (sender.tab) listeners.add(sender.tab.id);
    sendResponse({ ok: true });
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
  return false;
});

// Context menu: download any link with SwiftFetch.
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "swiftfetch-download",
    title: "Download with SwiftFetch",
    contexts: ["link"],
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId !== "swiftfetch-download" || !info.linkUrl) return;
  callHost({ type: "add-download", url: info.linkUrl }).then((reply) => {
    if (tab && tab.id) {
      chrome.tabs
        .sendMessage(tab.id, { type: "notify", ok: !!reply.ok, message: reply.ok ? "added to SwiftFetch" : String(reply.error) })
        .catch(() => {});
    }
  });
});
