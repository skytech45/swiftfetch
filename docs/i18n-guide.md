# SwiftFetch i18n Guide

Filled in during **Milestone 5** (Build Prompt §13.3). English + Hindi ship
complete; the framework is proven for 50+ locales.

## Where locales live

`apps/desktop/src/locales/` — one namespaced JSON file per language:

```
apps/desktop/src/locales/en.json   # reference (must be complete)
apps/desktop/src/locales/hi.json   # Hindi
apps/desktop/src/locales/fr.json   # example: a new locale
```

Namespaces are the top-level keys (`app`, `toolbar`, `grabber`, `mirrors`,
`updater`, …). Nested keys are addressed with dots: `t("grabber.title")`.
The English file is the reference — every other locale must contain every
key (the CI missing-key check enforces this).

## Lookup + fallback

`apps/desktop/src/i18n.ts`:

- `t(path, vars?)` — dot-path lookup in the active language, falling back
  to English, then to the path itself. `{vars}` are interpolated.
- `tp(path, count)` — plural-aware lookup via `Intl.PluralRules`:
  tries `{path}_{category}` (`_one` / `_other`, plus `_few` / `_many` /
  `_zero` where the language needs them), falls back to `{path}`, and
  interpolates `{count}`. English + Hindi both ship `filesFound`,
  `filesFound_one`, `filesFound_other` (see `grabber.filesFound*`).
- Language and theme persist via Tauri settings (`ui.language`,
  `ui.theme`); the `<html data-theme>` attribute drives light/dark CSS.

## Plural rules

Do not hand-roll `count === 1` checks — always use `tp()` so Hindi (and
future languages with richer plural families) behave correctly:

```tsx
const { tp } = useI18n();
<p>{tp("grabber.filesFound", files.length)}</p>
```

## RTL-ready layout

- No hardcoded `left`/`right` positioning in new CSS — use logical
  properties (`margin-inline-start`, `padding-inline-end`, `inset-inline`).
- Icons that imply direction (back/forward chevrons) must flip under
  `[dir="rtl"]` (add `transform: scaleX(-1)` rules when introducing them).
- The document direction defaults to `ltr`; a future RTL locale sets
  `document.dir = "rtl"` at startup (hook point: `useI18n` language load).

## Adding a new locale

1. Copy `en.json` to `<lang>.json` (use the BCP-47 code: `fr`, `de`, `ta`…).
2. Translate every string value; keep keys, `{placeholders}` and `_one` /
   `_other` suffixes intact.
3. Register it in `apps/desktop/src/i18n.ts` (`Lang`, `dicts`) and add the
   option to the language selector in `App.tsx`.
4. Run the missing-key check: `npm run i18n:check` (also runs in CI).
5. Spot-check the UI in both themes; verify plurals with counts 0/1/2.

## CI missing-key check

`scripts/check-i18n.mjs` flattens `en.json` and asserts every other locale
has the identical key set (no missing, no extra, no `null`/empty values).
It runs as `npm run i18n:check` and as a step in the Node CI job — the
build fails on any mismatch.
