import { useState } from "react";
import { invoke, type QueueView } from "../jobs";
import { useI18n } from "../i18n";

type ScheduleKind = "manual" | "start_stop" | "daily" | "periodic" | "once";

/** Builds scheduler JSON from the editor fields (scheduler-crate format). */
function scheduleJson(kind: ScheduleKind, start: string, stop: string, every: number): string | null {
  switch (kind) {
    case "manual":
      return null;
    case "start_stop":
      return JSON.stringify({ kind, start: `${start}:00`, stop: `${stop}:00` });
    case "daily":
      return JSON.stringify({ kind, at: `${start}:00` });
    case "periodic":
      return JSON.stringify({ kind, every_secs: Math.max(60, every * 60), jitter_secs: 0 });
    case "once":
      // `<input type="datetime-local">` value → ISO instant.
      return JSON.stringify({ kind, at: new Date(start).toISOString() });
  }
}

function ScheduleEditor({ queue, onSaved }: { queue: QueueView; onSaved: () => void }) {
  const { t } = useI18n();
  const parsed = queue.scheduleJson
    ? (JSON.parse(queue.scheduleJson) as { kind: ScheduleKind; start?: string; stop?: string; at?: string; every_secs?: number })
    : null;
  const [kind, setKind] = useState<ScheduleKind>(parsed?.kind ?? "manual");
  const [start, setStart] = useState(
    parsed?.start?.slice(0, 5) ?? parsed?.at?.slice(11, 16) ?? "22:00",
  );
  const [stop, setStop] = useState(parsed?.stop?.slice(0, 5) ?? "06:00");
  const [every, setEvery] = useState(Math.max(1, Math.round((parsed?.every_secs ?? 3600) / 60)));
  const [postAction, setPostAction] = useState(queue.postAction || "none");
  const [busy, setBusy] = useState(false);

  const save = async (): Promise<void> => {
    setBusy(true);
    try {
      await invoke("set_queue_schedule", {
        queueId: queue.id,
        schedule: scheduleJson(kind, start, stop, every),
        postAction,
      });
      onSaved();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="schedule-editor">
      <label>
        {t("queue.schedule.kind")}
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value as ScheduleKind)}
        >
          <option value="manual">{t("queue.schedule.manual")}</option>
          <option value="start_stop">{t("queue.schedule.startStop")}</option>
          <option value="daily">{t("queue.schedule.daily")}</option>
          <option value="periodic">{t("queue.schedule.periodic")}</option>
          <option value="once">{t("queue.schedule.once")}</option>
        </select>
      </label>
      {(kind === "start_stop" || kind === "daily") && (
        <label>
          {kind === "daily" ? t("queue.schedule.at") : t("queue.schedule.from")}
          <input type="time" value={start} onChange={(e) => setStart(e.target.value)} />
        </label>
      )}
      {kind === "start_stop" && (
        <label>
          {t("queue.schedule.to")}
          <input type="time" value={stop} onChange={(e) => setStop(e.target.value)} />
        </label>
      )}
      {kind === "periodic" && (
        <label>
          {t("queue.schedule.every")}
          <input
            type="number"
            min={1}
            value={every}
            onChange={(e) => setEvery(Number(e.target.value) || 60)}
          />
        </label>
      )}
      {kind === "once" && (
        <label>
          {t("queue.schedule.at")}
          <input
            type="datetime-local"
            value={start}
            onChange={(e) => setStart(e.target.value)}
          />
        </label>
      )}
      <label>
        {t("queue.postAction")}
        <select value={postAction} onChange={(e) => setPostAction(e.target.value)}>
          <option value="none">{t("queue.postNone")}</option>
          <option value="sleep">{t("queue.postSleep")}</option>
          <option value="hibernate">{t("queue.postHibernate")}</option>
          <option value="shutdown">{t("queue.postShutdown")}</option>
        </select>
      </label>
      <button type="button" className="mini" disabled={busy} onClick={() => void save()}>
        {t("settings.save")}
      </button>
    </div>
  );
}

export function QueuePanel({
  queues,
  onChanged,
}: {
  queues: QueueView[];
  onChanged: () => void;
}) {
  const { t } = useI18n();
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [expanded, setExpanded] = useState<string | null>(null);

  const create = async (): Promise<void> => {
    if (name.trim().length === 0) return;
    await invoke("create_queue", { name: name.trim(), maxConcurrent: 2 });
    setName("");
    setCreating(false);
    onChanged();
  };

  return (
    <>
      <h3>
        {t("sidebar.queues")}{" "}
        <button
          type="button"
          className="mini"
          aria-label={t("sidebar.newQueue")}
          onClick={() => setCreating((v) => !v)}
        >
          +
        </button>
      </h3>
      {creating && (
        <div className="new-queue">
          <input
            value={name}
            placeholder={t("sidebar.newQueue")}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void create();
            }}
          />
          <button type="button" className="mini" onClick={() => void create()}>
            ✓
          </button>
        </div>
      )}
      {queues.map((q) => (
        <div key={q.id} className="queue-group">
          <div className="queue-head">
            <span className={q.isActive ? "queue-name active" : "queue-name"}>
              {q.name} ({q.maxConcurrent})
            </span>
            <span className="queue-actions">
              <button
                type="button"
                className="mini"
                aria-label={t("queue.schedule.title")}
                onClick={() =>
                  setExpanded((cur) => (cur === q.id ? null : q.id))
                }
              >
                ⏱
              </button>
              {q.isActive ? (
                <button
                  type="button"
                  className="mini"
                  aria-label={t("sidebar.stopQueue")}
                  onClick={() =>
                    void invoke("stop_queue", { id: q.id }).then(onChanged)
                  }
                >
                  ■
                </button>
              ) : (
                <button
                  type="button"
                  className="mini"
                  aria-label={t("sidebar.startQueue")}
                  onClick={() =>
                    void invoke("start_queue", { id: q.id }).then(onChanged)
                  }
                >
                  ▶
                </button>
              )}
              {q.id !== "main" && (
                <button
                  type="button"
                  className="mini"
                  aria-label={t("sidebar.deleteQueue")}
                  onClick={() =>
                    void invoke("delete_queue", { id: q.id }).then(onChanged)
                  }
                >
                  ×
                </button>
              )}
            </span>
          </div>
          <ol className="queue-items">
            {q.jobIds.map((id) => (
              <li key={id}>
                <button
                  type="button"
                  className="mini"
                  aria-label={t("sidebar.moveUp")}
                  onClick={() =>
                    void invoke("move_queue_item", {
                      queueId: q.id,
                      jobId: id,
                      offset: -1,
                    }).then(onChanged)
                  }
                >
                  ↑
                </button>
                <button
                  type="button"
                  className="mini"
                  aria-label={t("sidebar.moveDown")}
                  onClick={() =>
                    void invoke("move_queue_item", {
                      queueId: q.id,
                      jobId: id,
                      offset: 1,
                    }).then(onChanged)
                  }
                >
                  ↓
                </button>
                <button
                  type="button"
                  className="mini"
                  aria-label={t("toolbar.delete")}
                  onClick={() =>
                    void invoke("dequeue_job", { jobId: id }).then(onChanged)
                  }
                >
                  ×
                </button>
              </li>
            ))}
          </ol>
          {expanded === q.id && (
            <ScheduleEditor queue={q} onSaved={onChanged} />
          )}
        </div>
      ))}
    </>
  );
}
