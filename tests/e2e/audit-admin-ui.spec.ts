import { actor, expectHttpError, journeyStep, newAuditedContext, noteEvent } from './diagnostics';
import {
  adminLogin,
  createReply,
  createStandaloneApp,
  createThread,
  expect,
  expectAdminPanel,
  expectSafePage,
  sqliteQuery,
  test,
  uniqueShort,
  type RustChanServer,
} from './helpers';
import { deflateSync } from 'node:zlib';
import fsp from 'node:fs/promises';
import path from 'node:path';
import type { Locator, Page, Response } from '@playwright/test';

/**
 * Admin-only surfaces that previously had API-only or zero browser coverage.
 *
 * Applicability and exclusions: this file runs on Chromium only
 * (`test.skip(testInfo.project.name !== 'chromium', ...)` per test). Every
 * scenario drives admin form controls, file inputs, download events and
 * destructive maintenance; the application contract is engine independent, so
 * repeating it across WebKit/Firefox/mobile would add runtime without adding
 * information. Admin no-JavaScript behaviour is covered elsewhere
 * (`firefox-nojs-public.spec.ts`, `nojs-enhancements.spec.ts`,
 * `audit-nojs-parity.spec.ts`), so this file always runs with JavaScript on.
 *
 * Browser interaction is the subject; SQLite reads only corroborate. Restore and
 * extract scenarios run against dedicated standalone instances because they are
 * destructive by contract and must never touch a shared runtime.
 *
 * Interaction notes learned from the rendered DOM:
 *  - `<input type="file">` maps to ARIA role `button`, so submit controls are
 *    selected with `button[type="submit"]` rather than `getByRole('button')`.
 *  - Admin panel controls live inside collapsed `<details class="admin-dropdown">`
 *    elements; the dropdowns are opened through their real `<summary>` before use.
 *  - Most admin saves are plain form POSTs, but backup create and restore-upload
 *    are XHR driven by `admin.js` and expose their own progress/done affordances.
 */

/**
 * Open every collapsed `<details>` inside `scope` through its real `<summary>`,
 * which is what an operator does before reaching the control inside it. The
 * admin panel nests controls in several different disclosure widgets
 * (`admin-dropdown`, `backup-manual-details`, per-board cards), so scoping to a
 * class list would silently miss one. Only visible summaries are clicked, so the
 * responsive mobile board menu (hidden at desktop widths) is never targeted.
 */
async function openDetailsIn(page: Page, scope: string): Promise<void> {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const closed = page.locator(`${scope} details:not([open]) > summary:visible`).first();
    if (await closed.count() === 0) return;
    await closed.click();
  }
}

async function panelSection(page: Page, app: RustChanServer, section: string): Promise<void> {
  await page.goto(`${app.baseURL}/admin/panel?open=${section}#${section}`);
  await expect(page.locator(`#${section}`)).toBeAttached();
  await openDetailsIn(page, '.admin-panel');
  await expect(page.locator(`#${section}`)).toBeVisible();
  await expectSafePage(page, { allowAdminInternals: true });
}

/** Click a form's real submit button and wait for the mutation request it produces. */
async function submitAndSettle(page: Page, form: Locator, actionPath: string, timeout = 90_000): Promise<Response> {
  const submit = form.locator('button[type="submit"]').first();
  await expect(submit, `submit control for POST ${actionPath}`).toBeVisible();
  const [response] = await Promise.all([
    page.waitForResponse(
      candidate => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === actionPath,
      { timeout },
    ),
    submit.click(),
  ]);
  expect([200, 303], `POST ${actionPath} status`).toContain(response.status());
  return response;
}

/**
 * Submit a form whose submit button carries `data-confirm`. main.js shows the
 * styled confirmation modal and only then runs the submission, which admin.js
 * may further intercept as an XHR (restore upload). Both steps are driven.
 */
async function submitWithConfirm(page: Page, form: Locator, actionPath: string, timeout = 120_000): Promise<Response> {
  const submit = form.locator('button[type="submit"]').first();
  await expect(submit, `submit control for POST ${actionPath}`).toBeVisible();
  const [response] = await Promise.all([
    page.waitForResponse(
      candidate => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === actionPath,
      { timeout },
    ),
    (async () => {
      await submit.click();
      const modal = page.locator('#confirm-modal');
      await expect(modal, `confirmation modal for POST ${actionPath}`).toBeVisible();
      await modal.locator('#confirm-modal-continue').click();
    })(),
  ]);
  return response;
}

