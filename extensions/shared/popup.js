/* SwiftFetch popup model (Build Prompt §12.2) — pure logic for the
 * extension popup: "videos found on this page" plus the "Download all
 * links" checkbox list. UMD like `sniff.js`; needs `SwiftFetchSniff`
 * (browser global) or `./sniff` (Node) for link filtering.
 */
(function (root, factory) {
  const sniff =
    typeof module !== "undefined" && module.exports
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      ? require("./sniff")
      : root.SwiftFetchSniff;
  const api = factory(sniff);
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  } else {
    root.SwiftFetchPopup = api;
  }
})(typeof globalThis !== "undefined" ? globalThis : this, (sniff) => {
  "use strict";

  /**
   * Builds the checkbox list model for "Download all links".
   * Every entry starts checked; callers flip `checked` per checkbox.
   */
  function buildLinksModel(links, types) {
    return sniff.filterDownloadLinks(links, types).map((entry) => ({ ...entry, checked: true }));
  }

  /** Sets every entry's checkbox. Returns the model for chaining. */
  function toggleAll(model, checked) {
    for (const entry of model) entry.checked = checked === true;
    return model;
  }

  /** URLs of the checked entries, in list order, deduplicated. */
  function selectedUrls(model) {
    const out = [];
    for (const entry of model || []) {
      if (entry && entry.checked === true && typeof entry.url === "string") {
        if (!out.includes(entry.url)) out.push(entry.url);
      }
    }
    return out;
  }

  /**
   * Decides what the popup shows: `{ mode, videos, links }` where mode is
   * 'error' | 'videos' | 'links' | 'empty'. Videos win over links; an
   * error message wins over everything.
   */
  function panelState({ videos, links, error }) {
    if (typeof error === "string" && error.length > 0) {
      return { mode: "error", videos: [], links: [] };
    }
    const videoList = sniff.buildVideoList(videos);
    if (videoList.length > 0) {
      return { mode: "videos", videos: videoList, links: [] };
    }
    const linkModel = buildLinksModel(links);
    if (linkModel.length > 0) {
      return { mode: "links", videos: [], links: linkModel };
    }
    return { mode: "empty", videos: [], links: [] };
  }

  return { buildLinksModel, toggleAll, selectedUrls, panelState };
});
