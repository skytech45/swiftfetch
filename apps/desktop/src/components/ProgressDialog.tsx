import { useEffect, useState } from "react";
import { formatBytes, invoke, type JobView, type SegmentView } from "../jobs";
import { useI18n } from "../i18n";

export function ProgressDialog({
  job,
  onClose,
}: {
  job: JobView;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [segments, setSegments] = useState<SegmentView[]>([]);
  const [mirrors, setMirrors] = useState<{ url: string; priority: number; fails: number; bytesOk: number }[]>([]);
  const [mirrorUrl, setMirrorUrl] = useState("");
  const [notice, setNotice] = useState<string | null>(null);

  const verify = async (): Promise<void> => {
    setNotice(null);
    try {
      const ok = await invoke<boolean>("verify_job", { id: job.id });
      setNotice(ok ? t("checksum.verified") : t("checksum.failed"));
    } catch (err) {
      setNotice(String(err));
    }
  };

  const preview = async (): Promise<void> => {
    setNotice(null);
    try {
      const [token, url] = await invoke<[string, string]>("preview_start", { id: job.id });
      await invoke("preview_open", { url });
      setNotice(`${t("preview.opened")} (${token.slice(0, 8)}…)`);
    } catch (err) {
      setNotice(String(err));
    }
  };

  useEffect(() => {
    const load = (): void => {
      invoke<SegmentView[]>("job_segments", { id: job.id })
        .then(setSegments)
        .catch(() => {});
      invoke<{ url: string; priority: number; fails: number; bytesOk: number }[]>("list_mirrors", {
        jobId: job.id,
      })
        .then(setMirrors)
        .catch(() => {});
    };
    load();
    const timer = window.setInterval(load, 500);
    return () => window.clearInterval(timer);
  }, [job.id]);

  const totalPct =
    job.totalLen !== null && job.totalLen > 0
      ? Math.min(100, (job.doneBytes / job.totalLen) * 100)
      : 0;

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label={t("progress.title")}>
      <div className="dialog wide">
        <h2>{t("progress.title")}</h2>
        <dl className="kv">
          <dt>{t("table.name")}</dt>
          <dd>{job.filename}</dd>
          <dt>URL</dt>
          <dd className="muted">{job.url}</dd>
          <dt>{t("table.progress")}</dt>
          <dd>
            {formatBytes(job.doneBytes)} / {formatBytes(job.totalLen)} (
            {totalPct.toFixed(1)}%)
          </dd>
        </dl>
        <h3>{t("progress.segments")}</h3>
        <div className="segments">
          {segments.map((s) => {
            const size = s.end - s.start + 1;
            const pct = size > 0 ? (s.done / size) * 100 : 0;
            return (
              <div key={s.idx} className="segment" title={t("progress.segment", { idx: s.idx })}>
                <div className="bar small">
                  <div className="fill" style={{ width: `${pct}%` }} />
                </div>
                <span className="muted small">{s.state}</span>
              </div>
            );
          })}
        </div>
        <div className="actions">
          <button type="button" onClick={() => void verify()}>
            {t("checksum.verify")}
          </button>
          <button type="button" onClick={() => void preview()}>
            {t("preview.button")}
          </button>
          <button type="button" className="primary" onClick={onClose}>
            {t("progress.close")}
          </button>
        </div>
        {notice !== null && <p className="muted small">{notice}</p>}
        <h3>{t("mirrors.title")}</h3>
        {mirrors.length === 0 ? (
          <p className="muted small">{t("mirrors.empty")}</p>
        ) : (
          <ul className="mirrors">
            {mirrors.map((m) => (
              <li key={m.url}>
                <span className="muted small">{m.url}</span>
                <button
                  type="button"
                  aria-label={t("mirrors.remove")}
                  onClick={() => {
                    void invoke("remove_mirror", { jobId: job.id, url: m.url }).then(() =>
                      setMirrors((cur) => cur.filter((x) => x.url !== m.url)),
                    );
                  }}
                >
                  {t("mirrors.remove")}
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="row">
          <input
            value={mirrorUrl}
            onChange={(e) => setMirrorUrl(e.target.value)}
            placeholder={t("mirrors.urlPlaceholder")}
          />
          <button
            type="button"
            onClick={() => {
              const url = mirrorUrl.trim();
              if (url.length === 0) return;
              void invoke("add_mirror", { jobId: job.id, url }).then(() => {
                setMirrors((cur) => [...cur, { url, priority: 0, fails: 0, bytesOk: 0 }]);
                setMirrorUrl("");
              });
            }}
          >
            {t("mirrors.add")}
          </button>
        </div>
      </div>
    </div>
  );
}
