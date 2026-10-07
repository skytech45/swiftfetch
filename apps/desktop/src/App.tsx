import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "./jobs";
import {
  formatBytes,
  formatEta,
  formatSpeed,
  installEventBridge,
  useJobs,
  type CategoryView,
  type JobView,
  type QueueView,
} from "./jobs";
import { saveTheme, useI18n, useTheme, type Theme } from "./i18n";
import { AddUrlDialog } from "./components/AddUrlDialog";
import { ProgressDialog } from "./components/ProgressDialog";
import { QueuePanel } from "./components/QueuePanel";
import { SettingsDialog } from "./components/SettingsDialog";

export default function App() {
  const { t, lang, setLang } = useI18n();
  const theme = useTheme();
  const jobs = useJobs();
  const [categories, setCategories] = useState<CategoryView[]>([]);
  const [queues, setQueues] = useState<QueueView[]>([]);
  const [filter, setFilter] = useState<string>("all");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [addOpen, setAddOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [sortDesc, setSortDesc] = useState(false);

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
    return () => {
      void un.then((f) => f());
      window.matchMedia("(prefers-color-scheme: dark)").removeEventListener("change", onTheme);
    };
  }, [refreshMeta]);

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

  return (
    <main className="shell">
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
      </footer>

      {addOpen && (
        <AddUrlDialog
          categories={categories}
          queues={queues}
          onClose={() => setAddOpen(false)}
          onAdded={() => {
            setAddOpen(false);
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
