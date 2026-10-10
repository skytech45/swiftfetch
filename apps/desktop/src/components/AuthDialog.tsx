import { useState } from "react";
import { invoke } from "../jobs";
import { useI18n } from "../i18n";

export interface AuthAccount {
  email: string;
  tier: string;
  trialEndsAt: string | null;
}

export function AuthDialog({
  onAuthed,
  onSkip,
}: {
  onAuthed: (account: AuthAccount) => void;
  onSkip: () => void;
}) {
  const { t } = useI18n();
  const [mode, setMode] = useState<"login" | "register">("login");
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (): Promise<void> => {
    setError(null);
    setBusy(true);
    try {
      const account = await invoke<AuthAccount>(
        mode === "login" ? "auth_signin" : "auth_signup",
        mode === "login" ? { email, password } : { name, email, password },
      );
      onAuthed(account);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label={t("auth.title")}>
      <div className="dialog">
        <h2>SwiftFetch</h2>
        <p className="muted">{t("auth.subtitle")}</p>
        <div className="row">
          <button
            type="button"
            className={mode === "login" ? "primary" : ""}
            onClick={() => setMode("login")}
          >
            {t("auth.login")}
          </button>
          <button
            type="button"
            className={mode === "register" ? "primary" : ""}
            onClick={() => setMode("register")}
          >
            {t("auth.register")}
          </button>
        </div>
        {mode === "register" && (
          <label>
            {t("auth.name")}
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("auth.namePlaceholder")}
              autoFocus
            />
          </label>
        )}
        <label>
          {t("auth.email")}
          <input
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder="you@example.com"
            autoFocus={mode === "login"}
          />
        </label>
        <label>
          {t("auth.password")}
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder={mode === "register" ? t("auth.passwordHint") : ""}
          />
        </label>
        {error !== null && <p className="error">{error}</p>}
        <div className="actions">
          <button type="button" onClick={onSkip}>
            {t("auth.skip")}
          </button>
          <button type="button" className="primary" disabled={busy} onClick={() => void submit()}>
            {mode === "login" ? t("auth.login") : t("auth.register")}
          </button>
        </div>
        <p className="muted small">{t("auth.skipHint")}</p>
      </div>
    </div>
  );
}
