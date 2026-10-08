import { useCallback, useEffect, useState } from "react";
import { formatBytes, formatSpeed, invoke } from "../jobs";
import { useI18n } from "../i18n";

export interface TorrentView {
  id: string;
  name: string;
  state: string;
  progressBytes: number;
  totalBytes: number;
  downBps: number;
  upBps: number;
  peers: number;
  seedRatio: number;
}

export function TorrentsPanel() {
  const { t } = useI18n();
  const [torrents, setTorrents] = useState<TorrentView[]>([]);

  const load = useCallback(() => {
    invoke<TorrentView[]>("torrent_list")
      .then(setTorrents)
      .catch(() => {});
  }, []);

  useEffect(() => {
    load();
    const timer = window.setInterval(load, 2000);
    return () => window.clearInterval(timer);
  }, [load]);

  if (torrents.length === 0) {
    return <p className="muted small">{t("torrent.empty")}</p>;
  }

  return (
    <ul className="torrents">
      {torrents.map((torrent) => {
        const pct =
          torrent.totalBytes > 0
            ? Math.min(100, (torrent.progressBytes / torrent.totalBytes) * 100)
            : 0;
        return (
          <li key={torrent.id} className="torrent">
            <div className="torrent-head">
              <strong>{torrent.name}</strong>
              <span className="muted small">
                {torrent.state} · {pct.toFixed(1)}% · ↓{formatSpeed(torrent.downBps)} ↑
                {formatSpeed(torrent.upBps)} · {t("torrent.peers", { count: torrent.peers })} ·{" "}
                {formatBytes(torrent.progressBytes)}/{formatBytes(torrent.totalBytes)}
              </span>
            </div>
            <div className="bar small">
              <div className="fill" style={{ width: `${pct}%` }} />
            </div>
            <div className="row">
              <button type="button" onClick={() => void invoke("torrent_pause", { id: torrent.id }).then(load)}>
                {t("toolbar.pause")}
              </button>
              <button type="button" onClick={() => void invoke("torrent_resume", { id: torrent.id }).then(load)}>
                {t("toolbar.resume")}
              </button>
              <button
                type="button"
                onClick={() => void invoke("torrent_remove", { id: torrent.id, deleteFiles: false }).then(load)}
              >
                {t("toolbar.delete")}
              </button>
            </div>
          </li>
        );
      })}
    </ul>
  );
}
