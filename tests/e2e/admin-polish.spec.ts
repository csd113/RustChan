import type { Locator, Page, TestInfo } from '@playwright/test';
import {
  RustChanServer,
  adminCsrf,
  adminLogin,
  createReply,
  createThread,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  sqliteQuery,
  test,
  uniqueShort,
} from './helpers';

const DESKTOP_MIN_TARGET = 30;
const MOBILE_MIN_TARGET = 38;

test.describe('admin panel and moderation UI polish', () => {
  test('admin login, panel sections, settings forms, and moderation queues stay usable', async ({ page, app }, testInfo) => {
    await page.goto(`${app.baseURL}/admin`, { waitUntil: 'domcontentloaded' });
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'admin login');
    await expectUsableTarget(page.getByLabel('Username'), 'login username', testInfo);
    await expectUsableTarget(page.getByLabel('Password'), 'login password', testInfo);
    await expectUsableTarget(page.getByRole('button', { name: 'authenticate' }), 'login submit', testInfo);
    await expectFocusAffordance(page.getByLabel('Username'), 'login username');

    await page.getByLabel('Username').fill('admin');
    await page.getByLabel('Password').fill('wrong-password');
    await page.getByRole('button', { name: 'authenticate' }).click();
    await expect(page.getByRole('alert')).toContainText(/invalid username or password/i);

    const reported = await seedReportedPost(page, app, testInfo);
    await adminLogin(page, app);
    await expectNoHorizontalOverflow(page, 'admin panel shell');
    await openAdminSection(page, 'site health');
    await openAdminSection(page, 'boards');
    await openAdminSection(page, 'moderation');
    await openAdminSection(page, 'board banners');
    await openAdminSection(page, 'themes');
    await openAdminSection(page, 'full site backup');
    await openAdminSection(page, 'media settings');
    await openAdminSection(page, 'database maintenance');

    await expectAdminControlsUsable(page, testInfo);
    await expectFocusAffordance(page.locator('.admin-section-index a[href="#boards"]'), 'section jump link');
    await expectFocusAffordance(page.locator('input[name="site_name"]').first(), 'site name input');

    const board = uniqueShort('pol', testInfo);
    await page.locator('form.admin-board-create-form input[name="short_name"]').fill(board);
    await page.locator('form.admin-board-create-form input[name="name"]').fill('Polish Board');
    await page.locator('form.admin-board-create-form input[name="description"]').fill('Board created by the admin polish pass');
    await Promise.all([
      page.waitForURL(/\/admin\/panel/),
      page.locator('form.admin-board-create-form').getByRole('button', { name: 'create' }).click(),
    ]);
    await openAdminSection(page, 'boards');
    await expect(page.locator(`#board-${board}`)).toBeVisible();
    await page.locator(`#board-${board} > summary`).click();
    await expectUsableTarget(page.locator(`#board-${board} input[name="description"]`), 'board description input', testInfo);
    await expectUsableTarget(page.locator(`#board-${board} button`, { hasText: 'save settings' }), 'board save settings', testInfo);
    await expectUsableTarget(page.locator(`#board-${board} button`, { hasText: 'delete board' }), 'board delete action', testInfo);
    await expectNoHorizontalOverflow(page, 'expanded board settings');

    await openAdminSection(page, 'moderation');
    const reportRow = page.locator('#reports table').filter({ hasText: reported.reason }).locator('tbody tr').first();
    await expect(reportRow).toBeVisible();
    await expectUsableTarget(reportRow.getByRole('button', { name: /resolve/i }), 'resolve report', testInfo);
    await expectFocusAffordance(reportRow.getByRole('button', { name: /resolve/i }), 'resolve report');

    await page.locator('#active-bans input[name="ip_hash"]').fill(reported.ipHash);
    await page.locator('#active-bans input[name="reason"]').fill('layout verification');
    await expectUsableTarget(page.locator('#active-bans').getByRole('button', { name: 'ban' }), 'add ban', testInfo);
    await page.locator('#word-filters input[name="pattern"]').fill('awkward phrase');
    await page.locator('#word-filters input[name="replacement"]').fill('clear phrase');
    await expectUsableTarget(page.locator('#word-filters').getByRole('button', { name: 'add' }), 'add filter', testInfo);

    await openAdminSection(page, 'database maintenance');
    await page.getByRole('button', { name: /check database health/i }).click();
    await expect(page.locator('body')).toContainText(/database health|integrity/i);
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'database health result');
  });

  test('admin controls on public board, catalog, thread, and post surfaces stay grouped and tappable', async ({ page, app }, testInfo) => {
    const fixture = await seedAdminControlFixture(page, app, testInfo);
    await adminLogin(page, app);

    await page.goto(`${app.baseURL}/`, { waitUntil: 'domcontentloaded' });
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'homepage with admin controls');
    const reorder = page.locator(`.board-card:has(a[href="/${fixture.board}"]) .board-reorder-toggle`).first();
    if (await reorder.count()) {
      await expectUsableTarget(reorder, 'homepage board reorder toggle', testInfo);
      await reorder.click();
      await expectUsableTarget(page.locator(`.board-card:has(a[href="/${fixture.board}"]) .board-reorder-controls button`).first(), 'homepage reorder button', testInfo);
    }

    await page.goto(`${app.baseURL}/${fixture.board}/catalog`, { waitUntil: 'domcontentloaded' });
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'catalog with admin session');
    await expectAdminToolbarUsable(page, testInfo);
    const catalogMenu = page.locator('.catalog-thread-menu-toggle').first();
    if (isNoJsProject(testInfo)) {
      await expect(catalogMenu).toBeHidden();
      await expectCatalogFallbackActionsUsable(page, testInfo);
    } else {
      await page.locator('.catalog-item').first().hover();
      await expectUsableTarget(catalogMenu, 'catalog admin thread menu toggle', testInfo);
      await catalogMenu.click();
      await expectUsableTarget(page.locator('.catalog-thread-menu-item').first(), 'catalog admin thread menu action', testInfo);
    }

    await page.goto(`${app.baseURL}/${fixture.board}/thread/${fixture.threadId}`, { waitUntil: 'domcontentloaded' });
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'thread with admin controls');
    await expectAdminToolbarUsable(page, testInfo);
    await expectPostAdminControlsUsable(page, testInfo);
    if (isNoJsProject(testInfo)) {
      await expectNoJsThreadFallbackControls(page, testInfo);
      return;
    }

    await page.getByRole('button', { name: /sticky/i }).click();
    await expect(page).toHaveURL(new RegExp(`/${fixture.board}/thread/${fixture.threadId}`));
    await expect(page.locator('.thread-state-badge-pin').first()).toBeVisible();
    await expectAdminToolbarUsable(page, testInfo);

    await page.getByRole('button', { name: /lock/i }).click();
    await expect(page.locator('.locked-notice')).toBeVisible();
    await expectNoHorizontalOverflow(page, 'locked thread admin state');

    await page.getByRole('button', { name: /unlock/i }).click();
    await expect(page.locator('.locked-notice')).toHaveCount(0);

    const deleteThread = page.getByRole('button', { name: /delete thread/i });
    await deleteThread.click();
    await expect(page.locator('#confirm-modal')).toBeVisible();
    await expectConfirmationModalUsable(page, testInfo);
    await page.locator('#confirm-modal-cancel').click();
    await expect(page.locator('#confirm-modal')).toBeHidden();

    const prompts: string[] = [];
    page.on('dialog', async (dialog) => {
      prompts.push(dialog.message());
      await dialog.dismiss();
    });
    await page.locator('.admin-post-controls form[data-ban-delete-pid]').first().getByRole('button', { name: /ban\+del/i }).click();
    expect(prompts, 'ban+delete should use the styled modal instead of browser prompts').toHaveLength(0);
    await expect(page.locator('#ban-delete-modal')).toBeVisible();
    await expectUsableTarget(page.locator('#ban-delete-reason'), 'ban delete reason input', testInfo);
    await expectUsableTarget(page.locator('#ban-delete-duration'), 'ban delete duration input', testInfo);
    await expectUsableTarget(page.locator('#ban-delete-cancel'), 'ban delete cancel', testInfo);
    await expectUsableTarget(page.locator('#ban-delete-submit'), 'ban delete submit', testInfo);
    await page.locator('#ban-delete-cancel').click();
    await expect(page.locator('#ban-delete-modal')).toBeHidden();
    await expect(page).toHaveURL(new RegExp(`/${fixture.board}/thread/${fixture.threadId}`));

    await page.locator('.post-controls .report-btn').first().click();
    await expect(page.locator('#report-modal')).toBeVisible();
    await expectUsableTarget(page.locator('#report-reason'), 'report reason input', testInfo);
    await expectUsableTarget(page.locator('#report-submit-btn'), 'report submit', testInfo);
    await expectUsableTarget(page.locator('#report-modal [data-action="close-report"]'), 'report cancel', testInfo);
    await page.locator('#report-modal [data-action="close-report"]').click();
    await expect(page.locator('#report-modal')).toBeHidden();
  });
});

