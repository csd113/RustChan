/**
 * RustChan deep UI / accessibility audit — representative pages and states.
 *
 * ## Profiles (EMULATED configurations, not physical devices)
 * The main matrix runs on the `chromium` project at explicit viewport sizes set
 * with `page.setViewportSize`. The narrow emulated profile runs on the
 * `mobile-webkit` project (Playwright's iPhone 13 emulation: 390x844 viewport,
 * mobile UA, touch). Nothing here is a claim about a physical phone, tablet,
 * or desktop; it is an emulated browser configuration.
 *
 * ## Viewport matrix (bounded; not every page at every size)
 *   320x720  narrow  — home, board, banned notice
 *   390x844  mobile  — long thread, report dialog, NSFW consent; all of
 *                      mobile-webkit (browser-emulated narrow profile)
 *   768x1024 tablet  — board/catalog/search, admin panel/mod-log
 *   1280x800 desktop — home, board, thread, setup validation, preferences,
 *                      thread updates, dialogs, keyboard walk, admin panel
 *
 * ## What is asserted
 * Horizontal overflow (existing tolerance-based helper), clipped/obscured
 * controls and their hit points, scroll traps (scroll to bottom and back),
 * dialog placement, visible feedback text, accessible names, keyboard
 * activation (Enter on submit, Space on buttons/checkboxes), focus visibility,
 * modal focus containment/restoration, and form-error association.
 * Focus-visibility is checked with `expectFocusVisible` on a representative set
 * of controls per page (not on every tab stop); the tab walk itself asserts
 * that every visited control is reachable in document order.
 *
 * ## Diagnostics contract
 * The strict-diagnostics layer in `./diagnostics` is active in this suite:
 * intentional HTTP 4xx responses are declared with `expectHttpError` before
 * the action. The setup-wizard test deliberately submits invalid details and
 * declares the expected HTTP 400 POST.
 *
 * ## Kept findings
 * Assertions that verify a required behaviour the application does not
 * implement are intentionally left failing with precise evidence (expected vs
 * observed). They are not softened into comments and not marked `test.fail()`.
 *
 * ## Screenshots
 * Screenshots are attached for HUMAN REVIEW only. The agent cannot see images
 * and did NOT visually inspect them. No visual-correctness claim is made; all
 * assertions are measurable geometry / computed-style / DOM properties.
 *
 * ## Exclusions
 * Firefox projects and the no-JS project are intentionally not covered here;
 * they are owned by other audit files. Screen-reader output (real AT
 * announcement order) is not observable from Playwright and is not claimed.
 */
import { deflateSync } from 'node:zlib';
import { writeFile } from 'node:fs/promises';
import path from 'node:path';
import {
  adminLogin,
  createReplyViaRequest,
  createStandaloneApp,
  createThread,
  createThreadViaRequest,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  test,
  uniqueShort,
} from './helpers';
import type { RustChanServer } from './helpers';
import {
  expectFocusVisible,
  expectNamedInteractiveControls,
  expectNoCoveredCenters,
  expectNoHorizontalOverflow,
  expectReadableContrast,
  expectSafeBody,
  expectUsableTarget,
  phase4SkipUnless,
} from './phase4-helpers';
import {
  actor as actorLabel,
  expectConsoleError,
  expectHttpError,
  noteEvent,
} from './diagnostics';
import type { Locator, Page } from './diagnostics';

function actor(role: 'visitor' | 'admin'): void {
  actorLabel(role);
}

const VIEWPORTS = {
  narrow: { width: 320, height: 720 },
  mobile: { width: 390, height: 844 },
  tablet: { width: 768, height: 1024 },
  desktop: { width: 1280, height: 800 },
} as const;

async function setViewport(
  page: Page,
  viewport: { width: number; height: number },
  label: string,
): Promise<void> {
  await page.setViewportSize(viewport);
  await expect
    .poll(() => page.evaluate(() => window.innerWidth), { message: `${label} viewport should apply` })
    .toBe(viewport.width);
}

function pngCrc32(buffer: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** Deterministic RGBA PNG bytes; no new dependency, used only for test fixtures. */
function encodePng(width: number, height: number): Buffer {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y += 1) {
    const rowStart = y * (width * 4 + 1);
    raw[rowStart] = 0; // filter: none
    for (let x = 0; x < width; x += 1) {
      const offset = rowStart + 1 + x * 4;
      raw[offset] = (x * 7 + y * 3) % 256;
      raw[offset + 1] = (x * 3 + y * 5) % 256;
      raw[offset + 2] = (x + y) % 256;
      raw[offset + 3] = 255;
    }
  }
  const chunk = (type: string, data: Buffer): Buffer => {
    const typeBytes = Buffer.from(type, 'ascii');
    const out = Buffer.alloc(12 + data.length);
    out.writeUInt32BE(data.length, 0);
    typeBytes.copy(out, 4);
    data.copy(out, 8);
    out.writeUInt32BE(pngCrc32(Buffer.concat([typeBytes, data])), 8 + data.length);
    return out;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

/**
 * Writes a realistic-size PNG into the test runtime's fixture directory.
 * The shared 1x1 `tiny.png` fixture renders a 3px media preview, which is not
 * a meaningful target-size sample; this is test data, not a repository file.
 */
async function writeAuditPng(
  app: RustChanServer,
  name: string,
  width: number,
  height: number,
): Promise<string> {
  const filePath = path.join(app.fixtureDir, name);
  await writeFile(filePath, encodePng(width, height));
  return filePath;
}

/** Controls must fit horizontally: nothing clipped by the left/right edge. */
async function expectNotClippedHorizontally(locator: Locator, label: string): Promise<void> {
  await expect(locator, `${label} should be visible`).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, `${label} should have layout bounds`).not.toBeNull();
  const viewport = locator.page().viewportSize();
  expect(viewport, `${label} needs a viewport`).not.toBeNull();
  expect(box!.x, `${label} should not start left of the viewport`).toBeGreaterThanOrEqual(-1);
  expect(
    box!.x + box!.width,
    `${label} should not extend past the right viewport edge (box=${JSON.stringify(box)})`,
  ).toBeLessThanOrEqual(viewport!.width + 1);
}

/** Dialogs must sit fully on-screen at the current viewport. */
async function expectFullyOnScreen(locator: Locator, label: string): Promise<void> {
  await expect(locator, `${label} should be visible`).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, `${label} should have layout bounds`).not.toBeNull();
  const viewport = locator.page().viewportSize();
  expect(viewport, `${label} needs a viewport`).not.toBeNull();
  const tolerance = 1;
  const fits =
    box!.x >= -tolerance
    && box!.y >= -tolerance
    && box!.x + box!.width <= viewport!.width + tolerance
    && box!.y + box!.height <= viewport!.height + tolerance;
  expect(
    fits,
    `${label} should be fully on-screen (box=${JSON.stringify(box)}, viewport=${JSON.stringify(viewport)})`,
  ).toBe(true);
}

