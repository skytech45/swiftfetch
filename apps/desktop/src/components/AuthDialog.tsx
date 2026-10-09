import { useState } from "react";
import { invoke } from "../jobs";
import { useI18n } from "../i18n";

export interface AuthAccount {
  email: string;
  tier: string;
}

export function AuthDialog({ onAuthed }: { onAuthed: (account: AuthAccount) => void }) {
  const { t } = useI18n();
  const [mode, setMode] = useState<"login" | "register">("login");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (): Promise<void> => {
    setError(null);
    setBusy(true);
    try {
      const account = await invoke<AuthAccount>(mode === "login" ? "auth_signin" : "auth_signup", {
        email,
        password,
      });
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
        <label>
          {t("auth.email")}
          <input
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder="you@example.com"
            autoFocus
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
          <button type="button" className="primary" disabled={busy} onClick={() => void submit()}>
            {mode === "login" ? t("auth.login") : t("auth.register")}
          </button>
        </div>
      </div>
    </div>
  );
}