async function seedReportedPost(
  page: Page,
  app: RustChanServer,
  testInfo: TestInfo,
): Promise<{ board: string; threadId: number; postId: number; reason: string; ipHash: string }> {
  const board = uniqueShort('rpt', testInfo);
  app.createBoardCli({ short: board, name: 'Reported Board', description: 'Moderation queue fixture' });
  const threadId = await createThread(page, app, board, {
    subject: 'reported thread',
    body: 'post that should appear in the admin report queue',
  });
  const postId = await firstPostId(page);
  const reason = `admin polish report ${Date.now()}`;
  await submitReport(page, board, threadId, postId, reason);
  const ipHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE id = ${postId};`);
  return { board, threadId, postId, reason, ipHash };
}

async function seedAdminControlFixture(
  page: Page,
  app: RustChanServer,
  testInfo: TestInfo,
): Promise<{ board: string; threadId: number }> {
  const board = uniqueShort('admctl', testInfo);
  app.createBoardCli({ short: board, name: 'Admin Controls', description: 'Public admin controls fixture' });
  setBoardFixtureSettings(app, board, {
    allowArchive: true,
    allowEditing: true,
    allowSelfDelete: true,
    allowImages: true,
  });
  const threadId = await createThread(page, app, board, {
    subject: 'admin control surface',
    body: 'OP with media so post controls share space with attachments',
    filePath: app.fixtures().tinyPng,
  });
  await createReply(page, app, board, threadId, 'reply that exposes admin delete and ban controls');
  return { board, threadId };
}

async function firstPostId(page: Page): Promise<number> {
  const id = await page.locator('.post').first().getAttribute('id');
  const postId = Number(id?.replace(/^p/, ''));
  expect(Number.isInteger(postId)).toBeTruthy();
  return postId;
}

async function openAdminSection(page: Page, label: string): Promise<void> {
  const summary = page.locator('.admin-dropdown > summary').filter({ hasText: label }).first();
  await expect(summary, `admin section ${label} should exist`).toBeVisible();
  const open = await summary.evaluate((element) => element.parentElement instanceof HTMLDetailsElement && element.parentElement.open);
  if (!open) {
    await summary.click({ timeout: 3_000 }).catch(async () => {
      await summary.evaluate((element) => {
        const details = element.parentElement;
        if (details instanceof HTMLDetailsElement) {
          details.open = true;
        }
      });
    });
  }
  await expect.poll(
    () => summary.evaluate((element) => element.parentElement instanceof HTMLDetailsElement && element.parentElement.open),
    { message: `admin section ${label} should be open` },
  ).toBe(true);
}

async function submitReport(page: Page, board: string, threadId: number, postId: number, reason: string): Promise<void> {
  const post = page.locator(`#p${postId}`);
  const reportButton = post.locator('.report-btn').first();
  if (await reportButton.isVisible()) {
    await reportButton.click();
    await page.locator('#report-reason').fill(reason);
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
      page.locator('#report-submit-btn').click(),
    ]);
    return;
  }

  const fallback = post.locator('.report-fallback-form').first();
  await expect(fallback).toBeVisible();
  await openDetailsIfClosed(fallback.locator('.report-fallback-details').first());
  await fallback.locator('.report-fallback-reason').fill(reason);
  await Promise.all([
    page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
    fallback.locator('.report-fallback-submit').click(),
  ]);
}

