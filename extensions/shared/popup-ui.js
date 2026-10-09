/* SwiftFetch popup UI (thin DOM shell — the model lives in popup.js,
 * which is unit-tested). Canonical source; synced into chrome/ + firefox/.
 */
(function () {
  const api = globalThis.browser ?? globalThis.chrome;
  const model = globalThis.SwiftFetchPopup;
  const stateEl = document.getElementById("state");
  const videosEl = document.getElementById("videos");
  const linksEl = document.getElementById("links");
  const videoList = document.getElementById("video-list");
  const linkList = document.getElementById("link-list");
  const toggleAll = document.getElementById("toggle-all");
  const sendBtn = document.getElementById("send");

  let linkModel = [];

  function setState(text) {
    stateEl.textContent = text;
  }

  function showVideos(videos) {
    videosEl.hidden = false;
    linksEl.hidden = true;
    setState(`${videos.length} video(s) found on this page`);
    videoList.textContent = "";
    for (const v of videos) {
      const li = document.createElement("li");
      const btn = document.createElement("button");
      btn.textContent = `⬇ ${v.kind}: ${v.url.slice(0, 80)}`;
      btn.title = v.url;
      btn.addEventListener("click", () => {
        api.runtime.sendMessage({ type: "add-download", url: v.url }).then((r) => {
          setState(r && r.ok ? "Added to SwiftFetch ✓" : `Failed: ${r && r.error}`);
        });
      });
      li.appendChild(btn);
      videoList.appendChild(li);
    }
  }

  function showLinks(links) {
    videosEl.hidden = true;
    linksEl.hidden = false;
    linkModel = links;
    setState(`${links.length} downloadable link(s) on this page`);
    linkList.textContent = "";
    for (const entry of links) {
      const li = document.createElement("li");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = entry.checked;
      box.addEventListener("change", () => {
        entry.checked = box.checked;
      });
      const span = document.createElement("span");
      span.textContent = entry.text;
      span.title = entry.url;
      li.appendChild(box);
      li.appendChild(span);
      linkList.appendChild(li);
    }
  }

  toggleAll.addEventListener("change", () => {
    model.toggleAll(linkModel, toggleAll.checked);
    for (const [i, entry] of linkModel.entries()) {
      const box = linkList.children[i] && linkList.children[i].querySelector("input");
      if (box) box.checked = entry.checked;
    }
  });

  sendBtn.addEventListener("click", () => {
    const urls = model.selectedUrls(linkModel);
    api.runtime.sendMessage({ type: "send-links", urls }).then((r) => {
      setState(r && r.ok ? `Sent ${urls.length} link(s) ✓` : `Failed: ${r && r.error}`);
    });
  });

  // Boot: ask the background for this tab's videos, else scrape links.
  api.tabs
    .query({ active: true, currentWindow: true })
    .then(async (tabs) => {
      const tab = tabs[0];
      const videosReply = await api.runtime.sendMessage({ type: "list-videos", tabId: tab && tab.id });
      const videos = (videosReply && videosReply.videos) || [];
      let links = [];
      if (videos.length === 0 && tab && tab.id !== undefined) {
        const scraped = await api.scripting
          .executeScript({
            target: { tabId: tab.id },
            func: () =>
              Array.from(document.querySelectorAll("a[href]"), (a) => ({
                url: a.href,
                text: a.textContent.trim().slice(0, 120),
              })),
          })
          .then((frames) => (frames[0] ? frames[0].result : []))
          .catch(() => []);
        links = model.buildLinksModel(scraped);
      }
      const state = model.panelState({ videos, links });
      if (state.mode === "videos") showVideos(state.videos);
      else if (state.mode === "links") showLinks(state.links);
      else setState("No videos or downloadable links on this page.");
    })
    .catch((err) => setState(`SwiftFetch: unavailable (${err})`));
})();