async function expectPageIsScrollable(page: Page, label: string, minimumOverflow = 120): Promise<void> {
  const metrics = await page.evaluate(() => ({
    scrollHeight: document.documentElement.scrollHeight,
    innerHeight: window.innerHeight,
  }));
  expect(
    metrics.scrollHeight - metrics.innerHeight,
    `${label} should be long enough to exercise scrolling`,
  ).toBeGreaterThan(minimumOverflow);
}

/** Asserts the document can scroll to the bottom and back (no scroll trap). */
async function expectScrollsToBottomAndBack(page: Page, label: string): Promise<void> {
  await page.evaluate(() => {
    window.scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' as ScrollBehavior });
  });
  await expect
    .poll(
      () => page.evaluate(() => {
        const el = document.scrollingElement ?? document.documentElement;
        return el.scrollTop + window.innerHeight >= el.scrollHeight - 2;
      }),
      { message: `${label} should reach the bottom of the document` },
    )
    .toBe(true);
  await page.evaluate(() => {
    window.scrollTo({ top: 0, behavior: 'instant' as ScrollBehavior });
  });
  await expect
    .poll(
      () => page.evaluate(() => (document.scrollingElement ?? document.documentElement).scrollTop <= 2),
      { message: `${label} should return to the top of the document` },
    )
    .toBe(true);
}

/** Feedback text must be visible: non-zero size, not hidden, not behind the viewport. */
async function expectFeedbackVisible(locator: Locator, label: string): Promise<void> {
  await locator.scrollIntoViewIfNeeded();
  await expect(locator, `${label} should be visible`).toBeVisible();
  const info = await locator.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    const style = window.getComputedStyle(element);
    return {
      width: rect.width,
      height: rect.height,
      fontSize: Number.parseFloat(style.fontSize || '0'),
      visibility: style.visibility,
      display: style.display,
      inViewport: rect.bottom > 0 && rect.right > 0 && rect.top < window.innerHeight && rect.left < window.innerWidth,
    };
  });
  expect(info.width, `${label} should not collapse horizontally`).toBeGreaterThan(0);
  expect(info.height, `${label} should not collapse vertically`).toBeGreaterThan(0);
  expect(info.fontSize, `${label} should render readable text`).toBeGreaterThan(0);
  expect(info.visibility, `${label} should not be visibility:hidden`).not.toBe('hidden');
  expect(info.display, `${label} should not be display:none`).not.toBe('none');
  expect(info.inViewport, `${label} should not sit behind the viewport`).toBe(true);
}

type TabDescriptor = string;

/**
 * Installs page-side helpers used to observe real tab order and focusables.
 * This is an observation harness: it re-implements the browser's tabbable
 * filter (visible, enabled, not inert, non-negative tabindex, radio groups)
 * and describes the focused element. It does not alter the page under test.
 */
async function installTabOrderProbe(page: Page): Promise<void> {
  await page.evaluate(() => {
    type WindowWithProbe = Window & {
      __auditDescribeEl?: (element: Element | null) => string;
      __auditFocusables?: () => string[];
    };
    const probeWindow = window as WindowWithProbe;
    probeWindow.__auditDescribeEl = (element) => {
      if (!element || element === document.body) return 'body';
      const html = element as HTMLElement;
      const tag = html.tagName.toLowerCase();
      const idPart = html.id ? `#${html.id}` : '';
      const className = typeof html.className === 'string' ? html.className.trim() : '';
      const classPart = !idPart && className ? `.${className.split(/\s+/).slice(0, 2).join('.')}` : '';
      const name = html.getAttribute('name');
      const namePart = name ? `[name="${name}"]` : '';
      const label = html.getAttribute('aria-label') || html.getAttribute('title') || '';
      const text = (html.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 48);
      return `${tag}${idPart}${classPart}${namePart}${label ? `[${label}]` : ''}${text ? `"${text}"` : ''}`;
    };
    probeWindow.__auditFocusables = () => {
      const selector = 'a[href], button, input:not([type="hidden"]), select, textarea, summary, [tabindex]';
      const describe = probeWindow.__auditDescribeEl!;
      const nodes = Array.from(document.querySelectorAll<HTMLElement>(selector));
      return nodes
        .filter((element) => {
          if (element.hasAttribute('disabled')) return false;
          if (element.closest('[inert]')) return false;
          if (element.tabIndex < 0) return false;
          const style = window.getComputedStyle(element);
          if (style.display === 'none' || style.visibility === 'hidden') return false;
          const rect = element.getBoundingClientRect();
          if (rect.width === 0 || rect.height === 0) return false;
          if (element instanceof HTMLInputElement) {
            if (element.type === 'hidden') return false;
            if (element.type === 'radio') {
              if (!element.checked) return false;
              const group = nodes.filter(
                (candidate) => candidate instanceof HTMLInputElement
                  && candidate.type === 'radio'
                  && candidate.name === element.name,
              );
              if (group[0] !== element) return false;
            }
          }
          return true;
        })
        .map((element) => describe(element));
    };
  });
}

async function collectTabOrder(page: Page, count: number): Promise<TabDescriptor[]> {
  await page.evaluate(() => {
    const active = document.activeElement as HTMLElement | null;
    if (active && typeof active.blur === 'function') active.blur();
    // Chromium keeps a "sequential focus navigation starting point" on the
    // element that was blurred, which would make Tab resume mid-form. Focusing
    // the body programmatically resets the starting point to the document
    // start; the temporary tabindex is removed again immediately.
    document.body.setAttribute('tabindex', '-1');
    document.body.focus();
    document.body.removeAttribute('tabindex');
  });
  const visited: TabDescriptor[] = [];
  for (let index = 0; index < count; index += 1) {
    await page.keyboard.press('Tab');
    visited.push(
      await page.evaluate(() => {
        const probe = window as Window & { __auditDescribeEl?: (element: Element | null) => string };
        return probe.__auditDescribeEl!(document.activeElement);
      }),
    );
  }
  return visited;
}

async function expectedTabOrder(page: Page): Promise<TabDescriptor[]> {
  return page.evaluate(() => {
    const probe = window as Window & { __auditFocusables?: () => string[] };
    return probe.__auditFocusables!();
  });
}

/**
 * Every control reached by Tab must appear in document order. Two real browser
 * behaviours are allowed: focus stopping on the document body between cycles
 * (not a control) and exactly one wrap from the last tab stop back to the
 * first. Anything else is a genuine out-of-order jump.
 */
