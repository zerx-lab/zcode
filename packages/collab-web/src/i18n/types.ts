/** Locale identifiers with a bundled dictionary. `en` is the source language. */
export type LocaleId = "en" | "zh-CN";

/** Explicit user choice, or `auto` to follow the browser's language list. */
export type LocalePreference = "auto" | LocaleId;

/**
 * Translation dictionary keyed by the **English source string**.
 * A missing key falls back to the key itself, so an upstream copy edit
 * degrades to English instead of showing a stale translation.
 */
export type Dict = Readonly<Record<string, string>>;
