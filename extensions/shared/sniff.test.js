// M4 extension tests: sniffing regexes + interception policy (Build Prompt
// §12.2, §17). Run with vitest from extensions/.
/* eslint-disable @typescript-eslint/no-require-imports -- UMD under test */
import { describe, expect, it } from "vitest";

const sniff = require("./sniff");

describe("isMediaUrl", () => {
  it("catches playlists, segments and media files", () => {
    for (const url of [
      "https://cdn.example/v/playlist.m3u8",
      "https://cdn.example/v/playlist.m3u8?token=abc",
      "https://cdn.example/v/stream.mpd#t=1",
      "https://cdn.example/v/seg-12.m4s",
      "https://cdn.example/v/clip.ts",
      "https://cdn.example/v/movie.mp4",
      "https://cdn.example/v/movie.webm",
      "https://host/manifest?x=1",
      "https://host/play?mime=video%2Fmp4",
    ]) {
      expect(isMedia(url)).toBe(true);
    }
    function isMedia(url) {
      return sniff.isMediaUrl(url);
    }
  });

  it("ignores pages, images and the YouTube one-click path", () => {
    for (const url of [
      "",
      "https://example.com/",
      "https://example.com/page.html",
      "https://example.com/photo.jpg",
      "https://example.com/app.js",
      "https://example.com/style.css",
      "https://rr1---sn.example.googlevideo.com/videoplayback?expire=1",
      "https://www.youtube.com/api/stats/watchtime",
    ]) {
      expect(sniff.isMediaUrl(url)).toBe(false);
    }
  });
});

describe("isMediaMime", () => {
  it("matches video/audio and playlist MIME types", () => {
    for (const mime of [
      "video/mp4",
      "video/webm",
      "audio/mpeg",
      "application/vnd.apple.mpegurl",
      "application/x-mpegurl",
      "application/dash+xml",
      "video/mp2t",
    ]) {
      expect(sniff.isMediaMime(mime)).toBe(true);
    }
  });

  it("rejects documents and binaries", () => {
    for (const mime of ["", "text/html", "application/pdf", "application/octet-stream", "image/png"]) {
      expect(sniff.isMediaMime(mime)).toBe(false);
    }
  });
});

describe("classifyMedia", () => {
  it("labels hls vs dash vs generic media", () => {
    expect(sniff.classifyMedia({ url: "https://h/master.m3u8" })).toBe("hls");
    expect(sniff.classifyMedia({ url: "https://h/manifest.mpd" })).toBe("dash");
    expect(sniff.classifyMedia({ url: "https://h/clip.mp4" })).toBe("media");
    expect(sniff.classifyMedia({ url: "https://h/x", mime: "video/mp4" })).toBe("media");
  });

  it("returns null for junk and YouTube traffic", () => {
    expect(sniff.classifyMedia(null)).toBe(null);
    expect(sniff.classifyMedia({})).toBe(null);
    expect(sniff.classifyMedia({ url: "https://h/page.html" })).toBe(null);
    expect(sniff.classifyMedia({ url: "https://x.googlevideo.com/v?x=1" })).toBe(null);
  });
});

describe("shouldIntercept", () => {
  const big = 50 * 1024 * 1024;
  it("sends attachments, binaries, listed types and big files", () => {
    expect(
      sniff.shouldIntercept({ url: "https://h/f.bin", contentDisposition: 'attachment; filename="f.bin"' }, {}),
    ).toBe("prompt");
    expect(sniff.shouldIntercept({ url: "https://h/f", mime: "application/octet-stream" }, {})).toBe("prompt");
    expect(sniff.shouldIntercept({ url: "https://h/tools.zip" }, {})).toBe("prompt");
    expect(sniff.shouldIntercept({ url: "https://h/blob", fileSize: big }, {})).toBe("prompt");
  });

  it("honors auto/off and ignores small pages", () => {
    expect(
      sniff.shouldIntercept({ url: "https://h/tools.zip" }, { autoSend: "auto" }),
    ).toBe("send");
    expect(sniff.shouldIntercept({ url: "https://h/tools.zip" }, { autoSend: "off" })).toBe("ignore");
    expect(sniff.shouldIntercept({ url: "https://h/page.html" }, {})).toBe("ignore");
    expect(sniff.shouldIntercept({ url: "ftp://h/f.zip" }, {})).toBe("ignore");
    expect(sniff.shouldIntercept(null, {})).toBe("ignore");
  });
});

describe("filterDownloadLinks", () => {
  it("keeps listed extensions, drops pages, dedupes", () => {
    const links = [
      { url: "https://h/a.zip", text: "a" },
      { url: "https://h/b.pdf", text: "b" },
      { url: "https://h/page.html", text: "page" },
      { url: "https://h/a.zip", text: "a again" },
      { url: "javascript:void(0)", text: "js" },
      { url: "https://h/UPPER.MP4", text: "upper" },
    ];
    const out = sniff.filterDownloadLinks(links);
    expect(out.map((e) => e.url)).toEqual(["https://h/a.zip", "https://h/b.pdf", "https://h/UPPER.MP4"]);
    expect(out[2].ext).toBe("mp4");
  });
});

describe("buildVideoList", () => {
  it("dedupes by URL, newest first, capped at 50", () => {
    const entries = Array.from({ length: 60 }, (_, i) => ({ url: `https://h/v${i}.m3u8`, kind: "hls" }));
    entries.splice(30, 0, { url: "https://h/v5.m3u8", kind: "hls" });
    const list = sniff.buildVideoList(entries);
    expect(list).toHaveLength(50);
    expect(list[0].url).toBe("https://h/v59.m3u8");
    expect(list.filter((e) => e.url === "https://h/v5.m3u8")).toHaveLength(1);
  });
});
