// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// @seyd/theme — the Seyd design system as code (docs/design.md).
//
// Importing this module loads the self-hosted fonts and the shared
// stylesheet, and applies the viewer's saved theme choice before first
// paint. The fonts ship with the page (no request to a font CDN), because
// Seyd's surfaces must work self-hosted and offline, and must not leak a
// visitor's address to a third party.
import './fonts.css';
import './ui.css';
import './prompt-deck.css';

export type Theme = 'system' | 'light' | 'dark';
const THEMES: Theme[] = ['system', 'light', 'dark'];
const KEY = 'seyd.theme';

/** The viewer's choice: `system` unless they picked one on this surface. */
export function getTheme(): Theme {
  try {
    const t = localStorage.getItem(KEY) as Theme | null;
    return t && THEMES.includes(t) ? t : 'system';
  } catch { return 'system'; }
}

/** What is in effect right now: a `?theme=` pin for this load, else the saved choice. */
export function currentTheme(): Theme {
  const q = new URLSearchParams(location.search).get('theme') as Theme | null;
  return q && THEMES.includes(q) ? q : getTheme();
}

/**
 * Applies a theme. `system` removes the override so the `prefers-color-scheme`
 * media query decides; the other two set `data-theme` on <html>, which the
 * tokens honour in both directions (tokens.css).
 */
export function setTheme(t: Theme): void {
  const url = new URL(location.href);
  if (url.searchParams.has('theme')) { url.searchParams.delete('theme'); history.replaceState(null, '', url); }
  if (t === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
  try { t === 'system' ? localStorage.removeItem(KEY) : localStorage.setItem(KEY, t); } catch { /* no storage */ }
  document.querySelectorAll<HTMLElement>('.theme-switch').forEach(paint);
}

/** Mounts a system / light / dark segmented control into `host`. */
export function mountThemeSwitch(host: HTMLElement): void {
  host.classList.add('theme-switch');
  host.setAttribute('role', 'group');
  host.setAttribute('aria-label', 'Colour theme');
  host.innerHTML = THEMES.map((t) => `<button type="button" data-theme-choice="${t}">${t}</button>`).join('');
  host.addEventListener('click', (e) => {
    const b = (e.target as HTMLElement).closest<HTMLElement>('[data-theme-choice]');
    if (b) setTheme(b.dataset.themeChoice as Theme);
  });
  paint(host);
}

function paint(host: HTMLElement): void {
  const cur = currentTheme();
  host.querySelectorAll<HTMLElement>('[data-theme-choice]').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.themeChoice === cur)));
}

export { mountPromptDeck, type Prompt } from './prompt-deck';

// Apply the saved choice as early as the module runs. A `?theme=` query
// parameter overrides it for one load, so a screenshot or a test can pin a
// mode without touching the viewer's saved preference.
(() => {
  const t = currentTheme();
  if (t === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
})();
