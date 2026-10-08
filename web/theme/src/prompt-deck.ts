// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// The prompt deck (docs/design.md §5). No side effects: a page imports
// `@seyd/theme/prompt-deck.css` beside it, or gets both through ui.css and
// the theme's index.

/** One example prompt of a prompt deck: a chip label and the prompt itself. */
export interface Prompt { label: string; text: string }

/**
 * Mounts a prompt deck (docs/design.md §5) into `host`: the prompts are typed
 * out one after another behind a `›`, each held for a while once complete;
 * the chips jump to one, and the copy button puts the complete prompt on the
 * clipboard. Under `prefers-reduced-motion` nothing is animated: each prompt
 * appears whole and the deck still advances on its own.
 */
export function mountPromptDeck(host: HTMLElement, prompts: Prompt[], opts: { charMs?: number; holdMs?: number } = {}): void {
  if (prompts.length === 0) return;
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const charMs = reduce ? 0 : (opts.charMs ?? 28);
  const holdMs = opts.holdMs ?? 4200;
  host.classList.add('prompt-deck');
  host.innerHTML =
    `<div class="prompt-line" aria-live="polite"><span class="prefix" aria-hidden="true">›</span><span class="text"></span>` +
    `<button type="button" class="small copy" aria-label="Copy this prompt">copy</button></div>` +
    `<div class="chips" role="group" aria-label="Example prompts">` +
    prompts.map((p, i) => `<button type="button" class="small" data-prompt="${i}">${escape(p.label)}</button>`).join('') + `</div>`;
  const line = host.querySelector<HTMLElement>('.prompt-line')!;
  const text = host.querySelector<HTMLElement>('.text')!;
  const copy = host.querySelector<HTMLButtonElement>('.copy')!;
  const chips = Array.from(host.querySelectorAll<HTMLButtonElement>('[data-prompt]'));

  let current = 0;
  let run = 0;           // bumped on every (re)start, so an older typing loop stops itself
  let timer: ReturnType<typeof setTimeout> | undefined;

  const show = (i: number, thenAdvance: boolean): void => {
    const id = ++run;
    clearTimeout(timer);
    current = i;
    chips.forEach((c, j) => c.setAttribute('aria-pressed', String(j === i)));
    line.classList.remove('done');
    text.textContent = '';
    const full = prompts[i].text;
    let n = charMs ? 0 : full.length;
    const step = (): void => {
      if (id !== run) return;
      text.innerHTML = `${escape(full.slice(0, n))}<span class="caret" aria-hidden="true"></span>`;
      if (n < full.length) {
        n += 1;
        // A pause at sentence ends reads as typing rather than printing.
        timer = setTimeout(step, charMs * (/[.?!,]$/.test(full.slice(0, n)) ? 7 : 1));
      } else {
        line.classList.add('done');
        if (thenAdvance) timer = setTimeout(() => show((i + 1) % prompts.length, true), holdMs);
      }
    };
    step();
  };

  chips.forEach((c) => c.addEventListener('click', () => show(Number(c.dataset.prompt), false)));
  copy.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(prompts[current].text);
      copy.textContent = 'copied';
      setTimeout(() => { copy.textContent = 'copy'; }, 1500);
    } catch { /* clipboard unavailable: the text is selectable */ }
  });

  // Start when visible, so a deck below the fold does not type to nobody,
  // and pause the cycle while the reader has picked a prompt themselves.
  if ('IntersectionObserver' in window) {
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) { io.disconnect(); show(0, true); }
    }, { threshold: 0.4 });
    io.observe(host);
  } else {
    show(0, true);
  }
}

function escape(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c] as string));
}
