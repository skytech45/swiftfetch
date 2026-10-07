import { useMemo, useState } from "react";
import { invoke } from "../jobs";
import { useI18n } from "../i18n";
import type { CategoryView, QueueView } from "../jobs";

type StartMode = "now" | "queue" | "later";

export function AddUrlDialog({
  categories,
  queues,
  initialUrl = "",
  onClose,
  onAdded,
}: {
  categories: CategoryView[];
  queues: QueueView[];
  /** Prefilled URL (drag-drop / clipboard capture). */
  initialUrl?: string;
  onClose: () => void;
  onAdded: () => void;
}) {
  const { t } = useI18n();
  const [url, setUrl] = useState(initialUrl);
  const [categoryId, setCategoryId] = useState("");
  const [queueId, setQueueId] = useState("");
  const [maxConns, setMaxConns] = useState(8);
  const [mode, setMode] = useState<StartMode>("now");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const valid = useMemo(() => {
    try {
      const parsed = new URL(url);
      return parsed.protocol === "http:" || parsed.protocol === "https:";
    } catch {
      return false;
    }
  }, [url]);

  const add = async (): Promise<void> => {
    if (!valid) {
      setError(t("addUrl.invalidUrl"));
      return;
    }
    setBusy(true);
    try {
      await invoke("add_url", {
        url,
        categoryId: categoryId || null,
        queueId: mode === "queue" ? queueId || "main" : queueId || null,
        maxConns,
        startNow: mode === "now",
      });
      onAdded();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label={t("addUrl.title")}>
      <div className="dialog">
        <h2>{t("addUrl.title")}</h2>
        <label>
          {t("addUrl.url")}
          <input
            type="url"
            value={url}
            placeholder={t("addUrl.urlPlaceholder")}
            onChange={(e) => setUrl(e.target.value)}
            autoFocus
          />
        </label>
        <div className="row2">
          <label>
            {t("addUrl.category")}
            <select
              value={categoryId}
              onChange={(e) => setCategoryId(e.target.value)}
            >
              <option value="">({t("addUrl.none")} — auto)</option>
              {categories.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            {t("addUrl.queue")}
            <select value={queueId} onChange={(e) => setQueueId(e.target.value)}>
              <option value="">{t("addUrl.none")}</option>
              {queues.map((q) => (
                <option key={q.id} value={q.id}>
                  {q.name}
                </option>
              ))}
            </select>
          </label>
        </div>
        <label>
          {t("addUrl.connections")}: {maxConns}
          <input
            type="range"
            min={1}
            max={32}
            value={maxConns}
            onChange={(e) => setMaxConns(Number(e.target.value))}
          />
        </label>
        <fieldset className="startmode">
          <label>
            <input
              type="radio"
              name="startmode"
              checked={mode === "now"}
              onChange={() => setMode("now")}
            />
            {t("addUrl.start.now")}
          </label>
          <label>
            <input
              type="radio"
              name="startmode"
              checked={mode === "queue"}
              onChange={() => setMode("queue")}
            />
            {t("addUrl.start.queue")}
          </label>
          <label>
            <input
              type="radio"
              name="startmode"
              checked={mode === "later"}
              onChange={() => setMode("later")}
            />
            {t("addUrl.start.later")}
          </label>
        </fieldset>
        {error !== null && <p className="error">{error}</p>}
        <div className="actions">
          <button type="button" onClick={onClose}>
            {t("addUrl.cancel")}
          </button>
          <button type="button" className="primary" disabled={!valid || busy} onClick={() => void add()}>
            {t("addUrl.add")}
          </button>
        </div>
      </div>
    </div>
  );
}