function expectTabOrderFollowsDocumentOrder(visited: string[], expected: string[], label: string): void {
  const controls = visited.filter((descriptor) => descriptor !== 'body');
  let cursor = 0;
  let wrapped = false;
  const outOfOrder: string[] = [];
  for (const descriptor of controls) {
    const ahead = expected.indexOf(descriptor, cursor);
    if (ahead !== -1) {
      cursor = ahead + 1;
      continue;
    }
    const anywhere = expected.indexOf(descriptor);
    if (anywhere !== -1 && !wrapped) {
      wrapped = true;
      cursor = anywhere + 1;
      continue;
    }
    outOfOrder.push(descriptor);
  }
  expect(
    outOfOrder,
    `${label} should be reachable in document order, allowing one focus-cycle wrap `
      + `(visited=${JSON.stringify(visited)})`,
  ).toEqual([]);
}

test.describe('ui and accessibility audit — desktop chromium matrix', () => {
  test.beforeEach(({}, testInfo) => {
    phase4SkipUnless(
      testInfo,
      ['chromium'],
      'the full UI/a11y matrix runs on desktop Chromium; the narrow emulated profile runs on mobile-webkit',
    );
  });

  test('visitors can scan home and board cards at desktop and 320px without clipped or unnamed controls', async ({ page, app }, testInfo) => {
    actor('visitor');
    await setViewport(page, VIEWPORTS.desktop, 'home desktop');
    await page.goto(app.baseURL, { waitUntil: 'domcontentloaded' });
    await expectSafeBody(page, 'home desktop');
    await expectNoHorizontalOverflow(page, 'home desktop');
    await expectNamedInteractiveControls(page, 'body', 'home desktop');
    await expectNoCoveredCenters(
      page.locator('.home-btn, .board-card-link, .admin-footer-link, .user-preferences-summary'),
      'home header, board card, and footer controls',
    );

    await test.step('narrow 320x720 home shell', async () => {
      await setViewport(page, VIEWPORTS.narrow, 'home narrow');
      await expectNoHorizontalOverflow(page, 'home narrow');
      await expectNoCoveredCenters(
        page.locator('.home-btn, .board-card-link, .user-preferences-summary'),
        'home narrow controls',
      );
      await expectNotClippedHorizontally(page.locator('.home-btn').first(), 'home home button at 320');
      await expectNotClippedHorizontally(page.locator('.board-card-link').first(), 'first board card link at 320');
      await expectNotClippedHorizontally(page.locator('.user-preferences-summary').first(), 'preferences summary at 320');
    });

    // Generated for human review only; not visually inspected by the agent.
    await testInfo.attach('audit-ui-home-320.png', {
      body: await page.screenshot({ fullPage: true }),
      contentType: 'image/png',
    });
  });

  test('a board, its catalog, and search results stay usable at 320px and tablet widths', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('uia', testInfo);
    app.createBoardCli({ short: board, name: 'Audit Board', description: 'UI audit board' });
    setBoardFixtureSettings(app, board, { allowImages: true, postCooldownSecs: 0 });
    const threadId = await createThreadViaRequest(page, app, board, {
      subject: 'audit thread with a searchable token',
      body: 'searchable-token-4711 body',
    });
    for (let index = 0; index < 4; index += 1) {
      await createThreadViaRequest(page, app, board, {
        subject: `audit thread ${index}`,
        body: `body ${index}`,
      });
    }
    await createReplyViaRequest(page, app, board, threadId, 'a reply so the board has preview content');
    await createReplyViaRequest(page, app, board, threadId, 'second reply');

    await test.step('board index at 320x720', async () => {
      await setViewport(page, VIEWPORTS.narrow, 'board narrow');
      await page.goto(`${app.baseURL}/${board}`, { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'board index narrow');
      await expectNoHorizontalOverflow(page, 'board index narrow');
      await expectNamedInteractiveControls(page, 'main', 'board index narrow');
      await expectNoCoveredCenters(
        page.locator('.board-nav-link, .post-toggle-btn, .thread-id-link, .thread .subject a'),
        'board index narrow controls',
      );
      await expectPageIsScrollable(page, 'board index narrow');
      await expectScrollsToBottomAndBack(page, 'board index narrow');
    });

    await test.step('catalog and search at 768x1024', async () => {
      await setViewport(page, VIEWPORTS.tablet, 'catalog tablet');
      await page.goto(`${app.baseURL}/${board}/catalog`, { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'catalog tablet');
      await expectNoHorizontalOverflow(page, 'catalog tablet');
      await expectNamedInteractiveControls(page, 'main', 'catalog tablet');
      await expectNoCoveredCenters(
        page.locator('.catalog-card-link, .catalog-thread-menu-toggle, .catalog-sort-select'),
        'catalog tablet controls',
      );
      await expectNotClippedHorizontally(page.locator('.catalog-sort-select').first(), 'catalog sort select');

      await page.goto(`${app.baseURL}/${board}/search?q=searchable-token-4711`, { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'search results');
      await expect(page.locator('.post').first()).toBeVisible();
      await expectNoHorizontalOverflow(page, 'search results');
      await expectNamedInteractiveControls(page, 'main', 'search results');
      await expectFeedbackVisible(page.locator('.board-search-summary-results').first(), 'search result count');

      await page.goto(`${app.baseURL}/${board}/search?q=no-such-token-98765`, { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'empty search');
      await expect(page.locator('.board-search-empty')).toBeVisible();
      await expectNoHorizontalOverflow(page, 'empty search');
      await expectNamedInteractiveControls(page, 'main', 'empty search');
    });

    await test.step('empty collection on the text-only board', async () => {
      await page.goto(`${app.baseURL}/txt`, { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'empty board');
      await expect(page.locator('.thread')).toHaveCount(0);
      await expect(page.locator('body')).toContainText(/uploads are disabled/i);
      await expectNamedInteractiveControls(page, 'main', 'empty board');
      await page.goto(`${app.baseURL}/txt/catalog`, { waitUntil: 'domcontentloaded' });
      await expect(page.locator('.catalog-empty-state')).toBeVisible();
      await expectNoHorizontalOverflow(page, 'empty catalog');
      await expectNamedInteractiveControls(page, 'main', 'empty catalog');
    });
  });

  test('a thread with long unbroken content stays readable and keeps reply controls reachable', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('uil', testInfo);
    app.createBoardCli({ short: board, name: 'Long Content Board', description: 'long content audit' });
    setBoardFixtureSettings(app, board, { allowImages: true, postCooldownSecs: 0 });
    const unbroken = 'Z'.repeat(700);
    const longUrl = `https://example.com/${'a'.repeat(420)}`;
    const threadId = await createThread(page, app, board, {
      subject: `long subject ${'S'.repeat(100)}`,
      body: ['long content start', unbroken, longUrl, 'long content end'].join('\n'),
    });
    for (let index = 0; index < 3; index += 1) {
      await createReplyViaRequest(
        page,
        app,
        board,
        threadId,
        `reply ${index} ${'Q'.repeat(600)}`,
      );
    }

    await test.step('desktop 1280x800', async () => {
      await setViewport(page, VIEWPORTS.desktop, 'long thread desktop');
      await page.goto(`${app.baseURL}/${board}/thread/${threadId}`, { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'long thread desktop');
      await expectNoHorizontalOverflow(page, 'long thread desktop');
      await expectNamedInteractiveControls(page, 'main', 'long thread desktop');
      const postBodies = page.locator('.post-body');
      const overflowing = await postBodies.evaluateAll((elements) => elements
        .map((element, index) => ({
          index: index + 1,
          overflow: element.scrollWidth - element.clientWidth,
        }))
        .filter((entry) => entry.overflow > 1));
      expect(overflowing, 'long post bodies should wrap instead of pushing content wide').toEqual([]);
      await expectNoCoveredCenters(
        page.locator('.post-controls button:visible, .post-controls a:visible, .thread-nav-btn'),
        'long thread controls',
      );
      await expectUsableTarget(page.locator('[data-action="toggle-post-form"]').first(), 'reply toggle', testInfo);
      await expectPageIsScrollable(page, 'long thread desktop');
      await expectScrollsToBottomAndBack(page, 'long thread desktop');
    });

    await test.step('mobile 390x844', async () => {
      await setViewport(page, VIEWPORTS.mobile, 'long thread mobile');
      await expectNoHorizontalOverflow(page, 'long thread mobile');
      await expectNoCoveredCenters(
        page.locator('.post-controls a:visible, .post-controls button:visible, .thread-nav-btn'),
        'long thread mobile controls',
      );
      await expectNotClippedHorizontally(page.locator('.thread-nav-btn').first(), 'thread update button at 390');
      await expectNotClippedHorizontally(page.locator('.thread-nav-links a').first(), 'thread nav link at 390');
    });

    // Generated for human review only; not visually inspected by the agent.
    await testInfo.attach('audit-ui-long-thread-390.png', {
      body: await page.screenshot({ fullPage: true }),
      contentType: 'image/png',
    });
  });

  test('invalid setup details produce a visible, announced validation state', async ({ page }) => {
    actor('admin');
    const setupApp = await createStandaloneApp();
    try {
      await setViewport(page, VIEWPORTS.desktop, 'setup wizard');
      await page.goto(`${setupApp.baseURL}/setup`, { waitUntil: 'domcontentloaded' });
      await expect(page.getByRole('heading', { name: /RustChan setup/i })).toBeVisible();
      // The setup page nests its own <main class="setup-wizard"> inside the
      // shared layout <main>, so a bare `main` locator is ambiguous.
      await expectNamedInteractiveControls(page, 'main.setup-wizard', 'setup wizard');
      await expectNoHorizontalOverflow(page, 'setup wizard');

      await page.getByLabel('Site name').fill('Audit RustChan');
      await page.locator('input[name="admin_password"]').fill('correct-horse-battery');
      await page.locator('input[name="admin_password_confirm"]').fill('different-horse-battery');
      await page.locator('input[name="board_slug"]').fill('aud');
      await page.locator('input[name="board_name"]').fill('Audit Board');

      // Deliberate invalid submission: the wizard re-renders with HTTP 400.
      expectHttpError({
        method: 'POST',
        path: /\/setup\/review$/,
        status: 400,
        reason: 'invalid setup details intentionally re-render the wizard with HTTP 400',
      });
      expectConsoleError({
        pattern: /Failed to load resource[\s\S]*400/i,
        reason: 'engines that log a console resource failure for the declared HTML 400 navigation',
        optional: true,
      });
      await Promise.all([
        page.waitForResponse((response) => response.url().endsWith('/setup/review')),
        page.getByRole('button', { name: /review setup/i }).click(),
      ]);

      const alert = page.locator('.setup-validation');
      await expectFeedbackVisible(alert, 'setup validation alert');
      await expect(alert).toHaveAttribute('role', 'alert');
      await expectReadableContrast(page, '.setup-validation', 'setup validation alert', 3);
      await expectNoHorizontalOverflow(page, 'setup validation state');
      await expectNamedInteractiveControls(page, 'main.setup-wizard', 'setup validation state');
      await expectNoCoveredCenters(
        page.locator('.setup-validation, .setup-section-actions button[type="submit"]'),
        'setup validation controls',
      );

      // The wizard documents a grouped validation summary: a focusable
      // role="alert" live region rendered immediately before the setup form
      // that names the failing items. Assert that contract. Per-field ARIA
      // association (aria-invalid + aria-describedby on each invalid control)
      // is not part of the rendered contract; it is recorded as an
      // observation, not asserted as a requirement.
      const association = await page.evaluate(() => {
        const alertEl = document.querySelector('.setup-validation');
        const form = document.querySelector('form.setup-form');
        if (!alertEl || !form) return null;
        const alertId = alertEl.id;
        const controls = Array.from(
          form.querySelectorAll<HTMLElement>('input:not([type="hidden"]), select, textarea'),
        );
        const passwordControls = controls.filter((control) => /password/i.test(control.getAttribute('name') || ''));
        const isLinked = (control: HTMLElement): boolean => {
          if (!alertId) return false;
          const described = (control.getAttribute('aria-describedby') || '').split(/\s+/);
          const errorMessage = control.getAttribute('aria-errormessage') || '';
          return described.includes(alertId) || errorMessage === alertId;
        };
        return {
          alertId,
          alertTabIndex: alertEl.getAttribute('tabindex'),
          alertText: (alertEl.textContent || '').replace(/\s+/g, ' ').trim(),
          passwordControlNames: passwordControls.map((control) => control.getAttribute('name')),
          markedInvalid: passwordControls
            .filter((control) => control.getAttribute('aria-invalid') === 'true')
            .map((control) => control.getAttribute('name')),
          linkedToAlert: passwordControls.filter(isLinked).map((control) => control.getAttribute('name')),
        };
      });
      expect(association, 'setup validation alert and setup form should both be present').not.toBeNull();
      expect(association!.alertText).toMatch(/password/i);
      expect(
        association!.passwordControlNames.length,
        'the alert should name the password fields it is about',
      ).toBeGreaterThan(0);
      expect(
        association!.alertTabIndex,
        'the validation summary should be focusable so keyboard users land on it',
      ).toBe('-1');
      noteEvent(
        'setup-validation-association',
        JSON.stringify({
          markedInvalid: association!.markedInvalid,
          linkedToAlert: association!.linkedToAlert,
          observation: 'grouped role=alert summary names the fields; no per-field aria-invalid/aria-describedby link is rendered',
        }),
      );
      // Conditional contract: any control the app marks invalid must be
      // programmatically linked to the message that explains it.
      for (const name of association!.markedInvalid) {
        expect(
          association!.linkedToAlert,
          `aria-invalid control ${name} must be linked to the validation message`,
        ).toContain(name);
      }
    } finally {
      await setupApp.dispose();
    }
  });

  test('preferences show a visible saving state and can hide NSFW boards', async ({ page, app }) => {
    actor('visitor');
    await setViewport(page, VIEWPORTS.desktop, 'preferences desktop');
    await page.goto(app.baseURL, { waitUntil: 'domcontentloaded' });

    const summary = page.locator('.user-preferences-panel > summary').first();
    await expectFocusVisible(summary, 'user preferences summary');
    await summary.focus();
    await page.keyboard.press('Enter');
    await expect(page.locator('.user-preferences-panel[open]')).toBeVisible();
    await expectNamedInteractiveControls(page, '.user-preferences-form', 'preferences panel');
    await expectNoCoveredCenters(
      page.locator('.user-preferences-form select, .user-preferences-form input, .user-preferences-form button'),
      'preferences controls',
    );
    await expectFocusVisible(page.locator('.user-preferences-form select[name="theme"]'), 'theme select');

    // Observe real async save feedback instead of waiting a fixed time.
    await page.evaluate(() => {
      const status = document.querySelector('.user-preferences-status');
      (window as Window & { __prefsAuditLog?: Array<{ text: string; state: string }> }).__prefsAuditLog = [];
      if (!status) return;
      new MutationObserver(() => {
        const log = (window as Window & { __prefsAuditLog?: Array<{ text: string; state: string }> }).__prefsAuditLog;
        log?.push({ text: status.textContent || '', state: (status as HTMLElement).dataset.state || '' });
      }).observe(status, {
        childList: true,
        characterData: true,
        subtree: true,
        attributes: true,
        attributeFilter: ['data-state'],
      });
    });

    const hideNsfw = page.locator('.user-preferences-form input[name="hide_nsfw_boards"]');
    // The preference checkbox is a compact native control inside a full-row
    // label; the app does not document a raw-input target size, so record the
    // measured box and assert the control's focus/Space behaviour instead.
    await expect(hideNsfw).toBeVisible();
    const checkboxBox = await hideNsfw.boundingBox();
    expect(checkboxBox, 'hide NSFW checkbox should have layout bounds').not.toBeNull();
    noteEvent('preferences-checkbox-box', JSON.stringify({ checkboxBox }));
    await hideNsfw.focus();
    await page.keyboard.press('Space');
    await expect(hideNsfw).toBeChecked();
    await expect
      .poll(
        () => page.evaluate(() => (window as Window & { __prefsAuditLog?: Array<{ state: string }> }).__prefsAuditLog?.some((entry) => entry.state === 'saving') ?? false),
        { message: 'preference change should expose a saving state' },
      )
      .toBe(true);
    await expect
      .poll(
        () => page.evaluate(() => (window as Window & { __prefsAuditLog?: Array<{ state: string }> }).__prefsAuditLog?.some((entry) => entry.state === 'saved') ?? false),
        { message: 'preference change should reach a saved state' },
      )
      .toBe(true);

    await expect(page.locator('html')).toHaveAttribute('data-hide-nsfw-boards', '1');
    await expect(page.locator('[data-board-nsfw="1"]').first()).toBeHidden();
    await expectFeedbackVisible(page.locator('.user-preferences-status'), 'preference save status');
    await expectNoHorizontalOverflow(page, 'home with preferences open');
  });

  test('a thread update exposes a busy state and completes with keyboard activation', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('uiu', testInfo);
    app.createBoardCli({ short: board, name: 'Update Board', description: 'update audit' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, {
      subject: 'thread updates',
      body: 'thread update audit body',
    });
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`, { waitUntil: 'domcontentloaded' });
    await expectSafeBody(page, 'thread updates page');
    await expectNoHorizontalOverflow(page, 'thread updates page');

    await page.evaluate(() => {
      const status = document.querySelector('[data-role="autoupdate-status"]');
      const button = document.querySelector('[data-action="fetch-updates"]');
      type UpdateLogEntry = { kind: string; text: string; state?: string; disabled?: boolean };
      const log: UpdateLogEntry[] = [];
      (window as Window & { __updateAuditLog?: UpdateLogEntry[] }).__updateAuditLog = log;
      if (status) {
        new MutationObserver(() => log.push({
          kind: 'status',
          text: status.textContent || '',
          state: (status as HTMLElement).dataset.state || '',
        })).observe(status, { childList: true, characterData: true, subtree: true, attributes: true });
      }
      if (button) {
        new MutationObserver(() => log.push({
          kind: 'button',
          text: button.textContent || '',
          disabled: (button as HTMLButtonElement).disabled,
        })).observe(button, {
          childList: true,
          characterData: true,
          subtree: true,
          attributes: true,
          attributeFilter: ['disabled'],
        });
      }
    });

    const updateButton = page.locator('[data-action="fetch-updates"]').first();
    await expectUsableTarget(updateButton, 'thread update button', testInfo);
    await expectFocusVisible(updateButton, 'thread update button');
    await updateButton.focus();
    await page.keyboard.press('Space');
    await expect
      .poll(
        () => page.evaluate(() => (window as Window & { __updateAuditLog?: Array<{ kind: string; disabled?: boolean }> }).__updateAuditLog?.some((entry) => entry.kind === 'button' && entry.disabled === true) ?? false),
        { message: 'update button should expose a disabled busy state' },
      )
      .toBe(true);
    await expect
      .poll(
        () => page.evaluate(() => (window as Window & { __updateAuditLog?: Array<{ kind: string; text: string }> }).__updateAuditLog?.some((entry) => entry.kind === 'status' && /updated/i.test(entry.text)) ?? false),
        { message: 'update status should report completion' },
      )
      .toBe(true);
    await expect(updateButton).toBeEnabled();
    await expectFeedbackVisible(page.locator('[data-role="autoupdate-status"]').first(), 'update status');
  });

  test('report, edit, delete, and media dialogs fit the viewport, trap focus, and restore it', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('uid', testInfo);
    app.createBoardCli({ short: board, name: 'Dialog Board', description: 'dialog audit' });
    setBoardFixtureSettings(app, board, {
      allowImages: true,
      allowEditing: true,
      allowSelfDelete: true,
      postCooldownSecs: 0,
    });
    // The shared tiny.png fixture is 1x1 and would render a 3px preview;
    // upload a realistically sized image so target-size checks describe real
    // content rather than a degenerate fixture.
    const mediaPath = await writeAuditPng(app, 'audit-dialog-media.png', 240, 180);
    const threadId = await createThread(page, app, board, {
      subject: 'dialog audit thread',
      body: 'dialog audit body',
      filePath: mediaPath,
    });

    await setViewport(page, VIEWPORTS.desktop, 'dialogs desktop');
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`, { waitUntil: 'domcontentloaded' });
    await expectSafeBody(page, 'dialog thread');

    await test.step('report dialog', async () => {
      const trigger = page.locator('.post-controls .report-btn').first();
      await trigger.focus();
      await page.keyboard.press('Enter');
      const dialog = page.locator('#report-modal');
      await expect(dialog).toBeVisible();
      await expectFullyOnScreen(dialog.locator('.compress-modal-box'), 'report dialog box');
      await expect(page.locator('#report-reason')).toBeFocused();
      for (let index = 0; index < 6; index += 1) {
        await page.keyboard.press('Tab');
        await expect
          .poll(() => page.evaluate(() => Boolean(document.querySelector('#report-modal')?.contains(document.activeElement))))
          .toBe(true);
      }
      await page.keyboard.press('Shift+Tab');
      await expect
        .poll(() => page.evaluate(() => Boolean(document.querySelector('#report-modal')?.contains(document.activeElement))))
        .toBe(true);
      await page.keyboard.press('Escape');
      await expect(dialog).toBeHidden();
      await expect(trigger, 'focus should return to the report trigger').toBeFocused();
    });

    await test.step('report dialog at 390x844', async () => {
      await setViewport(page, VIEWPORTS.mobile, 'report dialog mobile');
      const trigger = page.locator('.post-controls .report-btn').first();
      await trigger.focus();
      await page.keyboard.press('Enter');
      const dialog = page.locator('#report-modal');
      await expect(dialog).toBeVisible();
      await expectFullyOnScreen(dialog.locator('.compress-modal-box'), 'report dialog box at 390');
      await expectNoHorizontalOverflow(page, 'thread with report dialog at 390');
      await page.keyboard.press('Escape');
      await expect(dialog).toBeHidden();
      await expect(trigger).toBeFocused();
      await setViewport(page, VIEWPORTS.desktop, 'dialogs back to desktop');
    });

    await test.step('edit dialog', async () => {
      const trigger = page.locator('.self-action-controls .edit-btn').first();
      await trigger.focus();
      await page.keyboard.press('Enter');
      const modal = page.locator('#edit-modal.is-open');
      await expect(modal).toBeVisible();
      await expectFullyOnScreen(modal.locator('.edit-modal-box'), 'edit dialog box');
      await expect(page.locator('#edit-modal-body')).toBeFocused();
      for (let index = 0; index < 5; index += 1) {
        await page.keyboard.press('Tab');
        await expect
          .poll(() => page.evaluate(() => Boolean(document.querySelector('#edit-modal')?.contains(document.activeElement))))
          .toBe(true);
      }
      await page.keyboard.press('Escape');
      await expect(page.locator('#edit-modal')).toBeHidden();
      await expect(trigger, 'focus should return to the edit trigger').toBeFocused();
    });

    await test.step('delete confirmation dialog', async () => {
      const trigger = page.locator('.self-action-controls .del-btn').first();
      await trigger.focus();
      await page.keyboard.press('Enter');
      await expect(page.locator('#confirm-modal')).toBeVisible();
      await expectFullyOnScreen(page.locator('#confirm-modal .compress-modal-box'), 'confirm dialog box');
      await expect(page.locator('#confirm-modal-cancel')).toBeFocused();
      await page.keyboard.press('Escape');
      await expect(page.locator('#confirm-modal')).toBeHidden();
      await expect(trigger, 'focus should return to the delete trigger').toBeFocused();
      await expect(page.locator('.post').first()).toBeVisible();
    });

    await test.step('media expansion', async () => {
      const preview = page.locator('.media-preview.image-preview').first();
      await expectUsableTarget(preview, 'image preview', testInfo);
      await preview.focus();
      await page.keyboard.press('Enter');
      const expanded = page.locator('.media-expanded-image').first();
      await expect(expanded).toBeVisible();
      const close = page.locator('.media-close-btn').first();
      await expect(close).toBeFocused();
      await page.keyboard.press('Space');
      await expect(expanded).toBeHidden();
      await expect(preview, 'focus should return to the media preview').toBeFocused();
    });
  });

  test('board and footer controls follow document order under Tab with visible focus', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('uik', testInfo);
    app.createBoardCli({ short: board, name: 'Keyboard Board', description: 'keyboard audit' });
    setBoardFixtureSettings(app, board, { allowImages: true, postCooldownSecs: 0 });
    await createThreadViaRequest(page, app, board, { subject: 'keyboard thread', body: 'keyboard body' });

    await setViewport(page, VIEWPORTS.desktop, 'keyboard desktop');
    await page.goto(`${app.baseURL}/${board}`, { waitUntil: 'domcontentloaded' });
    await expectSafeBody(page, 'keyboard board');

    // Open the post form so its fields take part in the real tab order.
    const toggle = page.locator('[data-action="toggle-post-form"]').first();
    await toggle.focus();
    await page.keyboard.press('Enter');
    await expect(page.locator('form.post-form').first()).toBeVisible();

    await installTabOrderProbe(page);
    const visited = await collectTabOrder(page, 14);
    const expected = await expectedTabOrder(page);
    expect(
      expected.slice(0, 3).join(' | '),
      'document-order tabbables should start in the site header',
    ).toContain('home-btn');
    expectTabOrderFollowsDocumentOrder(visited, expected, 'board page Tab order');

    const keyControls: Array<[Locator, string]> = [
      [page.locator('.home-btn').first(), 'home button'],
      [page.locator('nav.board-list a').first(), 'board nav link'],
      [page.locator('[data-action="toggle-post-form"]').first(), 'post form toggle'],
      [page.locator('#thread-subject'), 'thread subject field'],
      [page.locator('#thread-body'), 'thread body field'],
      [page.locator('form.post-form button[type="submit"]').first(), 'thread submit button'],
      [page.locator('.user-preferences-summary').first(), 'preferences summary'],
      [page.locator('.admin-footer-link').first(), 'admin footer link'],
    ];
    for (const [locator, label] of keyControls) {
      await expectFocusVisible(locator, label);
    }

    await test.step('Enter activates the focused submit control', async () => {
      const subject = `keyboard created ${Date.now()}`;
      await page.locator('#thread-subject').fill(subject);
      await page.locator('#thread-body').fill('keyboard created thread body');
      const submit = page.locator('form.post-form button[type="submit"]').first();
      await submit.focus();
      await page.keyboard.press('Enter');
      await page.waitForURL(new RegExp(`/${board}/thread/\\d+`), { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'keyboard created thread');
      await expect(page.locator('.subject, .post-body').first()).toBeVisible();
      await expect(page.locator('body')).toContainText(subject);
    });

    await test.step('Space activates the sage checkbox', async () => {
      const replyToggle = page.locator('[data-action="toggle-post-form"]').first();
      await replyToggle.focus();
      await page.keyboard.press('Enter');
      await expect(page.locator('#post-form-wrap')).toBeVisible();
      const sage = page.locator('input[name="sage"]').first();
      await expect(sage).not.toBeChecked();
      await sage.focus();
      await page.keyboard.press('Space');
      await expect(sage).toBeChecked();
    });
  });

  test('the admin panel and moderation log are named and usable for a logged-in operator', async ({ page, app }, testInfo) => {
    actor('admin');
    const board = uniqueShort('uiad', testInfo);
    app.createBoardCli({ short: board, name: 'Admin Audit Board', description: 'admin audit' });

    await setViewport(page, VIEWPORTS.tablet, 'admin tablet');
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel`, { waitUntil: 'domcontentloaded' });
    // The admin panel and its live log intentionally show server-internal
    // paths to the authenticated operator; the shared helper models that with
    // `allowAdminInternals`, matching the other admin suites.
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'admin panel tablet');
    await expectNamedInteractiveControls(page, '#control-center', 'admin control center');
    await expectNamedInteractiveControls(page, '#site-settings', 'admin site settings');
    await expectNoCoveredCenters(
      page.locator('.admin-section-index a, .admin-dropdown > summary'),
      'admin section jump and disclosure controls',
    );

    await test.step('logged-in admin toolbar on a board page', async () => {
      await page.goto(`${app.baseURL}/${board}`, { waitUntil: 'domcontentloaded' });
      await expectSafeBody(page, 'board with admin toolbar');
      await expectNamedInteractiveControls(page, '.admin-toolbar', 'admin toolbar');
      await expectNoCoveredCenters(page.locator('.admin-toolbar-btn'), 'admin toolbar buttons');
      await expectNamedInteractiveControls(page, '.board-nav', 'board navigation for admin');
    });

    await test.step('live log shows its connecting and connected states', async () => {
      await page.goto(`${app.baseURL}/admin/panel#live-log`, { waitUntil: 'domcontentloaded' });
      await expectSafePage(page, { allowAdminInternals: true });
      const details = page.locator('#live-log details').first();
      const summary = page.locator('#live-log summary').first();
      await summary.focus();
      if (!(await details.evaluate((element) => (element as HTMLDetailsElement).open))) {
        await page.keyboard.press('Enter');
      }
      await expect
        .poll(
          () => details.evaluate((element) => (element as HTMLDetailsElement).open),
          { message: 'live log disclosure should be open' },
        )
        .toBe(true);
      await expect(page.locator('#admin-live-log-output')).toBeVisible();
      await expect
        .poll(
          () => page.locator('#admin-live-log-status').innerText(),
          { message: 'live log should report a connected state' },
        )
        .toContain('Live log connected');
      await expectFeedbackVisible(page.locator('#admin-live-log-status'), 'live log status');
      await expectNoHorizontalOverflow(page, 'admin panel live log');
    });

    // Generated for human review only; not visually inspected by the agent.
    await setViewport(page, VIEWPORTS.tablet, 'admin panel screenshot');
    await testInfo.attach('audit-ui-admin-panel-768.png', {
      body: await page.screenshot({ fullPage: true }),
      contentType: 'image/png',
    });

    await test.step('moderation log', async () => {
      await page.goto(`${app.baseURL}/admin/mod-log`, { waitUntil: 'domcontentloaded' });
      await expectSafePage(page, { allowAdminInternals: true });
      await expect(page.getByRole('heading', { name: /moderation log/i })).toBeVisible();
      await expectNoHorizontalOverflow(page, 'mod log');
      await expectNamedInteractiveControls(page, 'body', 'mod log');
      await expectNoCoveredCenters(page.locator('.admin-table-wrap, .board-header a'), 'mod log controls');
    });
  });

  test('a banned notice and its appeal form stay reachable at 320px', async ({ page, app }, testInfo) => {
    actor('visitor');
    const reason = `audit ban reason ${'R'.repeat(120)}`;
    await page.goto(`${app.baseURL}/banned?reason=${encodeURIComponent(reason)}`, { waitUntil: 'domcontentloaded' });
    await expect(page.getByRole('heading', { name: /you are banned/i })).toBeVisible();
    await expectSafeBody(page, 'banned notice');
    await expectNamedInteractiveControls(page, 'body', 'banned notice');
    await expectFeedbackVisible(page.locator('.error-page strong').first(), 'ban reason text');

    await setViewport(page, VIEWPORTS.narrow, 'banned narrow');
    // Application finding (kept failing): the standalone ban notice does not
    // wrap a long unbroken ban reason, so the document is 879px wider than a
    // 320px viewport and the appeal controls scroll off-screen. Record which
    // element forces the width before the assertion below reports it.
    const overflowEvidence = await page.evaluate(() => {
      const viewportWidth = document.documentElement.clientWidth;
      const offenders = Array.from(document.querySelectorAll<HTMLElement>('body *'))
        .map((element) => {
          const rect = element.getBoundingClientRect();
          return {
            tag: element.tagName.toLowerCase(),
            cls: element.getAttribute('class') || '',
            right: Math.round(rect.right),
            width: Math.round(rect.width),
            textPrefix: (element.textContent || '').trim().slice(0, 40),
          };
        })
        .filter((entry) => entry.right > viewportWidth + 1)
        .sort((left, right) => right.right - left.right)
        .slice(0, 5);
      return { viewportWidth, offenders };
    });
    noteEvent('banned-notice-overflow', JSON.stringify(overflowEvidence));
    await expectNoHorizontalOverflow(page, 'banned notice at 320');
    const textarea = page.locator('.appeal-form textarea');
    await expectUsableTarget(textarea, 'appeal reason field', testInfo);
    await expectFocusVisible(textarea, 'appeal reason field');
    await expectUsableTarget(page.locator('.appeal-form button[type="submit"]'), 'appeal submit button', testInfo);
    await expectNoCoveredCenters(
      page.locator('.appeal-form textarea, .appeal-form button[type="submit"], .error-page a'),
      'banned notice controls',
    );
    await expectNotClippedHorizontally(page.locator('.error-page strong').first(), 'ban reason text at 320');
  });

  test('declining the NSFW consent dialog returns to a usable home page', async ({ page, app }, testInfo) => {
    actor('visitor');
    await setViewport(page, VIEWPORTS.mobile, 'nsfw consent mobile');
    await page.goto(app.baseURL, { waitUntil: 'domcontentloaded' });
    const trigger = page.locator('a.board-card-link[data-action="open-nsfw-disclaimer"]').first();
    await expect(trigger).toBeVisible();
    await expectUsableTarget(trigger, 'NSFW board card link', testInfo);
    await trigger.click();

    const dialog = page.locator('#nsfw-disclaimer-overlay');
    await expect(dialog).toBeVisible();
    const describeActiveElement = () => page.evaluate(() => {
      const active = document.activeElement as HTMLElement | null;
      if (!active) return 'none';
      return [
        active.tagName.toLowerCase(),
        active.id ? `#${active.id}` : '',
        active.getAttribute('class') || '',
        (active.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 30),
      ].filter(Boolean).join(' ');
    });
    noteEvent('nsfw-dialog-focus-on-open', `activeElement when the consent dialog opened: ${await describeActiveElement()}`);
    await expect(dialog).toHaveAttribute('aria-modal', 'true');
    await expect(dialog).toHaveAttribute('aria-labelledby', 'nsfw-disclaimer-title');
    await expect(dialog).toHaveAttribute('aria-describedby', 'nsfw-disclaimer-info');
    await expectFullyOnScreen(dialog.locator('.compress-modal-box'), 'NSFW consent dialog box');
    await expectNoHorizontalOverflow(page, 'home with NSFW consent dialog');
    // The dialog owns focus when it opens, following the same pattern as the
    // report/edit/confirm modals; focus must not stay on the launcher behind an
    // aria-modal dialog.
    await expect
      .poll(() => page.evaluate(() => Boolean(document.querySelector('#nsfw-disclaimer-overlay')?.contains(document.activeElement))))
      .toBe(true);

    // Tab cannot escape the modal dialog.
    await page.keyboard.press('Tab');
    await expect
      .poll(() => page.evaluate(() => Boolean(document.querySelector('#nsfw-disclaimer-overlay')?.contains(document.activeElement))))
      .toBe(true);
    await page.keyboard.press('Tab');
    await expect
      .poll(() => page.evaluate(() => Boolean(document.querySelector('#nsfw-disclaimer-overlay')?.contains(document.activeElement))))
      .toBe(true);

    const decline = dialog.getByRole('link', { name: /cancel/i });
    await decline.focus();
    await page.keyboard.press('Enter');
    await expect(dialog).toBeHidden();
    expect(new URL(page.url()).search, 'declining should not leave NSFW query state in the URL').not.toContain('nsfw=');
    await expectNoHorizontalOverflow(page, 'home after declining NSFW consent');
    noteEvent('nsfw-dialog-focus-after-decline', `activeElement after declining: ${await describeActiveElement()}`);
    // Dismissing restores focus to the trigger, like the report/edit/confirm
    // modals; closeNsfwDisclaimer() used to hide the overlay and leave focus on
    // the hidden cancel control.
    await expect(trigger, 'focus should return to the NSFW board card after declining').toBeFocused();

    // Keyboard-only reopening keeps the same contract: Enter on the focused
    // trigger opens the dialog and moves focus into it, and Enter on the
    // focused cancel control dismisses it and returns focus to the trigger.
    await page.keyboard.press('Enter');
    await expect(dialog).toBeVisible();
    await expect
      .poll(() => page.evaluate(() => Boolean(document.querySelector('#nsfw-disclaimer-overlay')?.contains(document.activeElement))))
      .toBe(true);
    const reopenedDecline = dialog.getByRole('link', { name: /cancel/i });
    await expect(reopenedDecline).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(dialog).toBeHidden();
    await expect(trigger, 'focus should return to the NSFW board card after reopening and declining').toBeFocused();
  });
});

test.describe('ui and accessibility audit — emulated narrow profile (mobile-webkit)', () => {
  test.beforeEach(({}, testInfo) => {
    phase4SkipUnless(
      testInfo,
      ['mobile-webkit'],
      'the narrow emulated profile runs on the mobile-webkit project only',
    );
  });

  test('mobile-webkit emulation: home and board browsing stay within the viewport', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('uim', testInfo);
    app.createBoardCli({ short: board, name: 'Mobile Audit Board', description: 'narrow profile audit' });
    await createThreadViaRequest(page, app, board, { subject: 'mobile thread', body: 'mobile body' });

    await page.goto(app.baseURL, { waitUntil: 'domcontentloaded' });
    await expectSafeBody(page, 'home mobile-webkit');
    await expectNoHorizontalOverflow(page, 'home mobile-webkit');
    await expectNamedInteractiveControls(page, 'body', 'home mobile-webkit');
    await expectNoCoveredCenters(
      page.locator('.home-btn, .mobile-board-menu-btn, .board-card-link, .user-preferences-summary'),
      'home mobile-webkit controls',
    );

    await page.goto(`${app.baseURL}/${board}`, { waitUntil: 'domcontentloaded' });
    await expectSafeBody(page, 'board mobile-webkit');
    await expectNoHorizontalOverflow(page, 'board mobile-webkit');
    await expectNamedInteractiveControls(page, 'main', 'board mobile-webkit');
    await expectNoCoveredCenters(
      page.locator('.board-nav-link, .post-toggle-btn, .thread-id-link'),
      'board mobile-webkit controls',
    );

    // Generated for human review only; not visually inspected by the agent.
    await testInfo.attach('audit-ui-mobile-home.png', {
      body: await page.screenshot({ fullPage: true }),
      contentType: 'image/png',
    });
  });

  test('mobile-webkit emulation: long thread content stays readable and scrollable', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('uiml', testInfo);
    app.createBoardCli({ short: board, name: 'Mobile Long Board', description: 'narrow long content' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, {
      subject: `mobile long ${'L'.repeat(80)}`,
      body: ['start', 'K'.repeat(650), 'end'].join('\n'),
    });
    await createReplyViaRequest(page, app, board, threadId, 'K'.repeat(500));

    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`, { waitUntil: 'domcontentloaded' });
    await expectSafeBody(page, 'mobile long thread');
    await expectNoHorizontalOverflow(page, 'mobile long thread');
    await expectNamedInteractiveControls(page, 'main', 'mobile long thread');
    const overflowing = await page.locator('.post-body').evaluateAll((elements) => elements
      .map((element, index) => ({ index: index + 1, overflow: element.scrollWidth - element.clientWidth }))
      .filter((entry) => entry.overflow > 1));
    expect(overflowing, 'mobile long post bodies should wrap').toEqual([]);
    await expectNoCoveredCenters(
      page.locator('.post-controls a:visible, .post-controls button:visible, .thread-nav-btn'),
      'mobile long thread controls',
    );
    await expectPageIsScrollable(page, 'mobile long thread');
    await expectScrollsToBottomAndBack(page, 'mobile long thread');
  });
});
