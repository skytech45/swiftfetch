import { useEffect, useState } from "react";
import { invoke } from "../jobs";
import { saveTheme, useI18n, type Theme } from "../i18n";

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const [limit, setLimit] = useState<string>("");
  const [theme, setTheme] = useState<Theme>("system");

  useEffect(() => {
    invoke<string | null>("get_setting", { key: "speed.global_kbps" })
      .then((v) => {
        if (v !== null && v !== "null") setLimit(String(JSON.parse(v)));
      })
      .catch(() => {});
    invoke<string | null>("get_setting", { key: "ui.theme" })
      .then((v) => {
        if (v !== null) setTheme(JSON.parse(v) as Theme);
      })
      .catch(() => {});
  }, []);

  const save = async (): Promise<void> => {
    const parsed = limit.trim().length === 0 ? null : Number(limit);
    await invoke("set_global_speed", {
      kibPerS: parsed !== null && Number.isFinite(parsed) && parsed > 0 ? parsed : null,
    });
    await saveTheme(theme);
    // Theme applies immediately via useTheme's persisted read on next mount;
    // apply live here too.
    document.documentElement.dataset.theme =
      theme === "dark"
        ? "dark"
        : theme === "light"
          ? "light"
          : window.matchMedia("(prefers-color-scheme: dark)").matches
            ? "dark"
            : "light";
    onClose();
  };

  return (
    <div className="overlay" role="dialog" aria-modal="true" aria-label={t("settings.title")}>
      <div className="dialog">
        <h2>{t("settings.title")}</h2>
        <label>
          {t("settings.globalLimit")}
          <input
            type="number"
            min={0}
            value={limit}
            placeholder="∞"
            onChange={(e) => setLimit(e.target.value)}
          />
        </label>
        <label>
          {t("settings.theme")}
          <select value={theme} onChange={(e) => setTheme(e.target.value as Theme)}>
            <option value="system">{t("settings.themeSystem")}</option>
            <option value="light">{t("settings.themeLight")}</option>
            <option value="dark">{t("settings.themeDark")}</option>
          </select>
        </label>
        <div className="actions">
          <button type="button" onClick={onClose}>
            {t("addUrl.cancel")}
          </button>
          <button type="button" className="primary" onClick={() => void save()}>
            {t("settings.save")}
          </button>
        </div>
      </div>
    </div>
  );
}
