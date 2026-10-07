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

  useEffect(() => {
    const load = (): void => {
      invoke<SegmentView[]>("job_segments", { id: job.id })
        .then(setSegments)
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
          <button type="button" className="primary" onClick={onClose}>
            {t("progress.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
