import { useState } from "react";
import { invoke, type QueueView } from "../jobs";
import { useI18n } from "../i18n";

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
        </div>
      ))}
    </>
  );
}
