import { invoke } from "../jobs";
import { useI18n } from "../i18n";

export interface UpdateInfo {
  configured: boolean;
  available: boolean;
  version: string;
  notes: string;
  force: boolean;
  downloadUrl: string | null;
}

export function UpdateDialog({
  info,
  onClose,
}: {
  info: UpdateInfo;
  onClose: () => void;
}) {
  const { t } = useI18n();

  const download = async (): Promise<void> => {
    if (info.downloadUrl !== null) {
      await invoke("preview_open", { url: info.downloadUrl });
    }
    onClose();
  };

  return (
    <div
      className="overlay"
      role="dialog"
      aria-modal="true"
      aria-label={info.force ? t("updater.forceTitle") : t("updater.title")}
    >
      <div className="dialog">
        <h2>{info.force ? t("updater.forceTitle") : t("updater.title")}</h2>
        <p>{info.force ? t("updater.forceBody", { version: info.version }) : t("updater.available", { version: info.version })}</p>
        {info.notes.length > 0 && <pre className="muted small">{info.notes}</pre>}
        <div className="actions">
          {!info.force && (
            <button type="button" onClick={onClose}>
              {t("updater.later")}
            </button>
          )}
          <button type="button" className="primary" onClick={() => void download()}>
            {t("updater.download")}
          </button>
        </div>
      </div>
    </div>
  );
}