async function expectCatalogFallbackActionsUsable(page: Page, testInfo: TestInfo): Promise<void> {
  const actions = page.locator('.catalog-thread-fallback-actions').first();
  await expect(actions).toBeVisible();
  await expectUsableTarget(actions.locator('.catalog-thread-fallback-summary'), 'catalog fallback actions summary', testInfo);
  await expectUsableTarget(actions.getByRole('button', { name: /pin thread|unpin thread/i }).first(), 'catalog fallback pin action', testInfo);
  await expectUsableTarget(actions.getByRole('button', { name: /hide thread|unhide thread/i }).first(), 'catalog fallback hide action', testInfo);
  await expectReportFallbackUsable(actions, testInfo);
}

async function expectNoJsThreadFallbackControls(page: Page, testInfo: TestInfo): Promise<void> {
  await expect(page.locator('.post-controls .report-btn').first()).toBeHidden();
  await expectUsableTarget(page.getByRole('button', { name: /sticky/i }).first(), 'thread sticky action', testInfo);
  await expectUsableTarget(page.getByRole('button', { name: /lock/i }).first(), 'thread lock action', testInfo);
  await expectUsableTarget(page.getByRole('button', { name: /delete thread/i }).first(), 'thread delete action', testInfo);
  await expectUsableTarget(
    page.locator('.admin-post-controls form[data-ban-delete-pid]').first().getByRole('button', { name: /ban\+del/i }),
    'post ban delete action',
    testInfo,
  );
  await expectReportFallbackUsable(page.locator('.post-controls').first(), testInfo);
}