/** Confirm through the styled confirmation modal the application renders for data-confirm. */
async function confirmModal(page: Page, trigger: () => Promise<void>, timeout = 90_000): Promise<void> {
  await trigger();
  const modal = page.locator('#confirm-modal');
  await expect(modal).toBeVisible();
  await Promise.all([
    page.waitForResponse(response => response.request().method() === 'POST', { timeout }),
    modal.locator('#confirm-modal-continue').click(),
  ]);
}

/**
 * Banners must be at least 468x60 with the exact 468:60 aspect ratio
 * (src/banner.rs DISPLAY_WIDTH/DISPLAY_HEIGHT), so the 1x1 fixture PNG is not
 * usable here. Encode a real 468x60 PNG instead of mocking the upload.
 */
async function encodedPng(width: number, height: number, dir: string, name: string): Promise<string> {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y += 1) {
    const rowStart = y * (width * 4 + 1);
    raw[rowStart] = 0;
    for (let x = 0; x < width; x += 1) {
      const offset = rowStart + 1 + x * 4;
      raw[offset] = (x * 3) % 256;
      raw[offset + 1] = (y * 5) % 256;
      raw[offset + 2] = 128;
      raw[offset + 3] = 255;
    }
  }
  const chunk = (type: string, data: Buffer): Buffer => {
    const typeBytes = Buffer.from(type, 'ascii');
    const out = Buffer.alloc(12 + data.length);
    out.writeUInt32BE(data.length, 0);
    typeBytes.copy(out, 4);
    data.copy(out, 8);
    out.writeUInt32BE(crc32(Buffer.concat([typeBytes, data])), 8 + data.length);
    return out;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  const png = Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
  const target = path.join(dir, name);
  await fsp.mkdir(dir, { recursive: true });
  await fsp.writeFile(target, png);
  return target;
}

/** Banners require the exact 468:60 aspect ratio (src/banner.rs). */
function bannerPng(dir: string, name = 'deep-audit-banner.png'): Promise<string> {
  return encodedPng(468, 60, dir, name);
}

/** Favicons must be exactly 512x512 pixels (src/favicon.rs decode_uploaded_favicon). */
function faviconPng(dir: string, name = 'deep-audit-favicon.png'): Promise<string> {
  return encodedPng(512, 512, dir, name);
}

function crc32(data: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of data) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/**
 * Reach a board's new-thread form the way a poster does. The form starts
 * collapsed behind the post toggle on narrow layouts, so the toggle is clicked
 * when it is present and the subject field is used as the readiness signal.
 */
async function openBoardPostForm(page: Page, board: string): Promise<Locator> {
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) await toggle.click();
  const form = page.locator(`form[action="/${board}"]`).first();
  await expect(form.locator('input[name="subject"]')).toBeVisible();
  return form;
}

async function uploadHomeBanner(page: Page, app: RustChanServer, expectedRows: number): Promise<void> {
  const form = page.locator('form[action="/admin/home/banner"]').first();
  await expect(form).toBeVisible();
  await form.locator('input[type="file"][name="banner"]')
    .setInputFiles(await bannerPng(app.fixtureDir, `deep-audit-home-banner-${expectedRows}.png`));
  await submitAndSettle(page, form, '/admin/home/banner');
  await expect(page.locator('#home-banners .admin-banner-row'), 'uploaded banner rows').toHaveCount(expectedRows);
}

