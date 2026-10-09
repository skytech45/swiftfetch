// M4 extension tests: popup logic (Build Prompt §12.2, §17).
/* eslint-disable @typescript-eslint/no-require-imports -- UMD under test */
import { describe, expect, it } from "vitest";

const popup = require("./popup");

describe("buildLinksModel", () => {
  it("starts everything checked", () => {
    const model = popup.buildLinksModel([
      { url: "https://h/a.zip", text: "a" },
      { url: "https://h/b.mp4", text: "b" },
      { url: "https://h/page.html", text: "skip" },
    ]);
    expect(model).toHaveLength(2);
    expect(model.every((e) => e.checked)).toBe(true);
  });
});

describe("toggleAll + selectedUrls", () => {
  it("unchecks everything and selects nothing", () => {
    const model = popup.buildLinksModel([{ url: "https://h/a.zip", text: "a" }]);
    popup.toggleAll(model, false);
    expect(popup.selectedUrls(model)).toEqual([]);
  });

  it("returns checked URLs in order, deduplicated", () => {
    const model = [
      { url: "https://h/a.zip", checked: true },
      { url: "https://h/b.zip", checked: false },
      { url: "https://h/a.zip", checked: true },
      { url: "https://h/c.zip", checked: true },
    ];
    expect(popup.selectedUrls(model)).toEqual(["https://h/a.zip", "https://h/c.zip"]);
  });
});

describe("panelState", () => {
  it("error wins over everything", () => {
    expect(
      popup.panelState({ videos: [{ url: "https://h/v.m3u8" }], links: [], error: "boom" }).mode,
    ).toBe("error");
  });

  it("videos win over links", () => {
    const state = popup.panelState({
      videos: [{ url: "https://h/v.m3u8", kind: "hls" }],
      links: [{ url: "https://h/a.zip", text: "a" }],
    });
    expect(state.mode).toBe("videos");
    expect(state.videos).toHaveLength(1);
  });

  it("links show when no videos, empty otherwise", () => {
    expect(
      popup.panelState({ videos: [], links: [{ url: "https://h/a.zip", text: "a" }] }).mode,
    ).toBe("links");
    expect(popup.panelState({ videos: [], links: [] }).mode).toBe("empty");
    expect(popup.panelState({}).mode).toBe("empty");
  });
});