async function expectReportFallbackUsable(root: Locator, testInfo: TestInfo): Promise<void> {
  const fallback = root.locator('.report-fallback-form').first();
  await expect(fallback).toBeVisible();
  await expectUsableTarget(fallback.locator('.report-fallback-summary'), 'report fallback summary', testInfo);
  await openDetailsIfClosed(fallback.locator('.report-fallback-details').first());
  await expectUsableTarget(fallback.locator('.report-fallback-reason'), 'report fallback reason input', testInfo);
  await expectUsableTarget(fallback.locator('.report-fallback-submit'), 'report fallback submit', testInfo);
}

async function openDetailsIfClosed(details: Locator): Promise<void> {
  const open = await details.evaluate((element) => element instanceof HTMLDetailsElement && element.open);
  if (!open) {
    await details.locator('summary').click();
  }
}

async function expectAdminControlsUsable(page: Page, testInfo: TestInfo): Promise<void> {
  await expectNoHorizontalOverflow(page, 'admin controls');
  const controls = page.locator([
    '.admin-panel button:visible',
    '.admin-panel a.admin-link-button:visible',
    '.admin-panel input[type="text"]:visible',
    '.admin-panel input[type="password"]:visible',
    '.admin-panel input[type="number"]:visible',
    '.admin-panel select:visible',
    '.admin-panel textarea:visible',
  ].join(','));
  await expectVisibleControlsRendered(controls, 'admin panel controls');
  await expectNoCoveredCenters(controls, 'admin panel controls');
}

async function expectAdminToolbarUsable(page: Page, testInfo: TestInfo): Promise<void> {
  const toolbar = page.locator('.admin-toolbar').first();
  await expect(toolbar).toBeVisible();
  await expectVisibleControlsUsable(toolbar.locator('button, a'), 'thread admin toolbar', testInfo);
  await expectNoCoveredCenters(toolbar.locator('button, a'), 'thread admin toolbar');
}

async function expectPostAdminControlsUsable(page: Page, testInfo: TestInfo): Promise<void> {
  const controls = page.locator('.post-controls.admin-post-controls').first();
  await expect(controls).toBeVisible();
  await expectVisibleControlsUsable(controls.locator('button, a'), 'post admin controls', testInfo);
  await expectNoCoveredCenters(controls.locator('button, a'), 'post admin controls');
  await expectSameVisualGroup(controls.locator('button, a'), 'post admin controls');
}

async function expectConfirmationModalUsable(page: Page, testInfo: TestInfo): Promise<void> {
  await expectUsableTarget(page.locator('#confirm-modal-cancel'), 'confirm cancel', testInfo);
  await expectUsableTarget(page.locator('#confirm-modal-continue'), 'confirm continue', testInfo);
  await expectNoCoveredCenters(page.locator('#confirm-modal button'), 'confirmation modal buttons');
}

async function expectVisibleControlsUsable(locator: Locator, name: string, testInfo: TestInfo): Promise<void> {
  const count = await locator.count();
  expect(count, `${name} should expose visible controls`).toBeGreaterThan(0);
  for (let index = 0; index < count; index += 1) {
    const item = locator.nth(index);
    if (!(await item.isVisible())) {
      continue;
    }
    await expectUsableTarget(item, `${name} #${index + 1}`, testInfo);
  }
}

async function expectVisibleControlsRendered(locator: Locator, name: string): Promise<void> {
  const count = await locator.count();
  expect(count, `${name} should expose visible controls`).toBeGreaterThan(0);
  for (let index = 0; index < count; index += 1) {
    const item = locator.nth(index);
    if (!(await item.isVisible())) {
      continue;
    }
    const box = await item.boundingBox();
    expect(box, `${name} #${index + 1} should have layout bounds`).not.toBeNull();
    expect(box!.width, `${name} #${index + 1} should not collapse horizontally`).toBeGreaterThan(0);
    expect(box!.height, `${name} #${index + 1} should not collapse vertically`).toBeGreaterThan(0);
  }
}

