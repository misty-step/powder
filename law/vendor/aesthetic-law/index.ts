/* the law as a consumer-enforceable gate.

   Import from '@misty-step/aesthetic/law' in your Playwright tests:

     import { assertLaw, collectConsoleErrors } from '@misty-step/aesthetic/law';

     test('my app holds the law', async ({ page }) => {
       const errors = collectConsoleErrors(page);
       await page.goto('/dashboard');
       await assertLaw(page, { consoleErrors: errors });
     });

   Or sweep routes in both modes:

     import { assertLawRoute } from '@misty-step/aesthetic/law';

     for (const route of ['/dashboard', '/settings']) {
       for (const mode of ['light', 'dark'] as const) {
         test(`${route} · ${mode}`, assertLawRoute(route, mode));
       }
     }

   On failure, assertLaw throws with named offenders — which invariant
   broke and which elements caused it. No silent pass/fail. */

import { expect, type Page } from '@playwright/test';
import {
  checkAll,
  collectConsoleErrors,
  type InvariantName,
  type LawViolation,
} from './invariants.js';

export type AssertLawOptions = {
  /** Max font size in px (default 16). Override if your chrome uses a smaller size. */
  maxFontSize?: number;
  /** Errors collected via collectConsoleErrors(page) before navigation. */
  consoleErrors?: string[];
  /** Invariants to skip (e.g. ['fontSize'] if you intentionally use larger headings). */
  skip?: InvariantName[];
};

/** Assert the law holds on the current page. Throws with named offenders on failure. */
export async function assertLaw(
  page: Page,
  opts: AssertLawOptions = {},
): Promise<void> {
  const violations = await checkAll(page, opts);
  if (violations.length === 0) return;

  const message = violations
    .map(
      (v: LawViolation) =>
        `✗ law violation: ${v.invariant}\n  offenders:\n    ${v.offenders.join('\n    ')}`,
    )
    .join('\n');
  expect.fail(`\n${message}\n`);
}

/** Returns a Playwright test function that navigates to a route, optionally
    sets the aesthetic mode, and asserts the law. Use directly as the test body:

    test('dashboard · light', assertLawRoute('/dashboard', 'light')); */
export function assertLawRoute(
  route: string,
  mode?: 'light' | 'dark',
  opts?: Omit<AssertLawOptions, 'consoleErrors'>,
): (args: { page: Page }) => Promise<void> {
  return async ({ page }) => {
    const errors = collectConsoleErrors(page);
    if (mode) {
      await page.addInitScript((m: string) => {
        localStorage.setItem('ae-mode', m);
      }, mode);
    }
    await page.goto(route);
    await page.waitForLoadState('networkidle');
    await assertLaw(page, { ...opts, consoleErrors: errors });
  };
}

// re-exports for consumer convenience
export {
  collectConsoleErrors,
  checkFontSize,
  checkRadius,
  checkNoPageScroll,
  checkCursorDefault,
  checkConsoleClean,
  checkAll,
} from './invariants.js';
export type {
  InvariantResult,
  InvariantName,
  LawViolation,
} from './invariants.js';
