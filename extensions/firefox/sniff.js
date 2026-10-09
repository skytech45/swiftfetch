/* SwiftFetch shared extension core — media sniffing, interception policy,
 * link filtering (Build Prompt §12.2).
 *
 * UMD: loads as a classic script in MV3/Firefox backgrounds
 * (`globalThis.SwiftFetchSniff`), as a Node module under vitest, and as an
 * ES module import. No browser APIs in here — pure logic only, so every
 * function below is unit-testable.
 */
(function (root, factory) {
  if (typeof module !== "undefined" && module.exports) {
    module.exports = factory();
  } else if (typeof define === "function" && define.amd) {
    define([], factory);
  } else {
    root.SwiftFetchSniff = factory();
  }
})(typeof globalThis !== "undefined" ? globalThis : this, () => {
  "use strict";

  // Playlist / stream URL shapes the sniffer watches for.
  const MEDIA_URL_RES = [
    /\.m3u8(\?|#|$)/i,
    /\.mpd(\?|#|$)/i,
    /\.m4s(\?|#|$)/i,
    /\.ts(\?|#|$)/i,
    /\.mp4(\?|#|$)/i,
    /\.webm(\?|#|$)/i,
    /\.mkv(\?|#|$)/i,
    /\.flv(\?|#|$)/i,
    /\/manifest(\?|#|$)/i,
    /mime=video/i,
  ];

  // MIME types that mark a response as media.
  const MEDIA_MIME_RES = [
    /^video\//i,
    /^audio\//i,
    /mpegurl/i,
    /x-mpegurl/i,
    /dash\+xml/i,
    /mp2t/i,
  ];

  // googlvideo / player-range traffic is handled by the YouTube one-click
  // path (§12.4), never by the generic sniffer.
  const YOUTUBE_STREAM_RES = [/googlevideo\.com/i, /youtube\.com\/api\/stats/i];

  // Binary-ish MIME types eligible for download interception.
  const BINARY_MIME_RES = [/^application\/octet-stream/i, /^application\/x-/i, /^binary\//i];

  // Default file-type list for "Download all links" filtering.
  const DEFAULT_LINK_EXTS = ["zip", "rar", "7z", "pdf", "mp4", "mkv", "webm", "mp3", "exe", "msi", "iso", "tar", "gz"];

  /** True when `url` looks like a media/stream URL. */
  function isMediaUrl(url) {
    if (typeof url !== "string" || url.length === 0) return false;
    if (YOUTUBE_STREAM_RES.some((re) => re.test(url))) return false;
    return MEDIA_URL_RES.some((re) => re.test(url));
  }

  /** True when `mime` marks a response as media. */
  function isMediaMime(mime) {
    if (typeof mime !== "string" || mime.length === 0) return false;
    return MEDIA_MIME_RES.some((re) => re.test(mime));
  }

  /**
   * Classifies a watched request into `hls` | `dash` | `media` | null.
   * `details` is `{ url, mime? }` from webRequest / the popup probe.
   */
  function classifyMedia(details) {
    if (!details || typeof details.url !== "string") return null;
    const url = details.url;
    if (YOUTUBE_STREAM_RES.some((re) => re.test(url))) return null;
    if (/\.m3u8(\?|#|$)/i.test(url)) return "hls";
    if (/\.mpd(\?|#|$)/i.test(url)) return "dash";
    if (isMediaUrl(url)) return "media";
    if (details.mime && isMediaMime(details.mime)) return "media";
    return null;
  }

  /**
   * Interception decision for a finished download item
   * (`{ url, mime, contentDisposition, fileSize }`).
   * `config` is `{ autoSend: 'ask'|'auto'|'off', minBytes, types: [...] }`.
   * Returns 'send' | 'prompt' | 'ignore'.
   */
  function shouldIntercept(item, config) {
    const cfg = {
      autoSend: "ask",
      minBytes: 1024 * 1024,
      types: DEFAULT_LINK_EXTS,
      ...(config || {}),
    };
    if (cfg.autoSend === "off") return "ignore";
    if (!item || typeof item.url !== "string") return "ignore";
    const url = item.url;
    if (!/^https?:\/\//i.test(url)) return "ignore";
    const attachment = /attachment/i.test(item.contentDisposition || "");
    const binary = BINARY_MIME_RES.some((re) => re.test(item.mime || ""));
    const ext = extensionOf(url);
    const listed = cfg.types.some((t) => t.toLowerCase() === ext);
    const big = typeof item.fileSize === "number" && item.fileSize >= cfg.minBytes;
    if (!(attachment || binary || listed || big)) return "ignore";
    return cfg.autoSend === "auto" ? "send" : "prompt";
  }

  /** Lowercase extension of a URL path (no dot, no query). */
  function extensionOf(url) {
    const path = String(url).split("?")[0].split("#")[0];
    const file = path.slice(path.lastIndexOf("/") + 1);
    const dot = file.lastIndexOf(".");
    if (dot <= 0) return "";
    return file.slice(dot + 1).toLowerCase();
  }

  /**
   * Filters page links for "Download all links".
   * `links` is `[{ url, text }]`; returns entries with `{ url, text, ext }`
   * whose extension is in `types` (default list).
   */
  function filterDownloadLinks(links, types) {
    const allow = new Set((types || DEFAULT_LINK_EXTS).map((t) => String(t).toLowerCase()));
    const out = [];
    for (const link of links || []) {
      if (!link || typeof link.url !== "string") continue;
      if (!/^https?:\/\//i.test(link.url)) continue;
      const ext = extensionOf(link.url);
      if (!allow.has(ext)) continue;
      if (out.some((e) => e.url === link.url)) continue;
      out.push({ url: link.url, text: String(link.text || link.url), ext });
    }
    return out;
  }

  /**
   * Builds the popup "videos found on this page" model from sniffed entries
   * (`[{ url, kind, tabId }]`). Deduplicates by URL, newest first, capped
   * at 50 entries.
   */
  function buildVideoList(entries) {
    const seen = new Set();
    const out = [];
    const all = entries || [];
    for (let i = all.length - 1; i >= 0; i--) {
      const entry = all[i];
      if (!entry || typeof entry.url !== "string") continue;
      if (seen.has(entry.url)) continue;
      seen.add(entry.url);
      out.push({ url: entry.url, kind: entry.kind || "media" });
      if (out.length >= 50) break;
    }
    return out;
  }

  return {
    MEDIA_URL_RES,
    MEDIA_MIME_RES,
    DEFAULT_LINK_EXTS,
    isMediaUrl,
    isMediaMime,
    classifyMedia,
    shouldIntercept,
    extensionOf,
    filterDownloadLinks,
    buildVideoList,
  };
});