test.describe('admin surface journeys', () => {
  test('global favicon uploaded through the real file input is served with image headers', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('admin');
    await adminLogin(page, app);
    await panelSection(page, app, 'site-settings');

    expect((await page.request.get(`${app.baseURL}/favicon-32x32.png`)).status()).toBe(404);

    const form = page.locator('form[action="/admin/site/favicon"]').first();
    await expect(form.locator('input[type="file"][name="favicon"]')).toBeAttached();
    // Favicons must be exactly 512x512; a 1x1 fixture is rejected by design.
    await form.locator('input[type="file"][name="favicon"]').setInputFiles(await faviconPng(app.fixtureDir));
    await submitAndSettle(page, form, '/admin/site/favicon');
    await expectAdminPanel(page);
    await expect(page.locator('.admin-flash').filter({ hasText: /Global favicon updated/i }).first(), 'favicon success feedback').toBeVisible();

    for (const name of ['favicon-16x16.png', 'favicon-32x32.png', 'apple-touch-icon.png', 'favicon.ico']) {
      const response = await page.request.get(`${app.baseURL}/${name}`);
      expect(response.status(), `${name} after upload`).toBe(200);
      expect(response.headers()['content-type'], `${name} content type`).toMatch(/image\//);
      expect((await response.body()).length, `${name} body`).toBeGreaterThan(0);
    }

    actor('visitor');
    await page.goto(`${app.baseURL}/`);
    const iconLinks = await page.locator('link[rel*="icon"], link[rel="apple-touch-icon"]')
      .evaluateAll(links => links.map(link => link.getAttribute('href') ?? ''));
    expect(iconLinks.length, 'uploaded favourites should be advertised in the document head').toBeGreaterThan(0);
    expect(iconLinks.every(href => href.includes('v=')), `versioned icon links, received ${JSON.stringify(iconLinks)}`).toBe(true);

    await test.step('board favicon override then clear through the real forms', async () => {
      actor('admin');
      await panelSection(page, app, 'appearance');
      const boardUpload = page.locator('form[action="/admin/board/favicon"]').first();
      await expect(boardUpload).toBeVisible();
      await boardUpload.locator('input[type="file"][name="favicon"]').setInputFiles(await faviconPng(app.fixtureDir, 'deep-audit-board-favicon.png'));
      await submitAndSettle(page, boardUpload, '/admin/board/favicon');
      expect((await page.request.get(`${app.baseURL}/boards/pub/_favicon/favicon-32x32.png`)).status()).toBe(200);

      actor('visitor');
      await page.goto(`${app.baseURL}/pub`);
      const boardIcons = await page.locator('link[rel*="icon"]')
        .evaluateAll(links => links.map(link => link.getAttribute('href') ?? ''));
      expect(boardIcons.length, 'board favicon set should be advertised').toBeGreaterThan(0);
      expect(boardIcons.every(href => href.startsWith('/boards/pub/_favicon/')), `board-scoped icon links, received ${JSON.stringify(boardIcons)}`).toBe(true);

      actor('admin');
      await panelSection(page, app, 'appearance');
      const clear = page.locator('form[action="/admin/board/favicon/clear"]').first();
      await expect(clear).toBeVisible();
      await submitAndSettle(page, clear, '/admin/board/favicon/clear');
      expect((await page.request.get(`${app.baseURL}/boards/pub/_favicon/favicon-32x32.png`)).status()).toBe(404);

      actor('visitor');
      await page.goto(`${app.baseURL}/pub`);
      const fallback = await page.locator('link[rel*="icon"]')
        .evaluateAll(links => links.map(link => link.getAttribute('href') ?? ''));
      expect(fallback.length, 'global favicon should be advertised again').toBeGreaterThan(0);
      expect(fallback.every(href => !href.startsWith('/boards/pub/_favicon/')), `global-scoped icon links, received ${JSON.stringify(fallback)}`).toBe(true);
    });
    await journeyStep(testInfo, page, 'favicon-upload');
  });

  test('vacuum runs from the admin panel and the site keeps serving its data', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('poster');
    await createThread(page, app, 'pub', { subject: 'vacuum survivor', body: 'vacuum survivor body' });
    const postsBefore = sqliteQuery(app, 'SELECT COUNT(*) FROM posts;');
    expect(Number(postsBefore)).toBeGreaterThan(0);

    actor('admin');
    await adminLogin(page, app);
    await panelSection(page, app, 'maintenance');

    const button = page.locator('#maintenance button[type="submit"]', { hasText: /run VACUUM/i }).first();
    await expect(button).toBeVisible();
    await confirmModal(page, async () => {
      await button.click();
    });
    // The documented contract is a rendered VACUUM result page at /admin/vacuum
    // (before/after sizes), not a redirect back to the panel.
    await page.waitForURL(/\/admin\/vacuum/, { timeout: 90_000 });
    await expect(page.locator('body')).toContainText(/VACUUM complete/i);
    await expect(page.locator('body')).toContainText(/Before/i);
    await expect(page.locator('body')).toContainText(/After/i);

    expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;'), 'vacuum must not drop rows').toBe(postsBefore);
    expect(sqliteQuery(app, 'PRAGMA quick_check;')).toBe('ok');
    actor('visitor');
    await page.goto(`${app.baseURL}/pub`);
    await expect(page.locator('body')).toContainText('vacuum survivor');
  });

  test('home banner uploaded through the file input renders on the homepage and deletes through the panel', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('admin');
    await adminLogin(page, app);
    await panelSection(page, app, 'home-banners');
    await uploadHomeBanner(page, app, 1);

    actor('visitor');
    await page.goto(`${app.baseURL}/`);
    const rendered = page.locator('img[src*="/banner/assets/"]').first();
    await expect(rendered).toBeVisible();
    const src = await rendered.getAttribute('src');
    expect(src).toBeTruthy();
    expect(await rendered.evaluate(image => (image as HTMLImageElement).naturalWidth), 'banner must decode').toBeGreaterThan(0);

    actor('admin');
    await panelSection(page, app, 'home-banners');
    const row = page.locator('.admin-banner-row').first();
    await expect(row).toBeVisible();
    await submitAndSettle(page, row.locator('form[action="/admin/banner/delete"]'), '/admin/banner/delete');
    await expect(page.locator('.admin-banner-row')).toHaveCount(0);
    actor('visitor');
    await page.goto(`${app.baseURL}/`);
    await expect(page.locator(`img[src="${src}"]`)).toHaveCount(0);
    await journeyStep(testInfo, page, 'home-banner-upload-delete');
  });

  test('banner ordering and the enabled toggle change which banners the public page shows', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('admin');
    await adminLogin(page, app);
    await panelSection(page, app, 'home-banners');
    await uploadHomeBanner(page, app, 1);
    await uploadHomeBanner(page, app, 2);

    const rowSrcs = () => page.locator('.admin-banner-row img')
      .evaluateAll(images => images.map(image => (image as HTMLImageElement).getAttribute('src')));
    const before = await rowSrcs();
    expect(before.length).toBe(2);

    await test.step('move the second banner up and verify the panel order changed', async () => {
      const last = page.locator('.admin-banner-row').nth(1);
      const moveUp = last.locator('form[action="/admin/banner/move"]')
        .filter({ has: page.getByRole('button', { name: 'up', exact: true }) }).first();
      await submitAndSettle(page, moveUp, '/admin/banner/move');
      expect(await rowSrcs()).toEqual([before[1], before[0]]);
    });

    await test.step('disable the first banner through its real meta form', async () => {
      const first = page.locator('.admin-banner-row').first();
      // Read the src of the banner actually being disabled, so the public-page
      // assertion below targets that banner rather than a stale ordering.
      const disabledSrc = await first.locator('img').getAttribute('src');
      expect(disabledSrc).toBeTruthy();
      const enabled = first.locator('input[name="enabled"]');
      await expect(enabled).toBeChecked();
      await enabled.uncheck();
      await submitAndSettle(page, first.locator('form[action="/admin/banner/update"]'), '/admin/banner/update');
      await expect(page.locator('.admin-banner-row').first().locator('input[name="enabled"]')).not.toBeChecked();

      actor('visitor');
      await page.goto(`${app.baseURL}/`);
      await expect(page.locator(`img[src="${disabledSrc}"]`)).toHaveCount(0);
      await expect(page.locator('img[src*="/banner/assets/"]')).toHaveCount(1);
    });
  });

  test('external banner links show the interstitial and Continue reaches the declared target', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('admin');
    await adminLogin(page, app);

    await test.step('enable external banner links through the real banner-settings form', async () => {
      await panelSection(page, app, 'board-banners');
      const toggle = page.locator('input[name="banner_external_links_enabled"]').first();
      await expect(toggle).toBeVisible();
      if (!(await toggle.isChecked())) await toggle.check();
      const form = page.locator('form[action="/admin/site/settings"]')
        .filter({ has: page.locator('input[name="banner_external_links_enabled"]') }).first();
      await submitAndSettle(page, form, '/admin/site/settings');
      await expectAdminPanel(page);
    });

    await test.step('point a banner at an absolute URL that resolves inside this fixture', async () => {
      await panelSection(page, app, 'home-banners');
      await uploadHomeBanner(page, app, 1);
      const row = page.locator('.admin-banner-row').first();
      await row.locator('select[name="target_type"]').selectOption({ label: 'Open another website' });
      await row.locator('input[name="target_external_url"]').fill(`${app.baseURL}/healthz`);
      await submitAndSettle(page, row.locator('form[action="/admin/banner/update"]'), '/admin/banner/update');
      await expect(page.locator('.admin-banner-row').first().locator('input[name="target_external_url"]'))
        .toHaveValue(`${app.baseURL}/healthz`);
    });

    await test.step('visitor gets the warning page before the redirect', async () => {
      actor('visitor');
      await page.goto(`${app.baseURL}/`);
      const link = page.locator('a[href*="/banner/external/"]').first();
      await expect(link).toBeVisible();
      await link.click();
      await expect(page).toHaveURL(/\/banner\/external\/\d+/);
      await expect(page.locator('.external-banner-warning')).toContainText(/external website/i);
      await Promise.all([
        page.waitForURL(`${app.baseURL}/healthz`),
        page.getByRole('link', { name: /continue/i }).click(),
      ]);
      await expect(page.locator('body')).toContainText('ok');
    });
  });

  test('word filter added and removed through the admin UI rewrites live posts', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    const board = uniqueShort('wfil', testInfo);
    app.createBoardCli({ short: board, name: 'Word Filter' });

    actor('admin');
    await adminLogin(page, app);
    await panelSection(page, app, 'word-filters');
    const addForm = page.locator('form[action="/admin/filter/add"]').first();
    await addForm.locator('input[name="pattern"]').fill('rutabaga');
    await addForm.locator('input[name="replacement"]').fill('turnip');
    await submitAndSettle(page, addForm, '/admin/filter/add');
    expect(sqliteQuery(app, "SELECT COUNT(*) FROM word_filters WHERE pattern = 'rutabaga';")).toBe('1');
    await expect(page.locator('#word-filters tbody tr').filter({ hasText: 'rutabaga' })).toHaveCount(1);

    actor('poster');
    await createThread(page, app, board, { subject: 'filtered subject', body: 'the rutabaga is here' });
    const op = page.locator('.post.op');
    await expect(op).toContainText('turnip');
    expect(await op.innerText(), 'the raw filtered word must not survive').not.toContain('rutabaga');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE body LIKE '%rutabaga%';`), 'stored body keeps the original text').toBe('1');

    actor('admin');
    await panelSection(page, app, 'word-filters');
    const row = page.locator('#word-filters tbody tr').filter({ hasText: 'rutabaga' }).first();
    await expect(row).toBeVisible();
    await submitAndSettle(page, row.locator('form[action="/admin/filter/remove"]'), '/admin/filter/remove');
    expect(sqliteQuery(app, "SELECT COUNT(*) FROM word_filters WHERE pattern = 'rutabaga';")).toBe('0');

    actor('poster');
    const threadId = Number(sqliteQuery(app, `SELECT id FROM threads WHERE subject = 'filtered subject' LIMIT 1;`));
    await createReply(page, app, board, threadId, `a second rutabaga arrives ${Date.now()}`);
    await expect(page.locator('.post').last()).toContainText('rutabaga');
    await expect(page.locator('.post').last()).not.toContainText('turnip');
  });

  test('admin ban add, appeal dismiss, and ban remove work through the rendered forms', async ({ page, browser, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('poster');
    await createThread(page, app, 'pub', { subject: 'ban target', body: 'ban target body' });
    const ipHash = sqliteQuery(app, "SELECT ip_hash FROM posts WHERE body = 'ban target body' LIMIT 1;");
    expect(ipHash).toMatch(/^[0-9a-f]{64}$/);
    // On loopback the application derives identity from the rustchan_visitor_id
    // cookie (src/handlers/board.rs identity_key), so the banned actor is the
    // poster's cookie identity, not "every loopback client". Reuse that identity
    // in a separate context instead of expecting a brand new context to be banned.
    const posterState = await page.context().storageState();

    await test.step('admin adds the ban through the real moderation form', async () => {
      actor('admin');
      await adminLogin(page, app);
      await panelSection(page, app, 'moderation');
      const addForm = page.locator('form[action="/admin/ban/add"]').first();
      await addForm.locator('input[name="ip_hash"]').fill(ipHash);
      await addForm.locator('input[name="reason"]').fill('deep audit ban');
      await addForm.locator('input[name="duration_hours"]').fill('1');
      await submitAndSettle(page, addForm, '/admin/ban/add');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM bans WHERE ip_hash = '${ipHash}' AND reason = 'deep audit ban';`)).toBe('1');
      await expect(page.locator('#moderation tbody tr').filter({ hasText: 'deep audit ban' })).toHaveCount(1);
    });

    await test.step('the banned identity is refused at posting time and can appeal', async () => {
      actor('banned');
      const bannedContext = await newAuditedContext(browser, { storageState: posterState });
      const bannedPage = await bannedContext.newPage();
      // The documented ban contract is enforcement on the posting path
      // (src/handlers/posting.rs checks is_banned and redirects to /banned);
      // reading boards stays allowed.
      await bannedPage.goto(`${app.baseURL}/pub`);
      await expect(bannedPage.locator('form[action="/pub"]')).toHaveCount(1);
      const form = await openBoardPostForm(bannedPage, 'pub');
      await form.locator('input[name="subject"]').fill('banned attempt subject');
      await form.locator('textarea[name="body"]').fill('banned attempt body');
      // The plain-form ban response is HTTP 403 whose body is the ban page
      // (src/error.rs maps AppError::BannedUser to templates::ban_page). The XHR
      // path instead returns X-Rustchan-Redirect: /banned?reason=...
      expectHttpError({
        method: 'POST',
        path: '/pub',
        status: 403,
        reason: 'banned identity is refused at posting time with the ban page',
      });
      await Promise.all([
        bannedPage.waitForResponse(response => response.request().method() === 'POST'
          && new URL(response.url()).pathname === '/pub'
          && response.status() === 403, { timeout: 30_000 }),
        form.getByRole('button', { name: /post thread/i }).click(),
      ]);
      await expect(bannedPage.locator('body')).toContainText(/you are banned/i);
      await expect(bannedPage.locator('body')).toContainText(/deep audit ban/i);
      expect(
        sqliteQuery(app, "SELECT COUNT(*) FROM posts WHERE body = 'banned attempt body';"),
        'a banned identity must not create a post',
      ).toBe('0');

      const appeal = bannedPage.locator('form[action="/appeal"]').first();
      await expect(appeal).toBeVisible();
      await appeal.locator('textarea[name="reason"]').fill('deep audit appeal from a banned visitor');
      await Promise.all([
        bannedPage.waitForLoadState('domcontentloaded'),
        appeal.getByRole('button', { name: /submit appeal/i }).click(),
      ]);
      await expect.poll(
        () => Number(sqliteQuery(app, "SELECT COUNT(*) FROM ban_appeals WHERE status = 'open';")),
        { timeout: 30_000, message: 'exactly one pending appeal row' },
      ).toBe(1);
      await bannedContext.close();
    });

    await test.step('admin dismisses the appeal and the ban remains', async () => {
      actor('admin');
      await panelSection(page, app, 'moderation');
      const appealRow = page.locator('#moderation tr').filter({ hasText: 'deep audit appeal' }).first();
      await expect(appealRow).toBeVisible();
      await submitAndSettle(page, appealRow.locator('form[action="/admin/appeal/dismiss"]'), '/admin/appeal/dismiss');
      expect(sqliteQuery(app, "SELECT COUNT(*) FROM ban_appeals WHERE status = 'open';")).toBe('0');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM bans WHERE ip_hash = '${ipHash}';`)).toBe('1');
    });

    await test.step('admin lifts the ban through the real form and posting is restored', async () => {
      await panelSection(page, app, 'moderation');
      const banRow = page.locator('#moderation tr').filter({ hasText: 'deep audit ban' }).first();
      await expect(banRow).toBeVisible();
      await submitAndSettle(page, banRow.locator('form[action="/admin/ban/remove"]'), '/admin/ban/remove');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM bans WHERE ip_hash = '${ipHash}';`)).toBe('0');
      actor('poster');
      await page.goto(`${app.baseURL}/pub`);
      await expect(page.locator('body')).not.toContainText(/you are banned/i);
      await expect(page.locator('form[action="/pub"]')).toHaveCount(1);
      const form = await openBoardPostForm(page, 'pub');
      await form.locator('input[name="subject"]').fill('post-ban subject');
      await form.locator('textarea[name="body"]').fill('post-ban body');
      await Promise.all([
        page.waitForURL(/thread\/\d+/),
        form.getByRole('button', { name: /post thread/i }).click(),
      ]);
      expect(
        sqliteQuery(app, "SELECT COUNT(*) FROM posts WHERE body = 'post-ban body';"),
        'lifting the ban restores posting for that identity',
      ).toBe('1');
    });
  });

  test('admin thread delete and post delete submit through the rendered controls', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('admin');
    const board = uniqueShort('adel', testInfo);
    app.createBoardCli({ short: board, name: 'Admin Delete' });
    const doomed = await createThread(page, app, board, { subject: 'doomed thread', body: 'doomed thread body' });
    const survivor = await createThread(page, app, board, { subject: 'survivor thread', body: 'survivor thread body' });
    await createReply(page, app, board, survivor, 'survivor reply');

    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/${board}/thread/${doomed}`);
    const deleteForm = page.locator('form[action="/admin/thread/delete"]').first();
    await openDetailsIn(page, 'body');
    await expect(deleteForm).toBeVisible();
    await confirmModal(page, async () => {
      await deleteForm.locator('button[type="submit"]').first().click();
    });
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE id = ${doomed};`)).toBe('0');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE id = ${survivor};`)).toBe('1');

    await page.goto(`${app.baseURL}/${board}/thread/${survivor}`);
    const replyId = Number((await page.locator('.post').last().getAttribute('id'))?.replace(/^p/, ''));
    await openDetailsIn(page, 'body');
    const deletePostForm = page.locator('form[action="/admin/post/delete"]')
      .filter({ has: page.locator(`input[name="post_id"][value="${replyId}"]`) }).first();
    await expect(deletePostForm).toBeVisible();
    await confirmModal(page, async () => {
      await deletePostForm.locator('button[type="submit"]').first().click();
    });
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE id = ${replyId};`)).toBe('0');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE id = ${survivor};`)).toBe('1');
    actor('visitor');
    await page.goto(`${app.baseURL}/${board}/catalog`);
    await expect(page.locator('body')).toContainText('survivor thread');
    await expect(page.locator('body')).not.toContainText('doomed thread');
  });

  test('extract-board downloads one board and board restore imports it into a different instance', async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    actor('admin-source');
    const source = await createStandaloneApp({
      admin: true,
      boards: [{ short: 'ext', name: 'Extract Source' }, { short: 'oth', name: 'Other Board' }],
    });
    const target = await createStandaloneApp({ admin: true, boards: [{ short: 'oth', name: 'Other Board' }] });
    try {
      await createThread(page, source, 'ext', { subject: 'extract me', body: 'extract me body' });
      await adminLogin(page, source);
      await panelSection(page, source, 'backups');

      await test.step('create a full backup through the real form and its progress modal', async () => {
        const createForm = page.locator('form[action="/admin/backup/create"]').first();
        await expect(createForm).toBeVisible();
        await createForm.locator('#full-backup-btn').click();
        // admin.js intercepts this form, shows #backup-modal and polls
        // /admin/backup/progress; the app's own done signal is showDoneButton().
        await expect(page.locator('#backup-done-actions')).toBeVisible({ timeout: 120_000 });
        await expect(page.locator('#backup-progress-text')).toContainText(/Backup saved to server/i);
        await page.locator('[data-action="close-backup-modal"]').click();
        await page.waitForLoadState('domcontentloaded');
      });
      await expectSafePage(page, { allowAdminInternals: true });
      // Closing the backup modal reloads the panel, so the saved-backup rows are
      // collapsed again; re-open the disclosures before reaching the extract form.
      await openDetailsIn(page, '.admin-panel');
      await expect(page.locator('#backups')).toBeVisible();

      const extractForm = page.locator('form[action="/admin/backup/extract-board"]').first();
      await expect(extractForm).toBeVisible();
      await extractForm.locator('select[name="board_short"]').selectOption('ext');
      const [download] = await Promise.all([
        page.waitForEvent('download'),
        extractForm.locator('button[name="action"][value="download"]').click(),
      ]);
      const archivePath = await download.path();
      expect(archivePath, 'downloaded board archive should be materialized').toBeTruthy();
      noteEvent('extract-board-download', `suggested filename ${download.suggestedFilename()}`);

      actor('admin-target');
      await adminLogin(page, target);
      await panelSection(page, target, 'backups');
      const restoreForm = page.locator('form[action="/admin/board/restore"]').first();
      await restoreForm.locator('input[type="file"][name="backup_file"]').setInputFiles(archivePath!);
      // admin.js intercepts this upload form; the upload itself is the subject,
      // so the POST response and any operator-facing error text are captured as
      // evidence rather than racing a URL that already matches /admin/panel.
      const restoreResponse = await submitWithConfirm(page, restoreForm, '/admin/board/restore');
      // admin.js treats 204 (no content) as success and then navigates to the
      // flash redirect; reading the body after that redirect is not possible.
      noteEvent('board-restore-response', `status=${restoreResponse.status()} location=${restoreResponse.headers().location ?? ''}`);
      expect([200, 204, 303], `board restore status ${restoreResponse.status()}`).toContain(restoreResponse.status());
      await expect.poll(
        () => Number(sqliteQuery(target, "SELECT COUNT(*) FROM boards WHERE short_name = 'ext';")),
        { timeout: 120_000, message: 'board restored onto the target instance' },
      ).toBe(1);
      expect(sqliteQuery(target, "SELECT COUNT(*) FROM boards WHERE short_name = 'oth';"), 'unrelated board untouched').toBe('1');
      // A restore briefly takes the instance through its maintenance gate, so the
      // first request after it can fail; poll the real condition instead of sleeping.
      await expect.poll(async () => {
        const response = await page.goto(`${target.baseURL}/ext`).catch(() => null);
        return response?.status() ?? 0;
      }, { timeout: 90_000, message: 'restored board page serves 200' }).toBe(200);
      await expect(page.locator('body')).toContainText('extract me');
      await journeyStep(testInfo, page, 'extract-and-restore-board');
    } finally {
      // Leave any polling admin page before disposing the standalone instances:
      // the panel's live-log and site-health XHRs would otherwise be refused
      // during teardown and be reported as browser errors.
      await page.goto('about:blank').catch(() => undefined);
      await source.dispose();
      await target.dispose();
    }
  });

  test('site-health jobs payload is admin-only and the dismiss control carries a CSRF token', async ({ page, browser, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin journey suite runs on Chromium');
    const anonymous = await newAuditedContext(browser);
    const anonymousResponse = await anonymous.request.get(`${app.baseURL}/admin/site-health/jobs`, { maxRedirects: 0 });
    expect([302, 303, 401, 403]).toContain(anonymousResponse.status());
    await anonymous.close();

    actor('admin');
    await adminLogin(page, app);
    await panelSection(page, app, 'site-health');
    const jobs = await page.request.get(`${app.baseURL}/admin/site-health/jobs`);
    expect(jobs.status()).toBe(200);
    const payload = await jobs.json() as Record<string, unknown>;
    expect(Object.keys(payload).length).toBeGreaterThan(0);
    noteEvent('site-health-payload', `keys ${JSON.stringify(Object.keys(payload))}`);

    const dismiss = page.locator('form[action="/admin/site-health/jobs/dismiss"]').first();
    await expect(dismiss).toBeVisible();
    await expect(dismiss.locator('input[name="_csrf"]')).toHaveValue(/.+/);
    // Producing a failed job requires fault injection the binary does not expose,
    // so submitting dismiss with nothing to dismiss is deliberately not asserted
    // here; the audit report records it as an unverified gap.
  });
});
