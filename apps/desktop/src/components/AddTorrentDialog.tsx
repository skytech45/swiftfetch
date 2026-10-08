import { useState } from "react";
import { invoke } from "../jobs";
import { useI18n } from "../i18n";

export function AddTorrentDialog({ onClose, onAdded }: { onClose: () => void; onAdded: () => void }) {
  const { t } = useI18n();
  const [source, setSource] = useState("");
  const [ratio, setRatio] = useState("1.0");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const add = async (): Promise<void> => {
    const trimmed = source.trim();
    if (!trimmed.startsWith("magnet:") && !trimmed.endsWith(".torrent")) {
      setError(t("torrent.invalidSource"));
      return;
    }
    setBusy(true);
    try {
      await invoke("torrent_add", {
        source: trimmed,
        seedRatio: Number(ratio) > 0 ? Number(ratio) : 1.0,
      });
      onAdded();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label={t("torrent.title")}>
      <div className="dialog">
        <h2>{t("torrent.title")}</h2>
        <label>
          {t("torrent.source")}
          <input
            value={source}
            onChange={(e) => setSource(e.target.value)}
            placeholder={t("torrent.sourcePlaceholder")}
            autoFocus
          />
        </label>
        <label>
          {t("torrent.seedRatio")}
          <input
            type="number"
            min={0}
            step={0.1}
            value={ratio}
            onChange={(e) => setRatio(e.target.value)}
          />
        </label>
        {error !== null && <p className="error">{error}</p>}
        <div className="actions">
          <button type="button" onClick={onClose}>
            {t("addUrl.cancel")}
          </button>
          <button type="button" className="primary" disabled={busy} onClick={() => void add()}>
            {t("addUrl.add")}
          </button>
        </div>
      </div>
    </div>
  );
}