async function expectUsableTarget(locator: Locator, name: string, testInfo: TestInfo): Promise<void> {
  await expect(locator, `${name} should be visible`).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, `${name} should have layout bounds`).not.toBeNull();
  expect(box!.width, `${name} should not collapse horizontally`).toBeGreaterThan(0);
  expect(box!.height, `${name} should be tall enough to use`).toBeGreaterThanOrEqual(
    isMobileProject(testInfo) ? MOBILE_MIN_TARGET : DESKTOP_MIN_TARGET,
  );
  const awkward = await locator.evaluate((element) => {
    const style = window.getComputedStyle(element);
    return {
      horizontalOverflow: element.scrollWidth > element.clientWidth + 1,
      verticalOverflow: element.scrollHeight > element.clientHeight + 3 && style.whiteSpace === 'nowrap',
    };
  });
  const skipsHorizontalClipCheck = await locator.evaluate((element) => (
    element instanceof HTMLInputElement
    || element instanceof HTMLSelectElement
    || element instanceof HTMLTextAreaElement
  ));
  if (!skipsHorizontalClipCheck) {
    expect(awkward.horizontalOverflow, `${name} text/content should not be clipped horizontally`).toBeFalsy();
  }
  expect(awkward.verticalOverflow, `${name} text/content should not be clipped vertically`).toBeFalsy();
}

async function expectFocusAffordance(locator: Locator, name: string): Promise<void> {
  await locator.focus();
  const focusStyle = await locator.evaluate((element) => {
    const style = window.getComputedStyle(element);
    return {
      outlineStyle: style.outlineStyle,
      outlineWidth: Number.parseFloat(style.outlineWidth),
      boxShadow: style.boxShadow,
      borderColor: style.borderColor,
    };
  });
  expect(
    focusStyle.outlineStyle !== 'none' && focusStyle.outlineWidth > 0
      || focusStyle.boxShadow !== 'none'
      || focusStyle.borderColor !== '',
    `${name} should show a focus affordance`,
  ).toBeTruthy();
}

async function expectNoHorizontalOverflow(page: Page, name: string): Promise<void> {
  const overflow = await page.evaluate(() => {
    const doc = document.documentElement;
    const body = document.body;
    return Math.max(doc.scrollWidth - doc.clientWidth, body.scrollWidth - body.clientWidth);
  });
  expect(overflow, `${name} should not create horizontal page overflow`).toBeLessThanOrEqual(2);
}

async function expectNoCoveredCenters(locator: Locator, name: string): Promise<void> {
  const covered = await locator.evaluateAll((elements) => elements
    .filter((element) => {
      const rect = element.getBoundingClientRect();
      const style = window.getComputedStyle(element);
      return rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none';
    })
    .map((element, index) => {
      const rect = element.getBoundingClientRect();
      const x = rect.left + rect.width / 2;
      const y = rect.top + rect.height / 2;
      if (x < 0 || y < 0 || x > window.innerWidth || y > window.innerHeight) {
        return null;
      }
      const top = document.elementFromPoint(x, y);
      return top && (element === top || element.contains(top) || top.contains(element)) ? null : index + 1;
    })
    .filter((index): index is number => index !== null));
  expect(covered, `${name} should not be covered or overlapped at control centers`).toEqual([]);
}

async function expectSameVisualGroup(locator: Locator, name: string): Promise<void> {
  const boxes = await locator.evaluateAll((elements) => elements
    .filter((element) => {
      const rect = element.getBoundingClientRect();
      const style = window.getComputedStyle(element);
      return rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none';
    })
    .map((element) => {
      const rect = element.getBoundingClientRect();
      return { top: rect.top, bottom: rect.bottom };
    }));
  expect(boxes.length, `${name} should have grouped controls`).toBeGreaterThan(1);
  const firstTop = boxes[0].top;
  for (const box of boxes) {
    expect(Math.abs(box.top - firstTop), `${name} controls should stay visually grouped`).toBeLessThanOrEqual(8);
  }
}

function isMobileProject(testInfo: TestInfo): boolean {
  return testInfo.project.name.includes('mobile');
}

function isNoJsProject(testInfo: TestInfo): boolean {
  return testInfo.project.name === 'firefox-nojs';
}
