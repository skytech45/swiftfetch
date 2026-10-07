import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "./jobs";
import {
  formatBytes,
  formatEta,
  formatSpeed,
  installEventBridge,
  useJobs,
  type CategoryView,
  type JobView,
  type QuotaStatusView,
  type QueueView,
} from "./jobs";
import { saveTheme, useI18n, useTheme, type Theme } from "./i18n";
import { AddUrlDialog } from "./components/AddUrlDialog";
import { ProgressDialog } from "./components/ProgressDialog";
import { QueuePanel } from "./components/QueuePanel";
import { SettingsDialog } from "./components/SettingsDialog";

/** One notification bubble (clipboard capture, scheduler, quota). */
interface Toast {
  id: number;
  text: string;
  /** URL to offer adding (clipboard capture). */
  url?: string;
  /** Show a Cancel button (post-action countdown). */
  cancellable?: boolean;
}

/** Extracts an http(s)/ftp URL from dropped text/URI-list data. */
function extractUrl(data: string): string | null {
  for (const line of data.split("\n")) {
    const candidate = line.trim().replace(/^URL=/, "");
    if (/^(https?|ftp):\/\/\S+$/.test(candidate)) return candidate;
  }
  return null;
}

export default function App() {
  const { t, lang, setLang } = useI18n();
  const theme = useTheme();
  const jobs = useJobs();
  const [categories, setCategories] = useState<CategoryView[]>([]);
  const [queues, setQueues] = useState<QueueView[]>([]);
  const [filter, setFilter] = useState<string>("all");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [addOpen, setAddOpen] = useState(false);
  const [addUrl, setAddUrl] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [sortDesc, setSortDesc] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [quota, setQuota] = useState<QuotaStatusView | null>(null);
  const toastSeq = useRef(0);

  const pushToast = useCallback((toast: Omit<Toast, "id">) => {
    toastSeq.current += 1;
    const id = toastSeq.current;
    setToasts((cur) => [...cur, { ...toast, id }]);
    window.setTimeout(() => {
      setToasts((cur) => cur.filter((toast2) => toast2.id !== id));
    }, 8000);
  }, []);

  const refreshMeta = useCallback(() => {
    invoke<CategoryView[]>("list_categories").then(setCategories).catch(() => {});
    invoke<QueueView[]>("list_queues").then(setQueues).catch(() => {});
  }, []);

  useEffect(() => {
    const un = installEventBridge();
    refreshMeta();
    const onTheme = (e: MediaQueryListEvent): void => {
      if (document.documentElement.dataset.theme === undefined) return;
      void e;
    };
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", onTheme);

    // M3 automation events (clipboard capture, scheduler, quota, post-action).
    let disposed = false;
    const unlisteners: Array<Promise<() => void>> = [];
    const listen = (event: string, handler: (payload: unknown) => void): void => {
      unlisteners.push(
        import("@tauri-apps/api/event").then(({ listen: on }) =>
          on(event, (e) => handler(e.payload)),
        ),
      );
    };
    listen("clipboard://url", (payload) => {
      const url = (payload as { url?: string }).url ?? "";
      if (url.length > 0) {
        pushToast({ text: t("toast.clipboard"), url });
      }
    });
    listen("scheduler://fired", (payload) => {
      const p = payload as { queueId?: string; action?: string };
      pushToast({
        text: t(p.action === "open" ? "toast.queueOpened" : "toast.queueClosed"),
      });
    });
    listen("quota://changed", (payload) => {
      const p = payload as { exhausted?: boolean };
      pushToast({
        text: p.exhausted === true ? t("toast.quotaExhausted") : t("toast.quotaResumed"),
      });
    });
    listen("scheduler://post-action", (payload) => {
      const p = payload as { action?: string };
      pushToast({
        text: t("toast.postAction", { action: p.action ?? "shutdown" }),
        cancellable: true,
      });
    });
    void Promise.all(unlisteners).then((fns) => {
      if (disposed) for (const f of fns) f();
    });

    // Quota status for the status bar (10 s cadence is plenty).
    const quotaTimer = window.setInterval(() => {
      invoke<QuotaStatusView>("get_quota_status").then(setQuota).catch(() => {});
    }, 10_000);
    invoke<QuotaStatusView>("get_quota_status").then(setQuota).catch(() => {});

    return () => {
      disposed = true;
      void un.then((f) => f());
      window.clearInterval(quotaTimer);
      window.matchMedia("(prefers-color-scheme: dark)").removeEventListener("change", onTheme);
    };
  }, [refreshMeta, pushToast, t]);

  const visible = useMemo(() => {
    const filtered =
      filter === "all" ? jobs : jobs.filter((j) => j.categoryId === filter);
    return [...filtered].sort((a, b) =>
      sortDesc
        ? b.createdAt.localeCompare(a.createdAt)
        : a.createdAt.localeCompare(b.createdAt),
    );
  }, [jobs, filter, sortDesc]);

  const activeCount = jobs.filter((j) =>
    ["downloading", "probing", "verifying"].includes(j.state),
  ).length;
  const totalSpeed = jobs.reduce((acc, j) => acc + Math.max(0, j.speedBps), 0);

  const selectedIds = useMemo(
    () => visible.filter((j) => selected.has(j.id)).map((j) => j.id),
    [visible, selected],
  );

  const act = useCallback(
    async (action: "resume" | "pause", id: string) => {
      await invoke(
        action === "resume" ? "resume_job" : "pause_job",
        { id },
      );
    },
    [],
  );

  const deleteSelected = useCallback(async () => {
    const withFile = window.confirm(t("delete.confirm"));
    if (!withFile) return;
    for (const id of selectedIds) {
      await invoke("delete_job", { id, deleteFile: true });
    }
    setSelected(new Set());
  }, [selectedIds, t]);

  const pauseAll = useCallback(async () => {
    for (const j of jobs) {
      if (j.state === "downloading") await invoke("pause_job", { id: j.id });
    }
  }, [jobs]);

  const resumeAll = useCallback(async () => {
    for (const j of jobs) {
      if (j.state === "paused" || j.state === "interrupted") {
        await invoke("resume_job", { id: j.id });
      }
    }
  }, [jobs]);

  const changeTheme = useCallback(
    (next: Theme) => {
      void saveTheme(next);
      // Reload-less theme switch: re-apply via the hook by forcing state.
      window.location.hash = `#theme-${next}`;
      document.documentElement.dataset.theme =
        next === "dark"
          ? "dark"
          : next === "light"
            ? "light"
            : window.matchMedia("(prefers-color-scheme: dark)").matches
              ? "dark"
              : "light";
    },
    [],
  );

  const detailJob = detailId === null ? null : (jobs.find((j) => j.id === detailId) ?? null);

  // Drag a URL onto the window → open the add dialog prefilled (M3).
  const onDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  }, []);
  const onDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      const url = extractUrl(e.dataTransfer.getData("text/uri-list") || e.dataTransfer.getData("text/plain"));
      if (url !== null) {
        setAddUrl(url);
        setAddOpen(true);
      }
    },
    [],
  );

  return (
    <main className="shell" onDragOver={onDragOver} onDrop={onDrop}>
      {toasts.length > 0 && (
        <div className="toasts" role="status">
          {toasts.map((toast) => (
            <div key={toast.id} className="toast">
              <span>{toast.text}</span>
              {toast.url !== undefined && (
                <button
                  type="button"
                  className="mini"
                  onClick={() => {
                    setAddUrl(toast.url ?? "");
                    setAddOpen(true);
                    setToasts((cur) => cur.filter((t2) => t2.id !== toast.id));
                  }}
                >
                  {t("toolbar.addUrl")}
                </button>
              )}
              {toast.cancellable === true && (
                <button
                  type="button"
                  className="mini"
                  onClick={() => {
                    void invoke("cancel_post_action");
                    setToasts((cur) => cur.filter((t2) => t2.id !== toast.id));
                  }}
                >
                  {t("toast.cancelPost")}
                </button>
              )}
            </div>
          ))}
        </div>
      )}
      <header className="header">
        <span className="logo" aria-hidden="true" />
        <div>
          <h1>{t("app.title")}</h1>
          <p className="muted">{t("app.subtitle")}</p>
        </div>
        <div className="header-actions">
          <select
            aria-label={t("settings.language")}
            value={lang}
            onChange={(e) => setLang(e.target.value as "en" | "hi")}
          >
            <option value="en">English</option>
            <option value="hi">हिन्दी</option>
          </select>
          <select
            aria-label={t("settings.theme")}
            value={theme}
            onChange={(e) => changeTheme(e.target.value as Theme)}
          >
            <option value="system">{t("settings.themeSystem")}</option>
            <option value="light">{t("settings.themeLight")}</option>
            <option value="dark">{t("settings.themeDark")}</option>
          </select>
        </div>
      </header>

      <div className="toolbar" role="toolbar" aria-label={t("toolbar.addUrl")}>
        <button type="button" className="primary" onClick={() => setAddOpen(true)}>
          {t("toolbar.addUrl")}
        </button>
        <button
          type="button"
          disabled={selectedIds.length !== 1}
          onClick={() => selectedIds[0] !== undefined && void act("resume", selectedIds[0])}
        >
          {t("toolbar.resume")}
        </button>
        <button
          type="button"
          disabled={selectedIds.length !== 1}
          onClick={() => selectedIds[0] !== undefined && void act("pause", selectedIds[0])}
        >
          {t("toolbar.pause")}
        </button>
        <button
          type="button"
          disabled={selectedIds.length === 0}
          onClick={() => void deleteSelected()}
        >
          {t("toolbar.delete")}
        </button>
        <button type="button" onClick={() => void resumeAll()}>
          {t("toolbar.resumeAll")}
        </button>
        <button type="button" onClick={() => void pauseAll()}>
          {t("toolbar.pauseAll")}
        </button>
        <span className="spacer" />
        <button type="button" onClick={() => setSettingsOpen(true)}>
          {t("toolbar.settings")}
        </button>
      </div>

      <div className="content">
        <aside className="sidebar">
          <h3>{t("sidebar.categories")}</h3>
          <button
            type="button"
            className={filter === "all" ? "side-item active" : "side-item"}
            onClick={() => setFilter("all")}
          >
            {t("sidebar.all")}
          </button>
          {categories.map((c) => (
            <button
              key={c.id}
              type="button"
              className={filter === c.id ? "side-item active" : "side-item"}
              onClick={() => setFilter(c.id)}
            >
              {c.name}
            </button>
          ))}
          <QueuePanel queues={queues} onChanged={refreshMeta} />
        </aside>

        <section className="table-wrap">
          {visible.length === 0 ? (
            <p className="muted empty">{t("table.empty")}</p>
          ) : (
            <table className="downloads" data-testid="download-table">
              <thead>
                <tr>
                  <th />
                  <th>{t("table.name")}</th>
                  <th>{t("table.progress")}</th>
                  <th>{t("table.size")}</th>
                  <th>{t("table.status")}</th>
                  <th>{t("table.speed")}</th>
                  <th>{t("table.eta")}</th>
                  <th>
                    <button
                      type="button"
                      className="sort"
                      onClick={() => setSortDesc((v) => !v)}
                    >
                      {t("table.added")}
                    </button>
                  </th>
                </tr>
              </thead>
              <tbody>
                {visible.map((job) => (
                  <JobRow
                    key={job.id}
                    job={job}
                    selected={selected.has(job.id)}
                    onSelect={() =>
                      setSelected((prev) => {
                        const next = new Set(prev);
                        if (next.has(job.id)) next.delete(job.id);
                        else next.add(job.id);
                        return next;
                      })
                    }
                    onOpen={() => setDetailId(job.id)}
                  />
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>

      <footer className="statusbar">
        <span>
          {t("status.activeCount", { count: activeCount })} ·{" "}
          {t("status.globalSpeed")}: {formatSpeed(totalSpeed)}
        </span>
        {quota !== null &&
          (quota.hourlyLimit !== null || quota.dailyLimit !== null) && (
            <span className={quota.exhausted ? "quota exhausted" : "quota"}>
              {t("status.quota")}:{" "}
              {quota.hourlyLimit !== null &&
                `${formatBytes(quota.hourlyUsed)} / ${formatBytes(quota.hourlyLimit)} `}
              {quota.dailyLimit !== null &&
                `${formatBytes(quota.dailyUsed)} / ${formatBytes(quota.dailyLimit)}`}
            </span>
          )}
      </footer>

      {addOpen && (
        <AddUrlDialog
          categories={categories}
          queues={queues}
          initialUrl={addUrl}
          onClose={() => {
            setAddOpen(false);
            setAddUrl("");
          }}
          onAdded={() => {
            setAddOpen(false);
            setAddUrl("");
            refreshMeta();
          }}
        />
      )}
      {settingsOpen && (
        <SettingsDialog onClose={() => setSettingsOpen(false)} />
      )}
      {detailJob && (
        <ProgressDialog job={detailJob} onClose={() => setDetailId(null)} />
      )}
    </main>
  );
}

function JobRow({
  job,
  selected,
  onSelect,
  onOpen,
}: {
  job: JobView;
  selected: boolean;
  onSelect: () => void;
  onOpen: () => void;
}) {
  const { t } = useI18n();
  const pct =
    job.totalLen !== null && job.totalLen > 0
      ? Math.min(100, (job.doneBytes / job.totalLen) * 100)
      : job.state === "done"
        ? 100
        : 0;
  const active = ["downloading", "probing", "verifying"].includes(job.state);
  return (
    <tr
      className={selected ? "row selected" : "row"}
      onDoubleClick={onOpen}
      data-state={job.state}
    >
      <td>
        <input
          type="checkbox"
          checked={selected}
          onChange={onSelect}
          aria-label={job.filename}
        />
      </td>
      <td className="name" title={job.url}>
        {job.filename}
      </td>
      <td className="progress-cell">
        <div className="bar">
          <div className="fill" style={{ width: `${pct}%` }} data-active={active} />
        </div>
      </td>
      <td>{formatBytes(job.totalLen)}</td>
      <td>{t(`status.${job.state}`)}</td>
      <td>{formatSpeed(job.speedBps)}</td>
      <td>{formatEta(job.doneBytes, job.totalLen, job.speedBps)}</td>
      <td>{job.createdAt.slice(0, 16).replace("T", " ")}</td>
    </tr>
  );
}
