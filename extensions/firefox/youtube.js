// SwiftFetch YouTube content script (§12.4): floating button → quality
// list → one click. All parsing lives in the app (Rust); this script only
// forwards the watch URL and renders the returned quality list.
(() => {
  if (window.__swiftfetchInjected) return;
  window.__swiftfetchInjected = true;

  const API = globalThis.browser ?? globalThis.chrome;

  let panel = null;

  function button() {
    const b = document.createElement("button");
    b.id = "swiftfetch-float";
    b.title = "Download with SwiftFetch";
    b.textContent = "⬇ SwiftFetch";
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      showQualities();
    });
    return b;
  }

  function mount() {
    if (document.getElementById("swiftfetch-float")) return;
    document.documentElement.appendChild(button());
  }

  async function showQualities() {
    if (panel) {
      panel.remove();
      panel = null;
      return;
    }
    panel = document.createElement("div");
    panel.id = "swiftfetch-panel";
    panel.textContent = "…";
    document.documentElement.appendChild(panel);
    const reply = await API.runtime.sendMessage({
      type: "youtube-qualities",
      url: location.href,
      cookies: document.cookie,
      referer: location.origin,
    });
    panel.textContent = "";
    if (!reply || !reply.ok) {
      panel.textContent = (reply && reply.error) || "SwiftFetch: unavailable";
      return;
    }
    for (const q of reply.qualities || []) {
      const item = document.createElement("button");
      item.className = "swiftfetch-quality";
      item.textContent = q.label;
      item.addEventListener("click", async () => {
        const r = await API.runtime.sendMessage({
          type: "youtube-one-click",
          url: location.href,
          height: q.height,
          cookies: document.cookie,
          referer: location.origin,
        });
        panel.textContent = r && r.ok ? "Added to SwiftFetch ✓" : `Failed: ${r && r.error}`;
        setTimeout(() => {
          if (panel) {
            panel.remove();
            panel = null;
          }
        }, 2500);
      });
      panel.appendChild(item);
    }
    setTimeout(() => {
      if (panel && panel.childElementCount === 0 && panel.textContent === "…") {
        panel.remove();
        panel = null;
      }
    }, 1000);
  }

  mount();
  // YouTube navigations are SPA-soft; re-mount on history pushes.
  const observer = new MutationObserver(mount);
  observer.observe(document.body ?? document.documentElement, { childList: true, subtree: true });
})();
