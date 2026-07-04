/* the law as pure functions: each takes a Playwright Page and returns
   whether the invariant holds, plus named offenders if it doesn't.
   No Playwright assertions here — these are DOM evaluations only.
   assertLaw (in index.ts) wraps them with expect() to throw on failure.

   Importable without a build step: Playwright's test runner handles .ts
   natively. @playwright/test is a peerDependency, not a runtime dep of
   the kit — consumers who use the gate already have it. */

import type { Page } from '@playwright/test';

export type InvariantResult =
  | { pass: true }
  | { pass: false; offenders: string[] };

export type InvariantName =
  | 'fontSize'
  | 'radius'
  | 'noPageScroll'
  | 'cursorDefault'
  | 'consoleClean';

export type LawViolation = {
  invariant: InvariantName;
  offenders: string[];
};

/* 1. one size: nothing renders larger than the max (16px content, 13px
      chrome). Sub-pixel tolerance of 0.01 for rounding. */
export async function checkFontSize(
  page: Page,
  max = 16,
): Promise<InvariantResult> {
  const largest = await page.evaluate((maxPx) => {
    let max = 0;
    for (const el of document.querySelectorAll('body *')) {
      if (!(el instanceof HTMLElement)) continue;
      if (!el.offsetParent && el.tagName !== 'BODY') continue; // unrendered
      const size = parseFloat(getComputedStyle(el).fontSize);
      if (size > max) max = size;
    }
    return max;
  }, max);
  if (largest <= max + 0.01) return { pass: true };
  return {
    pass: false,
    offenders: [`max font size ${largest.toFixed(1)}px exceeds ${max}px`],
  };
}

/* 2. radius 0: no element has non-zero border-radius. Round marks are
      SVG circles, never CSS border-radius. */
export async function checkRadius(page: Page): Promise<InvariantResult> {
  const offenders = await page.evaluate(() => {
    const found: string[] = [];
    for (const el of document.querySelectorAll('body *')) {
      const r = getComputedStyle(el).borderRadius;
      if (r && r !== '0px') {
        found.push(`${el.tagName.toLowerCase()}.${el.className} → ${r}`);
        if (found.length > 10) break;
      }
    }
    return found;
  });
  return offenders.length === 0 ? { pass: true } : { pass: false, offenders };
}

/* 3. no page scroll: the page itself never scrolls; stages and desks
      scroll inside. */
export async function checkNoPageScroll(page: Page): Promise<InvariantResult> {
  const scrolls = await page.evaluate(
    () =>
      document.scrollingElement!.scrollHeight >
      document.scrollingElement!.clientHeight + 1,
  );
  return scrolls
    ? { pass: false, offenders: ['page-level scroll detected'] }
    : { pass: true };
}

/* 4. static text keeps the default cursor (no I-beam on non-interactive
      elements). */
export async function checkCursorDefault(page: Page): Promise<InvariantResult> {
  const cursor = await page.evaluate(
    () => getComputedStyle(document.body).cursor,
  );
  return cursor === 'default'
    ? { pass: true }
    : {
        pass: false,
        offenders: [`body cursor is ${cursor}, expected default`],
      };
}

/* 5. clean console: no error-level messages or uncaught page errors.
      Unlike the DOM checks above, this requires collecting errors from
      Playwright event listeners BEFORE navigating. Call collectConsoleErrors
      to set up the listeners, pass the returned array to assertLaw via
      opts.consoleErrors. */
export function collectConsoleErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('pageerror', (err) => errors.push(String(err)));
  return errors;
}

export function checkConsoleClean(errors: string[]): InvariantResult {
  return errors.length === 0
    ? { pass: true }
    : { pass: false, offenders: errors };
}

/* all five, in order. Returns every violation (doesn't short-circuit) so
   the consumer sees the full picture in one run. */
export async function checkAll(
  page: Page,
  opts: {
    maxFontSize?: number;
    consoleErrors?: string[];
    skip?: InvariantName[];
  } = {},
): Promise<LawViolation[]> {
  const { maxFontSize = 16, consoleErrors, skip = [] } = opts;
  const violations: LawViolation[] = [];

  if (!skip.includes('fontSize')) {
    const r = await checkFontSize(page, maxFontSize);
    if (!r.pass)
      violations.push({ invariant: 'fontSize', offenders: r.offenders });
  }
  if (!skip.includes('radius')) {
    const r = await checkRadius(page);
    if (!r.pass)
      violations.push({ invariant: 'radius', offenders: r.offenders });
  }
  if (!skip.includes('noPageScroll')) {
    const r = await checkNoPageScroll(page);
    if (!r.pass)
      violations.push({ invariant: 'noPageScroll', offenders: r.offenders });
  }
  if (!skip.includes('cursorDefault')) {
    const r = await checkCursorDefault(page);
    if (!r.pass)
      violations.push({ invariant: 'cursorDefault', offenders: r.offenders });
  }
  if (!skip.includes('consoleClean') && consoleErrors !== undefined) {
    const r = checkConsoleClean(consoleErrors);
    if (!r.pass)
      violations.push({ invariant: 'consoleClean', offenders: r.offenders });
  }

  return violations;
}
