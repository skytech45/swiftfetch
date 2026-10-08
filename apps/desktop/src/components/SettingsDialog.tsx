import { useEffect, useState } from "react";
import { invoke } from "../jobs";
import { saveTheme, useI18n, type Theme } from "../i18n";

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const [limit, setLimit] = useState<string>("");
  const [theme, setTheme] = useState<Theme>("system");
  const [hourlyMib, setHourlyMib] = useState<string>("");
  const [dailyMib, setDailyMib] = useState<string>("");
  const [clipboard, setClipboard] = useState(false);
  const [channel, setChannel] = useState("stable");
  const [checkStartup, setCheckStartup] = useState(true);
  const [updateUnconfigured, setUpdateUnconfigured] = useState(false);

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
    invoke<string | null>("get_setting", { key: "quota.config" })
      .then((v) => {
        if (v === null) return;
        const cfg = JSON.parse(v) as { hourlyLimit: number | null; dailyLimit: number | null };
        if (cfg.hourlyLimit !== null) setHourlyMib(String(Math.round(cfg.hourlyLimit / (1024 * 1024))));
        if (cfg.dailyLimit !== null) setDailyMib(String(Math.round(cfg.dailyLimit / (1024 * 1024))));
      })
      .catch(() => {});
    invoke<string | null>("get_setting", { key: "clipboard.monitor" })
      .then((v) => {
        if (v !== null) setClipboard(JSON.parse(v) as boolean);
      })
      .catch(() => {});
    invoke<{ channel: string; checkOnStartup: boolean; configured: boolean; version: string }>(
      "get_update_status",
    )
      .then((s) => {
        setChannel(s.channel);
        setCheckStartup(s.checkOnStartup);
        setUpdateUnconfigured(!s.configured);
      })
      .catch(() => {});
  }, []);

  const save = async (): Promise<void> => {
    const parsed = limit.trim().length === 0 ? null : Number(limit);
    await invoke("set_global_speed", {
      kibPerS: parsed !== null && Number.isFinite(parsed) && parsed > 0 ? parsed : null,
    });
    const toBytes = (text: string): number | null => {
      const n = Number(text);
      return text.trim().length > 0 && Number.isFinite(n) && n > 0
        ? Math.round(n * 1024 * 1024)
        : null;
    };
    await invoke("set_setting", {
      key: "quota.config",
      value: JSON.stringify({ hourlyLimit: toBytes(hourlyMib), dailyLimit: toBytes(dailyMib) }),
    });
    await invoke("set_setting", { key: "clipboard.monitor", value: JSON.stringify(clipboard) });
    await invoke("set_setting", { key: "update.channel", value: JSON.stringify(channel) });
    await invoke("set_setting", {
      key: "update.checkOnStartup",
      value: JSON.stringify(checkStartup),
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
        <fieldset className="quota-fields">
          <legend>{t("settings.quota")}</legend>
          <label>
            {t("settings.quotaHourly")}
            <input
              type="number"
              min={0}
              value={hourlyMib}
              placeholder="∞"
              onChange={(e) => setHourlyMib(e.target.value)}
            />
          </label>
          <label>
            {t("settings.quotaDaily")}
            <input
              type="number"
              min={0}
              value={dailyMib}
              placeholder="∞"
              onChange={(e) => setDailyMib(e.target.value)}
            />
          </label>
        </fieldset>
        <label className="check">
          <input
            type="checkbox"
            checked={clipboard}
            onChange={(e) => setClipboard(e.target.checked)}
          />
          {t("settings.clipboard")}
        </label>
        <label>
          {t("settings.theme")}
          <select value={theme} onChange={(e) => setTheme(e.target.value as Theme)}>
            <option value="system">{t("settings.themeSystem")}</option>
            <option value="light">{t("settings.themeLight")}</option>
            <option value="dark">{t("settings.themeDark")}</option>
          </select>
        </label>
        <fieldset className="quota-fields">
          <legend>{t("updater.title")}</legend>
          <label>
            {t("updater.channel")}
            <select value={channel} onChange={(e) => setChannel(e.target.value)}>
              <option value="stable">{t("updater.stable")}</option>
              <option value="beta">{t("updater.beta")}</option>
            </select>
          </label>
          <label className="check">
            <input
              type="checkbox"
              checked={checkStartup}
              onChange={(e) => setCheckStartup(e.target.checked)}
            />
            {t("updater.checkOnStartup")}
          </label>
          {updateUnconfigured && <p className="muted">{t("updater.disabled")}</p>}
        </fieldset>
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
