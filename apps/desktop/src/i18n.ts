import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import en from "./locales/en.json";
import hi from "./locales/hi.json";

export type Lang = "en" | "hi";

const dicts: Record<Lang, unknown> = { en, hi };

type Dict = Record<string, unknown>;

function lookup(dict: Dict, path: string): string | undefined {
  const parts = path.split(".");
  let cur: unknown = dict;
  for (const part of parts) {
    if (typeof cur !== "object" || cur === null) return undefined;
    cur = (cur as Dict)[part];
  }
  return typeof cur === "string" ? cur : undefined;
}

export interface I18n {
  lang: Lang;
  setLang: (lang: Lang) => void;
  t: (path: string, vars?: Record<string, string | number>) => string;
  /** Plural-aware lookup: `{path}_one` / `{path}_other` via `Intl.PluralRules`. */
  tp: (path: string, count: number) => string;
}

export function useI18n(): I18n {
  const [lang, setLangState] = useState<Lang>("en");

  useEffect(() => {
    invoke<string | null>("get_setting", { key: "ui.language" })
      .then((v) => {
        if (v === '"hi"' || v === '"en"') setLangState(JSON.parse(v) as Lang);
      })
      .catch(() => {});
  }, []);

  const setLang = useCallback((next: Lang) => {
    setLangState(next);
    void invoke("set_setting", { key: "ui.language", value: JSON.stringify(next) });
  }, []);

  const t = useCallback(
    (path: string, vars?: Record<string, string | number>) => {
      const raw =
        lookup(dicts[lang] as Dict, path) ?? lookup(dicts.en as Dict, path) ?? path;
      if (!vars) return raw;
      return Object.entries(vars).reduce(
        (acc, [k, v]) => acc.replaceAll(`{${k}}`, String(v)),
        raw,
      );
    },
    [lang],
  );

  const tp = useCallback(
    (path: string, count: number) => {
      const category = new Intl.PluralRules(lang).select(count);
      const suffixed = `${path}_${category}`;
      const raw =
        lookup(dicts[lang] as Dict, suffixed) ??
        lookup(dicts.en as Dict, suffixed) ??
        lookup(dicts[lang] as Dict, path) ??
        lookup(dicts.en as Dict, path) ??
        path;
      return raw.replaceAll("{count}", String(count));
    },
    [lang],
  );

  return { lang, setLang, t, tp };
}

// ── Settings / theme ─────────────────────────────────────────────────────

export type Theme = "system" | "light" | "dark";

export function useTheme(): Theme {
  const [theme, setTheme] = useState<Theme>("system");
  useEffect(() => {
    invoke<string | null>("get_setting", { key: "ui.theme" })
      .then((v) => {
        if (v === '"light"' || v === '"dark"' || v === '"system"') {
          setTheme(JSON.parse(v) as Theme);
        }
      })
      .catch(() => {});
  }, []);
  useEffect(() => {
    const root = document.documentElement;
    const apply = () => {
      const dark =
        theme === "dark" ||
        (theme === "system" &&
          window.matchMedia("(prefers-color-scheme: dark)").matches);
      root.dataset.theme = dark ? "dark" : "light";
    };
    apply();
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, [theme]);
  return theme;
}

export async function saveTheme(theme: Theme): Promise<void> {
  await invoke("set_setting", { key: "ui.theme", value: JSON.stringify(theme) });
}
