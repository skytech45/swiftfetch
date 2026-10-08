import { useState } from "react";
import { invoke } from "../jobs";
import { useI18n } from "../i18n";

export function GrabberDialog({ onClose }: { onClose: () => void }) {
  const { t, tp } = useI18n();
  const [name, setName] = useState("My site grab");
  const [seed, setSeed] = useState("https://example.com/");
  const [depth, setDepth] = useState("2");
  const [maxPages, setMaxPages] = useState("50");
  const [includeExts, setIncludeExts] = useState("zip,pdf,mp4");
  const [stayOnDomain, setStayOnDomain] = useState(true);
  const [respectRobots, setRespectRobots] = useState(true);
  const [running, setRunning] = useState(false);
  const [found, setFound] = useState(0);

  const start = async (): Promise<void> => {
    setRunning(true);
    try {
      const depthNum = Math.max(0, Math.min(5, Number(depth) || 2));
      const pagesNum = Math.max(1, Math.min(500, Number(maxPages) || 50));
      const config = JSON.stringify({
        max_depth: depthNum,
        max_pages: pagesNum,
        max_files: 100,
        include_exts: includeExts.split(",").map((s) => s.trim().toLowerCase()).filter((s) => s.length > 0),
        exclude_exts: [],
        stay_on_domain: stayOnDomain,
        respect_robots: respectRobots,
      });
      const id = await invoke<string>("create_grabber_project", {
        name,
        seedUrl: seed,
        configJson: config,
        queueId: null,
      });
      const count = await invoke<number>("run_grabber_project", { projectId: id });
      setFound(count);
    } catch {
      setFound(0);
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label={t("grabber.title")}>
      <div className="dialog">
        <h2>{t("grabber.title")}</h2>
        <label>
          {t("grabber.name")}
          <input value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label>
          {t("grabber.seedUrl")}
          <input value={seed} onChange={(e) => setSeed(e.target.value)} placeholder="https://" />
        </label>
        <label>
          {t("grabber.depth")}
          <input type="number" min={0} max={5} value={depth} onChange={(e) => setDepth(e.target.value)} />
        </label>
        <label>
          {t("grabber.maxPages")}
          <input type="number" min={1} max={500} value={maxPages} onChange={(e) => setMaxPages(e.target.value)} />
        </label>
        <label>
          {t("grabber.includeExts")}
          <input value={includeExts} onChange={(e) => setIncludeExts(e.target.value)} />
        </label>
        <label className="check">
          <input type="checkbox" checked={stayOnDomain} onChange={(e) => setStayOnDomain(e.target.checked)} />
          {t("grabber.stayOnDomain")}
        </label>
        <label className="check">
          <input type="checkbox" checked={respectRobots} onChange={(e) => setRespectRobots(e.target.checked)} />
          {t("grabber.respectRobots")}
        </label>
        {!respectRobots && <p className="warn">{t("grabber.disableRobotsWarn")}</p>}
        {found > 0 && <p>{tp("grabber.filesFound", found)}</p>}
        <div className="actions">
          <button type="button" onClick={onClose}>
            {t("grabber.close")}
          </button>
          <button type="button" className="primary" disabled={running} onClick={() => void start()}>
            {t("grabber.start")}
          </button>
        </div>
      </div>
    </div>
  );
}
