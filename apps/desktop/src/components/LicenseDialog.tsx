import { useState } from "react";
import { invoke } from "../jobs";
import { useI18n } from "../i18n";
import type { AuthAccount } from "./AuthDialog";

export function LicenseDialog({
  onClose,
  onActivated,
}: {
  onClose: () => void;
  onActivated: (account: AuthAccount) => void;
}) {
  const { t } = useI18n();
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (): Promise<void> => {
    setError(null);
    setBusy(true);
    try {
      const account = await invoke<AuthAccount>("activate_license", { key });
      onActivated(account);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label={t("license.title")}>
      <div className="dialog">
        <h2>{t("license.title")}</h2>
        <p className="muted">{t("license.blurb")}</p>
        <label>
          {t("license.key")}
          <input
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder="SF-XXXX-XXXX-XXXX-XXXX"
            spellCheck={false}
            autoFocus
          />
        </label>
        {error !== null && <p className="error">{error}</p>}
        <div className="actions">
          <button type="button" onClick={onClose}>
            {t("addUrl.cancel")}
          </button>
          <button type="button" className="primary" disabled={busy} onClick={() => void submit()}>
            {t("license.activate")}
          </button>
        </div>
      </div>
    </div>
  );
}
