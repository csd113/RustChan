// Functional journey audit: complete user journeys and state transitions.
//
// Project applicability
// ---------------------
// Every journey in this file runs on the `chromium` desktop project because it
// asserts product behaviour and record state, not engine behaviour. Layout,
// progressive-enhancement, and engine differences are already covered by the
// phase/audit suites (`audit-surfaces`, `phase4-*`, `firefox-nojs-*`,
// `mobile-*`). The single exception is the JavaScript-disabled poll creation
// journey, which runs in the `chromium-nojs` and `firefox-nojs` projects because
// the `noscript` option rows are the point of that test. Each test states its own
// gate with `test.skip(...)` and a rationale.
//
// Harness rules honoured here
// ---------------------------
// * Locators favour roles, labels, and the app's documented stable hooks.
// * No request mocking, no `waitForTimeout`, no retries, no forced clicks.
// * Every intentional 4xx is declared with `expectHttpError` before the action.
// * Browser-visible outcomes are primary; SQLite reads corroborate them.

import {
  adminLogin,
  boardId,
  createReply,
  createThread,
  createThreadViaRequest,
  expect,
  expectNoDialog,
  expectSafeResponse,
  publicCsrf,
  setBoardFixtureSettings,
  setThreadFixtureState,
  sqliteQuery,
  test,
  threadIdFromUrl,
  uniqueShort,
  type RustChanServer,
} from './helpers';
import {
  actor,
  expectHttpError,
  measureStep,
  newAuditedContext,
  type Page,
  type TestInfo,
} from './diagnostics';

// ---------------------------------------------------------------------------
// Local helpers
// ---------------------------------------------------------------------------

/**
 * Collection-time annotation marking a bounded-screenshot deep journey so the
 * audit runner's deep mode can treat it as a success-journey artifact even
 * before the test body starts.
 */
const JOURNEY_DETAILS = {
  annotation: {
    type: 'deep-journey',
    description: 'multi-actor product journey with browser and database evidence',
  },
} as const;

/** Per-test board short name; `variant` yields a second distinct board. */
function boardShort(testInfo: TestInfo, variant = ''): string {
  return uniqueShort(`jrny${variant}`, testInfo);
}

/**
 * Navigates and asserts the exact document status.
 *
 * Chromium reports no response when the target URL is the one the document
 * already shows (same-document no-op). That case is not a silent skip: the
 * live location must match the requested URL exactly, and the earlier load of
 * that URL asserted its own status.
 */
async function gotoOk(page: Page, url: string, expectedStatus = 200): Promise<void> {
  const response = await page.goto(url, { waitUntil: 'domcontentloaded' });
  if (response === null) {
    expect(page.url(), `${url} should already be the current document`).toBe(new URL(url).href);
    return;
  }
  expect(response.status(), `${url} should answer ${expectedStatus}`).toBe(expectedStatus);
}

/** Opens the collapsed thread/reply composer and returns its form. */
async function openComposer(page: Page, formSelector: string) {
  const toggle = page.locator('[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  const form = page.locator(formSelector).first();
  await expect(form).toBeVisible();
  return form;
}

/** Numeric scalar query; also asserts the value is a finite number. */
function scalar(app: RustChanServer, sql: string): number {
  const raw = sqliteQuery(app, sql);
  const value = Number(raw);
  expect(Number.isFinite(value), `expected a numeric query result, got ${JSON.stringify(raw)}`).toBe(true);
  return value;
}

/** Description of one board row used by several visibility assertions. */
function boardRow(app: RustChanServer, short: string): string {
  return sqliteQuery(
    app,
    `SELECT name || '|' || description || '|' || nsfw || '|' || allow_images FROM boards WHERE short_name = '${short}';`,
  );
}

// ---------------------------------------------------------------------------
// 1. Multi-actor thread lifecycle with media on a fresh image board
// ---------------------------------------------------------------------------

test('an owner uploads an image, an unrelated reader sees it and replies, and owner edit and delete round-trip for both actors', JOURNEY_DETAILS, async ({ page, app, browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium; engine/layout coverage lives in the phase suites.');
  const short = boardShort(testInfo);
  const subject = `image thread ${short}`;
  const initialBody = `initial op body ${short}`;
  const editedBody = `edited op body ${short} café漢字`;
  const readerBody = `reader reply ${short}`;
  const ownerReplyBody = `owner reply ${short}`;
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}` });

  let threadId = 0;
  let opId = 0;
  let filePath = '';
  let thumbPath = '';

  await measureStep('owner creates the image thread', async () => {
    actor('owner');
    threadId = await createThread(page, app, short, {
      subject,
      body: initialBody,
      filePath: app.fixtures().tinyPng,
    });
    expect(threadId).toBeGreaterThan(0);
    opId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 1;`);
    filePath = sqliteQuery(app, `SELECT file_path FROM posts WHERE id = ${opId};`);
    thumbPath = sqliteQuery(app, `SELECT thumb_path FROM posts WHERE id = ${opId};`);
    expect(opId).toBeGreaterThan(0);
    // The record, not just the rendered page, carries the upload and the body.
    expect(sqliteQuery(app, `SELECT body FROM posts WHERE id = ${opId};`)).toBe(initialBody);
    expect(filePath).toMatch(new RegExp(`^${short}/.+\\.(png|webp|jpg|jpeg)$`));
    expect(thumbPath).toMatch(new RegExp(`^${short}/.+`));
    expect(
      scalar(app, `SELECT COUNT(*) FROM file_hashes WHERE file_path = '${filePath}' AND thumb_path = '${thumbPath}';`),
      'the media row should be registered for the uploaded file',
    ).toBe(1);

    const op = page.locator(`#p${opId}`);
    await expect(op.locator('.subject')).toContainText(subject);
    await expect(op.locator('.post-body')).toContainText(initialBody);
    await expect(op.locator('.file-info a').first()).toHaveAttribute('href', `/boards/${filePath}`);
    const thumb = op.locator('img.thumb').first();
    await expect(thumb).toHaveAttribute('src', `/boards/${thumbPath}`);
    await expect.poll(
      () => thumb.evaluate((img: HTMLImageElement) => img.naturalWidth),
      { message: 'the OP thumbnail bytes should load in the browser' },
    ).toBeGreaterThan(0);
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('0');
  });

  const readerContext = await newAuditedContext(browser);
  const readerPage = await readerContext.newPage();

  await measureStep('an unrelated reader sees the thread, image, and catalog card', async () => {
    actor('unrelated');
    await gotoOk(readerPage, `${app.baseURL}/${short}`);
    const boardOp = readerPage.locator(`#p${opId}`);
    await expect(boardOp.locator('.subject')).toContainText(subject);
    const boardThumb = boardOp.locator('img.thumb').first();
    await expect(boardThumb).toHaveAttribute('src', `/boards/${thumbPath}`);
    await boardThumb.scrollIntoViewIfNeeded();
    await expect.poll(
      () => boardThumb.evaluate((img: HTMLImageElement) => img.naturalWidth),
      { message: 'the board-index thumbnail bytes should load' },
    ).toBeGreaterThan(0);

    await gotoOk(readerPage, `${app.baseURL}/${short}/catalog`);
    const catalogItem = readerPage.locator('.catalog-item').filter({ hasText: subject });
    await expect(catalogItem).toHaveCount(1);
    await expect(catalogItem.locator('img.catalog-thumb').first()).toBeVisible();

    await gotoOk(readerPage, `${app.baseURL}/${short}/thread/${threadId}`);
    await expect(readerPage.locator(`#p${opId} .post-body`)).toContainText(initialBody);
    // Owner-only controls never render for a visitor without an ownership grant.
    await expect(readerPage.locator('.self-action-controls')).toHaveCount(0);
  });

  await measureStep('the unrelated reader replies and both views advance by one', async () => {
    actor('unrelated');
    await createReply(readerPage, app, short, threadId, readerBody);
    actor('owner');
    await gotoOk(page, `${app.baseURL}/${short}/thread/${threadId}`);
    await expect(page.locator('.post').filter({ hasText: readerBody })).toHaveCount(1);
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
    expect(scalar(app, `SELECT reply_count FROM threads WHERE id = ${threadId};`)).toBe(1);
  });

  await measureStep('the owner edits the opening post through the real modal', async () => {
    actor('owner');
    const op = page.locator(`#p${opId}`);
    const editLink = op.locator('.self-action-controls a.edit-btn');
    await expect(editLink).toBeVisible();
    await editLink.click();
    await expect(page.getByRole('dialog', { name: /edit your post/i })).toBeVisible();
    const modal = page.locator('#edit-modal');
    await expect(modal).toHaveClass(/is-open/);
    const editor = modal.getByLabel('edit post body');
    await expect(editor).toHaveValue(initialBody);
    await editor.fill(editedBody);
    const [editResponse] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${short}/post/${opId}/edit`),
      modal.getByRole('button', { name: 'save edit' }).click(),
    ]);
    expect(editResponse.status()).toBe(204);
    expect(editResponse.headers()['x-rustchan-redirect']).toBe(`/${short}/thread/${threadId}#p${opId}`);
    // The modal handler only navigates on success, so a visible edited body on
    // this document proves the post-edit navigation replaced the page.
    await expect(page.locator(`#p${opId} .post-body`)).toHaveText(editedBody);
    expect(sqliteQuery(app, `SELECT body FROM posts WHERE id = ${opId};`)).toBe(editedBody);
    expect(scalar(app, `SELECT COUNT(*) FROM posts WHERE id = ${opId} AND edited_at IS NOT NULL;`)).toBe(1);
  });

  await measureStep('the edit is visible after reload and to the other actor', async () => {
    actor('owner');
    await page.reload();
    await expect(page.locator(`#p${opId} .post-body`)).toHaveText(editedBody);
    actor('unrelated');
    await gotoOk(readerPage, `${app.baseURL}/${short}/thread/${threadId}`);
    await expect(readerPage.locator(`#p${opId} .post-body`)).toHaveText(editedBody);
    await expect(readerPage.locator('.post').filter({ hasText: editedBody })).toHaveCount(1);
  });

  await measureStep('the owner deletes their own reply and the media survives', async () => {
    actor('owner');
    await createReply(page, app, short, threadId, ownerReplyBody);
    const ownerReplyId = scalar(
      app,
      `SELECT id FROM posts WHERE thread_id = ${threadId} AND body = '${ownerReplyBody}';`,
    );
    expect(ownerReplyId).toBeGreaterThan(0);
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('2');
    expect(scalar(app, `SELECT reply_count FROM threads WHERE id = ${threadId};`)).toBe(2);

    const deleteLink = page.locator(`#p${ownerReplyId} .self-action-controls a.del-btn`);
    await expect(deleteLink).toBeVisible();
    await expectNoDialog(page, async () => {
      await deleteLink.click();
      await expect(page.locator('#confirm-modal-message')).toHaveText(`Delete your post No.${ownerReplyId}?`);
      await page.locator('#confirm-modal-continue').click();
    });

    actor('owner');
    await expect(page.locator(`#p${ownerReplyId}`)).toHaveCount(0);
    await expect(page.locator('.post').filter({ hasText: readerBody })).toHaveCount(1);
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
    expect(scalar(app, `SELECT reply_count FROM threads WHERE id = ${threadId};`)).toBe(1);
    expect(scalar(app, `SELECT COUNT(*) FROM posts WHERE id = ${ownerReplyId};`)).toBe(0);

    actor('unrelated');
    await gotoOk(readerPage, `${app.baseURL}/${short}/thread/${threadId}`);
    await expect(readerPage.locator(`#p${ownerReplyId}`)).toHaveCount(0);
    await expect(readerPage.locator('.post').filter({ hasText: readerBody })).toHaveCount(1);

    // The OP media is untouched by the reply deletion.
    expect(sqliteQuery(app, `SELECT file_path FROM posts WHERE id = ${opId};`)).toBe(filePath);
    const mediaResponse = await page.request.get(`${app.baseURL}/boards/${filePath}`);
    expect(mediaResponse.status()).toBe(200);
    expect(
      scalar(app, `SELECT COUNT(*) FROM file_hashes WHERE file_path = '${filePath}';`),
      'the OP media row should still exist after the reply was deleted',
    ).toBe(1);
  });

  await readerContext.close();
});

// ---------------------------------------------------------------------------
// 2. Own-post edit/delete denied when the board disables them
// ---------------------------------------------------------------------------

test('a board with editing and self-delete disabled hides the controls and denies direct edit and delete requests', JOURNEY_DETAILS, async ({ page, app }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  const subject = `locked controls ${short}`;
  const body = `locked controls body ${short}`;
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}` });
  setBoardFixtureSettings(app, short, { allowEditing: false, allowSelfDelete: false });

  let threadId = 0;
  let opId = 0;

  await measureStep('the owner posts while controls are disabled', async () => {
    actor('owner');
    threadId = await createThread(page, app, short, { subject, body });
    opId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 1;`);
    await expect(page.locator(`#p${opId} .subject`)).toContainText(subject);
    // No owner controls at all: neither edit nor delete is offered.
    await expect(page.locator('.self-action-controls')).toHaveCount(0);
    await expect(page.locator(`#p${opId} .del-btn`)).toHaveCount(0);
  });

  await measureStep('a direct edit page request is forbidden', async () => {
    actor('owner');
    const editPath = `/${short}/post/${opId}/edit`;
    expectHttpError({
      method: 'GET',
      path: editPath,
      status: 403,
      reason: 'the board disables user editing, so the edit page must be refused',
    });
    const denied = await page.goto(`${app.baseURL}${editPath}`, { waitUntil: 'domcontentloaded' });
    expect(denied, 'the denied edit request should still produce a document').not.toBeNull();
    expect(denied!.status()).toBe(403);
    await expect(page.locator('.error-page')).toContainText('cannot edit their own posts');
  });

  await measureStep('a direct delete POST is forbidden and the record is unchanged', async () => {
    actor('owner');
    const csrf = await publicCsrf(page, app, `/${short}/thread/${threadId}`);
    const denied = await page.request.post(`${app.baseURL}/${short}/post/${opId}/delete`, {
      form: { _csrf: csrf },
      maxRedirects: 0,
    });
    // page.request responses are not part of the browser diagnostics stream, so
    // the exact status must be asserted here.
    expect(denied.status()).toBe(403);
    const deniedBody = await expectSafeResponse(denied);
    expect(deniedBody).toContain('cannot delete their own posts');

    expect(sqliteQuery(app, `SELECT body FROM posts WHERE id = ${opId};`)).toBe(body);
    expect(scalar(app, `SELECT COUNT(*) FROM posts WHERE id = ${opId};`)).toBe(1);
    expect(scalar(app, `SELECT reply_count FROM threads WHERE id = ${threadId};`)).toBe(0);
    expect(scalar(app, `SELECT COUNT(*) FROM threads WHERE id = ${threadId} AND locked = 0;`)).toBe(1);

    await gotoOk(page, `${app.baseURL}/${short}/thread/${threadId}`);
    await expect(page.locator(`#p${opId} .post-body`)).toHaveText(body);
    await expect(page.locator('.post')).toHaveCount(1);
  });
});

// ---------------------------------------------------------------------------
// 3. Preferences and theme persistence (cookie-scoped contract)
// ---------------------------------------------------------------------------

test('theme and preference changes persist across navigation, reload, and restart, but do not leak into a fresh visitor', JOURNEY_DETAILS, async ({ page, app, browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');

  const otherContext = await newAuditedContext(browser);
  const otherPage = await otherContext.newPage();

  let initialTheme = '';
  let targetTheme = 'blue-sky';

  await measureStep('baseline: a fresh visitor uses the site default with NSFW boards visible', async () => {
    actor('unrelated');
    await gotoOk(otherPage, app.baseURL);
    initialTheme = (await otherPage.locator('html').getAttribute('data-active-theme')) ?? '';
    expect(initialTheme, 'the site default theme should be exposed on the html element').not.toBe('');
    targetTheme = initialTheme === 'blue-sky' ? 'deep-orbit' : 'blue-sky';
    await expect(otherPage.locator('.index-section[data-board-nsfw="1"]')).toHaveCount(1);
    await expect(otherPage.locator('.index-section[data-board-nsfw="1"] .board-card')).toHaveCount(1);
  });

  await measureStep('the visitor changes the theme and hides NSFW boards through the real preferences form', async () => {
    actor('visitor');
    await gotoOk(page, app.baseURL);
    await page.locator('#theme-picker-btn').click();
    const form = page.locator('.user-preferences-form').first();
    await expect(form).toBeVisible();
    const select = form.locator('select[name="theme"]');
    await expect(select.locator(`option[value="${targetTheme}"]`)).toHaveCount(1);
    const [themeSave] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/preferences'),
      select.selectOption(targetTheme),
    ]);
    expect(themeSave.status()).toBe(204);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', targetTheme);
    await expect(page.locator('.user-preferences-status')).toHaveText('Saved.');
    const [hideSave] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/preferences'),
      form.locator('input[name="hide_nsfw_boards"]').check(),
    ]);
    expect(hideSave.status()).toBe(204);
    await expect(page.locator('.user-preferences-status')).toHaveText('Saved.');
    // Hiding NSFW is applied immediately (CSS) and again server-side on reload.
    await expect(page.locator('.index-section[data-board-nsfw="1"]')).toBeHidden();

    const cookies = await page.context().cookies();
    expect(cookies.find((cookie) => cookie.name === 'rustchan_theme')?.value).toBe(targetTheme);
    expect(cookies.find((cookie) => cookie.name === 'rustchan_hide_nsfw')?.value).toBe('1');
  });

  await measureStep('navigation, reload, and a server restart keep the visitor preferences', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/pub`);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', targetTheme);
    await page.reload();
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', targetTheme);

    await app.restart();
    await gotoOk(page, app.baseURL);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', targetTheme);
    await expect(page.locator('.index-section[data-board-nsfw="1"]')).toHaveCount(0);
    const cookies = await page.context().cookies();
    expect(cookies.find((cookie) => cookie.name === 'rustchan_theme')?.value).toBe(targetTheme);
  });

  await measureStep('an unrelated visitor is unaffected by the other visitor preferences', async () => {
    actor('unrelated');
    await gotoOk(otherPage, app.baseURL);
    await expect(otherPage.locator('html')).toHaveAttribute('data-active-theme', initialTheme);
    await expect(otherPage.locator('.index-section[data-board-nsfw="1"]')).toHaveCount(1);
    await expect(otherPage.locator('.index-section[data-board-nsfw="1"] .board-card')).toHaveCount(1);
    const cookies = await otherContext.cookies();
    expect(cookies.find((cookie) => cookie.name === 'rustchan_theme')).toBeUndefined();
    expect(cookies.find((cookie) => cookie.name === 'rustchan_hide_nsfw')).toBeUndefined();
  });

  await otherContext.close();
});

// ---------------------------------------------------------------------------
// 4. Board settings visibility through the admin panel UI
// ---------------------------------------------------------------------------

test('admin board edits show up on the board, catalog, homepage, and a fresh visitor, and the upload toggle gates the form', JOURNEY_DETAILS, async ({ page, app, browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  const control = boardShort(testInfo, 'c');
  const initialName = `Before ${short}`;
  const updatedName = `After ${short}`;
  const initialDescription = `before description ${short}`;
  const updatedDescription = `after description ${short}`;
  app.createBoardCli({ short, name: initialName, description: initialDescription, noVideos: true });
  app.createBoardCli({ short: control, name: `Control ${control}`, description: `control description ${control}`, noVideos: true });

  const adminContext = await newAuditedContext(browser);
  const adminPage = await adminContext.newPage();

  await measureStep('baseline: the board accepts image uploads and shows the initial identity', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}`);
    await expect(page.locator('.board-index-header h1')).toContainText(initialName);
    await expect(page.locator('.board-desc')).toHaveText(initialDescription);
    await expect(page.locator('#post-form-wrap input[type="file"]')).toHaveCount(1);
    expect(boardRow(app, short)).toBe(`${initialName}|${initialDescription}|0|1`);
  });

  await measureStep('admin edits name, description, NSFW state, and the image toggle in the panel form', async () => {
    actor('admin');
    await adminLogin(adminPage, app);
    await gotoOk(adminPage, `${app.baseURL}/admin/panel?open=board-${short}#board-${short}`);
    const card = adminPage.locator(`#board-${short}`);
    await expect(card).toBeVisible();
    await expect(card).toHaveAttribute('open', '');
    await card.locator('input[name="name"]').fill(updatedName);
    await card.locator('input[name="description"]').fill(updatedDescription);
    await card.locator('input[name="nsfw"]').check();
    await card.locator('input[name="allow_images"]').uncheck();
    const [saveResponse] = await Promise.all([
      adminPage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST'
        && new URL(candidate.url()).pathname === '/admin/board/settings'),
      card.locator('form.board-settings-form button[type="submit"]').click(),
    ]);
    expect(saveResponse.status()).toBe(303);
    // The panel also renders a deliberate theme-preview flash pair, so the save
    // feedback is pinned to its own message.
    const flash = adminPage.locator('.admin-flash.flash-ok').filter({ hasText: 'Board settings saved.' });
    await expect(flash).toHaveCount(1);
    await expect(flash).toContainText('Board settings saved.');
    expect(boardRow(app, short)).toBe(`${updatedName}|${updatedDescription}|1|0`);
  });

  await measureStep('the public board, catalog, and homepage reflect the new identity', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}`);
    await expect(page.locator('.board-index-header h1')).toContainText(updatedName);
    await expect(page.locator('.board-desc')).toHaveText(updatedDescription);
    // The upload toggle removes the user-visible file input, not just DB text.
    await expect(page.locator('#post-form-wrap input[type="file"]')).toHaveCount(0);
    await expect(page.locator('#post-form-wrap')).toContainText('uploads are disabled on this board');

    await gotoOk(page, `${app.baseURL}/${short}/catalog`);
    await expect(page.locator('.board-catalog-header h1')).toContainText(updatedName);
    await expect(page.locator('.board-desc')).toHaveText(updatedDescription);

    await gotoOk(page, app.baseURL);
    const slug = page.locator('.board-card-slug', { hasText: `/${short}/` });
    const card = page.locator('.board-card').filter({ has: slug });
    await expect(card).toHaveCount(1);
    await expect(card.locator('.board-card-name')).toHaveText(updatedName);
    await expect(card.locator('.board-card-desc')).toHaveText(updatedDescription);
    await expect(card.locator('.nsfw-badge')).toBeVisible();
    await expect(page.locator('[data-board-nsfw="1"]').locator('.board-card').filter({ has: slug })).toHaveCount(1);

    // The unrelated board keeps its own identity and upload policy.
    expect(boardRow(app, control)).toBe(`Control ${control}|control description ${control}|0|1`);
    await gotoOk(page, `${app.baseURL}/${control}`);
    await expect(page.locator('.board-index-header h1')).toContainText(`Control ${control}`);
    await expect(page.locator('#post-form-wrap input[type="file"]')).toHaveCount(1);
  });

  const anonContext = await newAuditedContext(browser);
  const anonPage = await anonContext.newPage();

  await measureStep('a fresh visitor sees the updated board and the upload toggle', async () => {
    actor('unrelated');
    await gotoOk(anonPage, `${app.baseURL}/${short}`);
    await expect(anonPage.locator('.board-index-header h1')).toContainText(updatedName);
    await expect(anonPage.locator('.board-desc')).toHaveText(updatedDescription);
    await expect(anonPage.locator('#post-form-wrap input[type="file"]')).toHaveCount(0);
    await gotoOk(anonPage, app.baseURL);
    const anonSlug = anonPage.locator('.board-card-slug', { hasText: `/${short}/` });
    const anonCard = anonPage.locator('.board-card').filter({ has: anonSlug });
    await expect(anonCard.locator('.board-card-name')).toHaveText(updatedName);
    await expect(anonCard.locator('.nsfw-badge')).toBeVisible();
  });

  await measureStep('re-enabling images through the panel restores the public upload control', async () => {
    actor('admin');
    await adminLogin(adminPage, app);
    await gotoOk(adminPage, `${app.baseURL}/admin/panel?open=board-${short}#board-${short}`);
    const card = adminPage.locator(`#board-${short}`);
    await card.locator('input[name="allow_images"]').check();
    const [saveResponse] = await Promise.all([
      adminPage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST'
        && new URL(candidate.url()).pathname === '/admin/board/settings'),
      card.locator('form.board-settings-form button[type="submit"]').click(),
    ]);
    expect(saveResponse.status()).toBe(303);
    expect(boardRow(app, short)).toBe(`${updatedName}|${updatedDescription}|1|1`);
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}`);
    await expect(page.locator('#post-form-wrap input[type="file"]')).toHaveCount(1);
  });

  await anonContext.close();
  await adminContext.close();
});

// ---------------------------------------------------------------------------
// 5. Search through the real search form
// ---------------------------------------------------------------------------

test('search finds a unique token and a unicode marker and reports an empty result set for a blank query', JOURNEY_DETAILS, async ({ page, app }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noVideos: true });
  const token = `findme${short}`;
  const unicodeToken = 'café漢字';
  const matchingThread = await createThreadViaRequest(page, app, short, {
    subject: `subject ${token}`,
    body: `body carries ${token} once`,
  });
  const otherThread = await createThreadViaRequest(page, app, short, {
    subject: `unrelated subject ${short}`,
    body: 'nothing to match here',
  });
  const unicodeThread = await createThreadViaRequest(page, app, short, {
    subject: `unicode ${short}`,
    body: `marker ${unicodeToken} end`,
  });
  // Search results render posts, so resolve the opening-post ids as well.
  const matchingOpId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${matchingThread} AND is_op = 1;`);
  const otherOpId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${otherThread} AND is_op = 1;`);
  const unicodeOpId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${unicodeThread} AND is_op = 1;`);

  await measureStep('the search form finds the unique token and hides the non-match', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}/search`);
    await page.getByLabel('Query:').fill(token);
    const [response] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'GET'
        && new URL(candidate.url()).pathname === `/${short}/search`),
      page.getByRole('button', { name: 'search' }).click(),
    ]);
    expect(response.status()).toBe(200);
    await expect(page.locator(`#p${matchingOpId} .post-body`)).toContainText(token);
    await expect(page.locator(`#p${otherOpId}`)).toHaveCount(0);
    await expect(page.locator('.board-search-summary-results')).toHaveText('1 result');
  });

  await measureStep('a unicode query matches its post', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}/search`);
    await page.getByLabel('Query:').fill(unicodeToken);
    await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'GET'
        && new URL(candidate.url()).pathname === `/${short}/search`),
      page.getByRole('button', { name: 'search' }).click(),
    ]);
    await expect(page.locator(`#p${unicodeOpId} .post-body`)).toContainText(unicodeToken);
    await expect(page.locator(`#p${matchingOpId}`)).toHaveCount(0);
  });

  await measureStep('a whitespace-only query degrades to the empty result state', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}/search`);
    await page.getByLabel('Query:').fill('   ');
    await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'GET'
        && new URL(candidate.url()).pathname === `/${short}/search`),
      page.getByRole('button', { name: 'search' }).click(),
    ]);
    await expect(page.locator('.board-search-empty')).toContainText('no results found');
    await expect(page.locator('.post')).toHaveCount(0);
    await expect(page.locator('.board-search-form input[name="q"]')).toHaveValue('');
  });
});

// ---------------------------------------------------------------------------
// 6. Pagination boundaries on the board index
// ---------------------------------------------------------------------------

test('board pagination splits threads across pages without duplicates and an out-of-range page stays safe', JOURNEY_DETAILS, async ({ page, app }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noVideos: true });
  const created: number[] = [];
  for (let index = 0; index < 11; index += 1) {
    created.push(await createThreadViaRequest(page, app, short, {
      subject: `page thread ${index} ${short}`,
      body: `page thread body ${index} ${short}`,
    }));
  }

  await measureStep('page one and page two together contain every thread exactly once', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}?page=1`);
    const pageOneIds = await page.locator('.thread').evaluateAll((elements) =>
      elements.map((element) => Number(element.id.slice(1))));
    await expect(page.locator('.pagination')).toHaveText(/page 1 \/ 2/);
    await expect(page.locator('.pagination a', { hasText: '[next]' })).toHaveCount(1);
    expect(pageOneIds).toHaveLength(10);

    await gotoOk(page, `${app.baseURL}/${short}?page=2`);
    const pageTwoIds = await page.locator('.thread').evaluateAll((elements) =>
      elements.map((element) => Number(element.id.slice(1))));
    await expect(page.locator('.pagination')).toHaveText(/page 2 \/ 2/);
    await expect(page.locator('.pagination a', { hasText: '[prev]' })).toHaveCount(1);
    expect(pageTwoIds).toHaveLength(1);

    const union = [...pageOneIds, ...pageTwoIds].sort((left, right) => left - right);
    expect(union).toEqual([...created].sort((left, right) => left - right));
    expect(pageOneIds.filter((id) => pageTwoIds.includes(id))).toEqual([]);
  });

  await measureStep('an out-of-range page renders an empty safe page instead of failing', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}?page=99`);
    await expect(page.locator('.thread')).toHaveCount(0);
    await expect(page.locator('.pagination')).toHaveText(/page 99 \/ 2/);
    await expect(page.locator('body')).toContainText(`/${short}/`);
  });
});

// ---------------------------------------------------------------------------
// 7. NSFW consent gate (homepage gate contract, cookie scoped)
// ---------------------------------------------------------------------------

test('the NSFW disclaimer gate blocks board content until consent, then persists for that browser only', JOURNEY_DETAILS, async ({ page, app, browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const subject = `nsfw gated thread ${uniqueShort('jrnyN', testInfo)}`;
  await createThreadViaRequest(page, app, 'nsfw', { subject, body: `gated body ${subject}` });

  const visitorContext = await newAuditedContext(browser);
  const visitorPage = await visitorContext.newPage();

  await measureStep('the homepage gates the NSFW card and hides board content', async () => {
    actor('visitor');
    await gotoOk(visitorPage, app.baseURL);
    const cardLink = visitorPage.locator('.board-card-link[data-action="open-nsfw-disclaimer"]').first();
    await expect(cardLink).toBeVisible();
    await expect(cardLink).toHaveAttribute('data-board-label', '/nsfw/');
    // Board content is not rendered before consent.
    await expect(visitorPage.locator('.thread').filter({ hasText: subject })).toHaveCount(0);
    await expect(visitorPage.getByText(subject)).toHaveCount(0);
  });

  await measureStep('accepting the disclaimer through the real control reveals the board', async () => {
    actor('visitor');
    await visitorPage.locator('.board-card-link[data-action="open-nsfw-disclaimer"]').first().click();
    const disclaimer = visitorPage.locator('#nsfw-disclaimer-overlay');
    await expect(disclaimer).toBeVisible();
    await expect(disclaimer.getByRole('button', { name: 'I Agree' })).toBeVisible();
    await expect(visitorPage.getByText(subject)).toHaveCount(0);
    const [acceptResponse] = await Promise.all([
      visitorPage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/nsfw/accept'),
      disclaimer.getByRole('button', { name: 'I Agree' }).click(),
    ]);
    expect(acceptResponse.status()).toBe(303);
    await visitorPage.waitForURL(/\/nsfw\/catalog/);
    await expect(visitorPage.locator('.catalog-subject').filter({ hasText: subject })).toHaveCount(1);
  });

  await measureStep('consent survives reload and navigation inside the same browser', async () => {
    actor('visitor');
    await visitorPage.reload();
    await expect(visitorPage.locator('.catalog-subject').filter({ hasText: subject })).toHaveCount(1);
    await gotoOk(visitorPage, `${app.baseURL}/nsfw`);
    await expect(visitorPage.locator('.thread').filter({ hasText: subject })).toHaveCount(1);
    // Unrelated boards keep rendering normally.
    await gotoOk(visitorPage, `${app.baseURL}/pub`);
    await expect(visitorPage.locator('.board-index-header h1')).toContainText('/pub/');
    await expect(visitorPage.locator('#nsfw-disclaimer-overlay')).toHaveCount(0);
    const consentCookie = (await visitorContext.cookies()).find((cookie) => cookie.name === 'rustchan_nsfw_ok');
    expect(consentCookie?.value).toBe('1');
  });

  await measureStep('a second browser still sees the gate', async () => {
    actor('unrelated');
    const otherContext = await newAuditedContext(browser);
    const otherPage = await otherContext.newPage();
    await gotoOk(otherPage, app.baseURL);
    await expect(otherPage.locator('.board-card-link[data-action="open-nsfw-disclaimer"]').first()).toBeVisible();
    await expect(otherPage.getByText(subject)).toHaveCount(0);
    const otherCookies = await otherContext.cookies();
    expect(otherCookies.find((cookie) => cookie.name === 'rustchan_nsfw_ok')).toBeUndefined();
    await otherContext.close();
  });

  await visitorContext.close();
});

// ---------------------------------------------------------------------------
// 8. Poll lifecycle with a three-option poll
// ---------------------------------------------------------------------------

test('a three-option poll accepts one vote per identity, rejects a duplicate, and counts a second actor', JOURNEY_DETAILS, async ({ page, app, browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  const question = `journey poll ${short}?`;
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noVideos: true });

  let threadId = 0;
  let pollId = 0;
  let alphaId = 0;
  let betaId = 0;

  await measureStep('the poll is created through the new-thread form', async () => {
    actor('owner');
    await gotoOk(page, `${app.baseURL}/${short}`);
    const form = await openComposer(page, `form[action="/${short}"]`);
    await form.locator('input[name="subject"]').fill(`poll thread ${short}`);
    await form.locator('textarea[name="body"]').fill(`poll body ${short}`);
    await form.locator('details.poll-creator > summary').click();
    await form.locator('input[name="poll_question"]').fill(question);
    const optionInputs = form.locator('input[name="poll_option"]');
    await optionInputs.nth(0).fill('Option Alpha');
    await optionInputs.nth(1).fill('Option Beta');
    await form.locator('[data-action="add-poll-option"]').click();
    await expect(optionInputs).toHaveCount(3);
    await optionInputs.nth(2).fill('Option Gamma');
    const [response] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${short}`),
      form.getByRole('button', { name: /post thread/i }).click(),
    ]);
    expect(response.status()).toBe(303);
    await page.waitForURL(new RegExp(`/${short}/thread/\\d+`));
    threadId = threadIdFromUrl(page.url());
    pollId = scalar(app, `SELECT id FROM polls WHERE thread_id = ${threadId};`);
    alphaId = scalar(app, `SELECT id FROM poll_options WHERE poll_id = ${pollId} AND text = 'Option Alpha';`);
    betaId = scalar(app, `SELECT id FROM poll_options WHERE poll_id = ${pollId} AND text = 'Option Beta';`);
    expect(pollId).toBeGreaterThan(0);
    expect(
      scalar(app, `SELECT COUNT(*) FROM poll_options WHERE poll_id = ${pollId};`),
      'three options should be stored for the poll',
    ).toBe(3);
    await expect(page.locator('.poll-container .poll-question')).toHaveText(question);
    await expect(page.locator('.poll-vote-option')).toHaveCount(3);
    await expect(page.locator('.poll-total')).toHaveCount(0);
  });

  const secondActorContext = await newAuditedContext(browser);
  const secondActorPage = await secondActorContext.newPage();
  // Same identity, second document: its pre-vote form is the duplicate vector.
  const stalePage = await page.context().newPage();

  await measureStep('a non-voter sees the ballot while the owner votes', async () => {
    actor('unrelated');
    await gotoOk(secondActorPage, `${app.baseURL}/${short}/thread/${threadId}`);
    await expect(secondActorPage.locator('.poll-vote-option')).toHaveCount(3);
    await expect(secondActorPage.locator('.poll-results')).toHaveCount(0);
    await gotoOk(stalePage, `${app.baseURL}/${short}/thread/${threadId}`);
    await expect(stalePage.locator('.poll-vote-option')).toHaveCount(3);

    actor('owner');
    await gotoOk(page, `${app.baseURL}/${short}/thread/${threadId}`);
    await page.locator('.poll-vote-option').filter({ hasText: 'Option Alpha' }).locator('input[name="option_id"]').check();
    const [voteResponse] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/vote'),
      page.getByRole('button', { name: '[ Cast Vote ]' }).click(),
    ]);
    expect(voteResponse.status()).toBe(303);
    await page.waitForURL(new RegExp(`/${short}/thread/${threadId}`));
    const alphaResult = page.locator('.poll-option-result').filter({ hasText: 'Option Alpha' });
    await expect(alphaResult.locator('.poll-opt-count')).toHaveText('1 (100%)');
    await expect(alphaResult).toHaveClass(/user-voted/);
    await expect(page.locator('.poll-total')).toHaveText('1 total vote');
    expect(scalar(app, `SELECT COUNT(*) FROM poll_votes WHERE poll_id = ${pollId};`)).toBe(1);
    expect(scalar(app, `SELECT COUNT(*) FROM poll_votes WHERE option_id = ${alphaId};`)).toBe(1);
  });

  await measureStep('a duplicate vote from the same identity is rejected and changes nothing', async () => {
    actor('owner');
    expectHttpError({
      method: 'POST',
      path: '/vote',
      status: 400,
      reason: 'the duplicate vote is rejected with the documented inline error page',
    });
    await stalePage.locator('.poll-vote-option').filter({ hasText: 'Option Beta' }).locator('input[name="option_id"]').check();
    const [duplicate] = await Promise.all([
      stalePage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/vote'),
      stalePage.getByRole('button', { name: '[ Cast Vote ]' }).click(),
    ]);
    expect(duplicate.status()).toBe(400);
    await expect(stalePage.locator('.error-page')).toContainText('already voted');
    expect(scalar(app, `SELECT COUNT(*) FROM poll_votes WHERE poll_id = ${pollId};`)).toBe(1);
    expect(scalar(app, `SELECT COUNT(*) FROM poll_votes WHERE option_id = ${betaId};`)).toBe(0);
  });

  await measureStep('a different identity voting increments the chosen option only', async () => {
    actor('unrelated');
    await secondActorPage.locator('.poll-vote-option').filter({ hasText: 'Option Beta' }).locator('input[name="option_id"]').check();
    const [voteResponse] = await Promise.all([
      secondActorPage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/vote'),
      secondActorPage.getByRole('button', { name: '[ Cast Vote ]' }).click(),
    ]);
    expect(voteResponse.status()).toBe(303);
    await secondActorPage.waitForURL(new RegExp(`/${short}/thread/${threadId}`));
    const betaResult = secondActorPage.locator('.poll-option-result').filter({ hasText: 'Option Beta' });
    await expect(betaResult.locator('.poll-opt-count')).toHaveText('1 (50%)');
    await expect(betaResult).toHaveClass(/user-voted/);
    await expect(secondActorPage.locator('.poll-option-result').filter({ hasText: 'Option Alpha' })).not.toHaveClass(/user-voted/);
    await expect(secondActorPage.locator('.poll-total')).toHaveText('2 total votes');
    expect(scalar(app, `SELECT COUNT(*) FROM poll_votes WHERE poll_id = ${pollId};`)).toBe(2);
    expect(scalar(app, `SELECT COUNT(*) FROM poll_votes WHERE option_id = ${alphaId};`)).toBe(1);
    expect(scalar(app, `SELECT COUNT(*) FROM poll_votes WHERE option_id = ${betaId};`)).toBe(1);

    actor('owner');
    await page.reload();
    await expect(page.locator('.poll-total')).toHaveText('2 total votes');
    await expect(page.locator('.poll-option-result').filter({ hasText: 'Option Alpha' })).toHaveClass(/user-voted/);
    await expect(page.locator('.poll-option-result').filter({ hasText: 'Option Beta' })).not.toHaveClass(/user-voted/);
  });

  await secondActorContext.close();
});

// ---------------------------------------------------------------------------
// 9. Poll creation with JavaScript disabled (noscript option rows)
// ---------------------------------------------------------------------------

test('a poll with three options is creatable without JavaScript through the noscript option rows', JOURNEY_DETAILS, async ({ page, app }, testInfo) => {
  test.skip(
    testInfo.project.name !== 'chromium-nojs' && testInfo.project.name !== 'firefox-nojs',
    'This journey asserts the noscript poll option rows, which only exist when scripting is disabled; JavaScript projects cover the add-option button instead.',
  );
  const short = boardShort(testInfo);
  const question = `no-js poll ${short}?`;
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noVideos: true });

  await measureStep('the no-JS form exposes the extra option rows and creates the poll', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}`);
    const form = page.locator(`form[action="/${short}"]`).first();
    await expect(form).toBeVisible();
    await form.locator('input[name="subject"]').fill(`no-js poll thread ${short}`);
    await form.locator('textarea[name="body"]').fill(`no-js poll body ${short}`);
    await form.locator('details.poll-creator > summary').click();
    await form.locator('input[name="poll_question"]').fill(question);
    const optionInputs = form.locator('input[name="poll_option"]');
    await optionInputs.nth(0).fill('NoJS One');
    await optionInputs.nth(1).fill('NoJS Two');
    // The extra rows only exist inside the noscript fallback.
    const extraDetails = form.locator('details.poll-creator noscript details');
    await expect(extraDetails).toHaveCount(1);
    await extraDetails.locator('summary').click();
    await optionInputs.nth(2).fill('NoJS Three');
    const [response] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${short}`),
      form.getByRole('button', { name: /post thread/i }).click(),
    ]);
    expect(response.status()).toBe(303);
    await page.waitForURL(new RegExp(`/${short}/thread/\\d+`));
    const threadId = threadIdFromUrl(page.url());
    const pollId = scalar(app, `SELECT id FROM polls WHERE thread_id = ${threadId};`);
    expect(scalar(app, `SELECT COUNT(*) FROM poll_options WHERE poll_id = ${pollId};`)).toBe(3);
    await expect(page.locator('.poll-container .poll-question')).toHaveText(question);
    await expect(page.locator('.poll-vote-option')).toHaveCount(3);
    await expect(page.locator('.poll-opt-text').filter({ hasText: 'NoJS Three' })).toHaveCount(1);
  });
});

// ---------------------------------------------------------------------------
// 10. Sage and bump semantics
// ---------------------------------------------------------------------------

test('a sage reply raises reply_count without bumping while a normal reply moves bumped_at forward', JOURNEY_DETAILS, async ({ page, app }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noVideos: true });
  const threadId = await createThread(page, app, short, {
    subject: `sage thread ${short}`,
    body: `sage thread body ${short}`,
  });
  // Freeze the bump clock so "did not advance" is observable even in a fast test.
  const frozenBumpedAt = Math.floor(Date.now() / 1000) - 300;
  setThreadFixtureState(app, threadId, { bumpedAt: frozenBumpedAt });

  await measureStep('the sage reply increments the reply count but not the bump clock', async () => {
    actor('owner');
    await gotoOk(page, `${app.baseURL}/${short}/thread/${threadId}`);
    const form = await openComposer(page, `form[action="/${short}/thread/${threadId}"]`);
    await form.locator('textarea[name="body"]').fill(`sage reply ${short}`);
    await form.locator('input[name="sage"]').check();
    const [response] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST'
        && new URL(candidate.url()).pathname === `/${short}/thread/${threadId}`),
      form.getByRole('button', { name: /post reply/i }).click(),
    ]);
    expect(response.status()).toBe(303);
    await expect(page.locator('.post').filter({ hasText: `sage reply ${short}` })).toHaveCount(1);
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
    expect(scalar(app, `SELECT reply_count FROM threads WHERE id = ${threadId};`)).toBe(1);
    expect(
      scalar(app, `SELECT bumped_at FROM threads WHERE id = ${threadId};`),
      'a sage reply must not bump the thread',
    ).toBe(frozenBumpedAt);
  });

  await measureStep('a normal reply moves the bump clock forward', async () => {
    actor('owner');
    const form = await openComposer(page, `form[action="/${short}/thread/${threadId}"]`);
    await form.locator('textarea[name="body"]').fill(`bumping reply ${short}`);
    await form.locator('input[name="sage"]').uncheck();
    const [response] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST'
        && new URL(candidate.url()).pathname === `/${short}/thread/${threadId}`),
      form.getByRole('button', { name: /post reply/i }).click(),
    ]);
    expect(response.status()).toBe(303);
    await expect(page.locator('.post').filter({ hasText: `bumping reply ${short}` })).toHaveCount(1);
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('2');
    expect(scalar(app, `SELECT reply_count FROM threads WHERE id = ${threadId};`)).toBe(2);
    const bumpedAt = scalar(app, `SELECT bumped_at FROM threads WHERE id = ${threadId};`);
    expect(bumpedAt, 'a normal reply should move bumped_at forward').toBeGreaterThan(frozenBumpedAt);
    expect(Math.abs(Math.floor(Date.now() / 1000) - bumpedAt)).toBeLessThan(30);
  });
});

// ---------------------------------------------------------------------------
// 11. Boundary states: empty board, validation limits, unicode round-trip,
//     missing thread
// ---------------------------------------------------------------------------

test('an empty board, the one-thread state, validation limits, unicode round-trips, and a missing thread all behave', JOURNEY_DETAILS, async ({ page, app }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noImages: true, noVideos: true });

  await measureStep('a freshly created board renders both empty states', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}`);
    await expect(page.locator('.thread')).toHaveCount(0);
    await expect(page.locator('.pagination')).toHaveCount(0);
    await gotoOk(page, `${app.baseURL}/${short}/catalog`);
    await expect(page.locator('.catalog-empty-state')).toContainText('No threads yet.');
  });

  const singleThread = await createThreadViaRequest(page, app, short, {
    subject: `single thread ${short}`,
    body: `single thread body ${short}`,
  });

  await measureStep('exactly one thread appears in the index and catalog', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}`);
    await expect(page.locator('.thread')).toHaveCount(1);
    await expect(page.locator(`#t${singleThread} .subject`)).toContainText(`single thread ${short}`);
    await gotoOk(page, `${app.baseURL}/${short}/catalog`);
    await expect(page.locator('.catalog-item')).toHaveCount(1);
  });

  await measureStep('an over-limit body is rejected inline and creates no post row', async () => {
    actor('visitor');
    const postsBefore = scalar(app, `SELECT COUNT(*) FROM posts WHERE board_id = ${boardId(app, short)};`);
    await gotoOk(page, `${app.baseURL}/${short}`);
    const form = await openComposer(page, `form[action="/${short}"]`);
    const oversizedBody = 'x'.repeat(4097);
    await form.locator('input[name="subject"]').fill(`too long ${short}`);
    // `fill` inserts text through the browser, which truncates at the
    // textarea's maxlength="4096"; the contract under test here is the
    // server-side limit, so the over-limit value is assigned directly and then
    // submitted through the real form.
    await form.locator('textarea[name="body"]').evaluate((element, value) => {
      (element as HTMLTextAreaElement).value = value;
    }, oversizedBody);
    await expect(form.locator('textarea[name="body"]')).toHaveValue(oversizedBody);
    expectHttpError({
      method: 'POST',
      path: `/${short}`,
      status: 422,
      reason: 'an over-limit body is re-rendered inline with the documented validation banner',
    });
    const [response] = await Promise.all([
      page.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${short}`),
      form.getByRole('button', { name: /post thread/i }).click(),
    ]);
    expect(response.status()).toBe(422);
    await expect(page.locator('.post-error-banner')).toContainText('Post body exceeds 4096 characters.');
    await expect(page.locator('.post-error-banner')).not.toContainText('thread panicked');
    // The draft survives the failed submission.
    await expect(page.locator(`form[action="/${short}"]`).first().locator('textarea[name="body"]')).toHaveValue(oversizedBody);
    expect(scalar(app, `SELECT COUNT(*) FROM posts WHERE board_id = ${boardId(app, short)};`)).toBe(postsBefore);
  });

  await measureStep('a whitespace-only body is rejected and creates no post row', async () => {
    actor('visitor');
    const postsBefore = scalar(app, `SELECT COUNT(*) FROM posts WHERE board_id = ${boardId(app, short)};`);
    const csrf = await publicCsrf(page, app, `/${short}`);
    const response = await page.request.post(`${app.baseURL}/${short}`, {
      multipart: {
        _csrf: csrf,
        submission_token: `journey-${short}-${Date.now()}`,
        subject: `blank ${short}`,
        body: '   \n\t  ',
      },
      maxRedirects: 0,
    });
    expect(response.status()).toBe(422);
    const text = await expectSafeResponse(response);
    expect(text).toContain('Post body cannot be empty.');
    expect(scalar(app, `SELECT COUNT(*) FROM posts WHERE board_id = ${boardId(app, short)};`)).toBe(postsBefore);
  });

  await measureStep('unicode, emoji, and a long unbroken run round-trip intact', async () => {
    actor('visitor');
    const unicodeBody = `roundtrip café漢字 🚀🙂 ${'A'.repeat(400)}Z`;
    const unicodeThread = await createThreadViaRequest(page, app, short, {
      subject: `unicode thread ${short}`,
      body: unicodeBody,
    });
    const unicodeOpId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${unicodeThread} AND is_op = 1;`);
    await gotoOk(page, `${app.baseURL}/${short}/thread/${unicodeThread}`);
    const rendered = await page.locator(`#p${unicodeOpId} .post-body`).innerText();
    expect(rendered).toBe(unicodeBody);
    expect(sqliteQuery(app, `SELECT body FROM posts WHERE thread_id = ${unicodeThread} AND is_op = 1;`)).toBe(unicodeBody);
  });

  await measureStep('a missing thread id renders the documented 404 instead of failing', async () => {
    actor('visitor');
    const missingPath = `/${short}/thread/99999999`;
    expectHttpError({
      method: 'GET',
      path: missingPath,
      status: 404,
      reason: 'a missing thread id must render the 404 page',
    });
    const response = await page.goto(`${app.baseURL}${missingPath}`, { waitUntil: 'domcontentloaded' });
    expect(response, 'the 404 navigation should still produce a document').not.toBeNull();
    expect(response!.status()).toBe(404);
    await expect(page.locator('.error-page h1')).toHaveText('error 404');
    await expect(page.locator('.error-page')).toContainText(/not found/i);
    await expect(page.locator('body')).not.toContainText('thread panicked');
  });
});

// ---------------------------------------------------------------------------
// 12. Admin moderation: report queue and thread deletion
// ---------------------------------------------------------------------------

test('admin deletes a reported thread through the modal, drains the report queue, and records the act in the mod log', JOURNEY_DETAILS, async ({ page, app, browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  const subject = `reported thread ${short}`;
  const reason = `journey report ${short}`;
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noVideos: true });
  const threadId = await createThreadViaRequest(page, app, short, { subject, body: `reported body ${short}` });
  const opId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 1;`);

  const reporterContext = await newAuditedContext(browser);
  const reporterPage = await reporterContext.newPage();
  const adminContext = await newAuditedContext(browser);
  const adminPage = await adminContext.newPage();

  await measureStep('a visitor reports the opening post through the report modal', async () => {
    actor('unrelated');
    await gotoOk(reporterPage, `${app.baseURL}/${short}/thread/${threadId}`);
    await reporterPage.locator(`#p${opId} button[data-action="open-report"]`).click();
    const modal = reporterPage.locator('#report-modal');
    await expect(modal).toBeVisible();
    await modal.locator('#report-reason').fill(reason);
    const [response] = await Promise.all([
      reporterPage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/report'),
      modal.locator('#report-submit-btn').click(),
    ]);
    expect(response.status()).toBe(303);
    await reporterPage.waitForURL(new RegExp(`/${short}/thread/${threadId}\\?reported=1`));
    await expect(reporterPage.locator('.post-success-banner')).toContainText('Report submitted');
    expect(scalar(app, `SELECT COUNT(*) FROM reports WHERE board_id = ${boardId(app, short)} AND status = 'open';`)).toBe(1);
  });

  await measureStep('the admin panel shows the queue entry before the thread is deleted', async () => {
    actor('admin');
    await adminLogin(adminPage, app);
    await gotoOk(adminPage, `${app.baseURL}/admin/panel?open=reports#reports`);
    await expect(adminPage.locator('#reports details.admin-dropdown').first()).toHaveAttribute('open', '');
    await expect(adminPage.locator('#reports .admin-dropdown-counter-label')).toContainText('Report inbox: [1]');
    await expect(adminPage.locator('#reports')).toContainText(reason);
  });

  await measureStep('the admin deletes the whole thread through the modal confirmation', async () => {
    actor('admin');
    await gotoOk(adminPage, `${app.baseURL}/${short}/thread/${threadId}`);
    const deleteButton = adminPage.locator('.admin-toolbar button', { hasText: 'delete thread' });
    await expect(deleteButton).toBeVisible();
    await expectNoDialog(adminPage, async () => {
      await deleteButton.click();
      await expect(adminPage.locator('#confirm-modal-message')).toHaveText('Delete this entire thread and all its posts?');
      await adminPage.locator('#confirm-modal-continue').click();
    });
    await adminPage.waitForURL(new RegExp(`/${short}$`));
    await expect(adminPage.locator('.thread').filter({ hasText: subject })).toHaveCount(0);
    expect(scalar(app, `SELECT COUNT(*) FROM threads WHERE id = ${threadId};`)).toBe(0);
    expect(scalar(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`)).toBe(0);
    expect(scalar(app, `SELECT COUNT(*) FROM reports WHERE thread_id = ${threadId};`)).toBe(0);
  });

  await measureStep('board, catalog, and moderation log reflect the deletion', async () => {
    actor('visitor');
    await gotoOk(page, `${app.baseURL}/${short}`);
    await expect(page.locator('.thread')).toHaveCount(0);
    await gotoOk(page, `${app.baseURL}/${short}/catalog`);
    await expect(page.locator('.catalog-empty-state')).toContainText('No threads yet.');

    actor('admin');
    await gotoOk(adminPage, `${app.baseURL}/admin/panel?open=reports#reports`);
    await expect(adminPage.locator('#reports .admin-dropdown-counter-label')).toContainText('Report inbox: [0]');
    expect(scalar(app, `SELECT COUNT(*) FROM mod_log WHERE action = 'delete_thread' AND target_id = ${threadId};`)).toBe(1);
    await gotoOk(adminPage, `${app.baseURL}/admin/mod-log`);
    const logRow = adminPage.locator('table.admin-table tbody tr').filter({ hasText: `thread #${threadId}` });
    await expect(logRow).toHaveCount(1);
    await expect(logRow).toContainText('delete_thread');
  });

  await reporterContext.close();
  await adminContext.close();
});

// ---------------------------------------------------------------------------
// 13. Admin moderation: ban add/remove lifecycle
// ---------------------------------------------------------------------------

test('admin bans a real poster hash, the actor lands on the ban notice, and lifting the ban restores posting', JOURNEY_DETAILS, async ({ page, app, browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'Product journey coverage runs on desktop Chromium.');
  const short = boardShort(testInfo);
  const banReason = `journey ban ${short}`;
  app.createBoardCli({ short, name: `Journey ${short}`, description: `journey board ${short}`, noVideos: true });
  const threadId = await createThreadViaRequest(page, app, short, {
    subject: `ban host thread ${short}`,
    body: `ban host body ${short}`,
  });
  const replyBody = `ban target reply ${short}`;
  await createReply(page, app, short, threadId, replyBody);
  const replyId = scalar(app, `SELECT id FROM posts WHERE thread_id = ${threadId} AND body = '${replyBody}';`);
  const ipHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE id = ${replyId};`);
  expect(ipHash).toMatch(/^[0-9a-f]{64}$/);

  const adminContext = await newAuditedContext(browser);
  const adminPage = await adminContext.newPage();

  await measureStep('the admin bans the poster hash through the panel form', async () => {
    actor('admin');
    await adminLogin(adminPage, app);
    await gotoOk(adminPage, `${app.baseURL}/admin/panel?open=reports#reports`);
    const banForm = adminPage.locator('form[action="/admin/ban/add"]');
    await expect(banForm).toBeVisible();
    await banForm.locator('input[name="ip_hash"]').fill(ipHash);
    await banForm.locator('input[name="reason"]').fill(banReason);
    await banForm.locator('input[name="duration_hours"]').fill('5');
    const [response] = await Promise.all([
      adminPage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/admin/ban/add'),
      banForm.getByRole('button', { name: 'ban' }).click(),
    ]);
    expect(response.status()).toBe(303);
    const banFlash = adminPage.locator('.admin-flash.flash-ok').filter({ hasText: 'Ban added.' });
    await expect(banFlash).toHaveCount(1);
    await expect(banFlash).toContainText('Ban added.');

    const storedReason = sqliteQuery(app, `SELECT reason FROM bans WHERE ip_hash = '${ipHash}';`);
    expect(storedReason).toBe(banReason);
    const banWindowSecs = scalar(app, `SELECT expires_at - created_at FROM bans WHERE ip_hash = '${ipHash}';`);
    expect(banWindowSecs).toBeGreaterThanOrEqual(17_990);
    expect(banWindowSecs).toBeLessThanOrEqual(18_010);
    await gotoOk(adminPage, `${app.baseURL}/admin/panel?open=reports#reports`);
    const banRow = adminPage.locator('#active-bans tbody tr').filter({ hasText: banReason });
    await expect(banRow).toHaveCount(1);
  });

  await measureStep('the banned actor is sent to the ban notice when posting', async () => {
    actor('owner');
    await gotoOk(page, `${app.baseURL}/${short}`);
    const form = await openComposer(page, `form[action="/${short}"]`);
    await form.locator('input[name="subject"]').fill(`banned attempt ${short}`);
    await form.locator('textarea[name="body"]').fill(`banned attempt body ${short}`);
    await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
    await form.getByRole('button', { name: /post thread/i }).click();
    await page.waitForURL(/\/banned/);
    await expect(page.getByRole('heading', { name: /you are banned/i })).toBeVisible();
    await expect(page.locator('.error-page')).toContainText(banReason);
    expect(
      scalar(app, `SELECT COUNT(*) FROM posts WHERE body = 'banned attempt body ${short}';`),
      'the banned attempt must not create a post row',
    ).toBe(0);
  });

  await measureStep('the admin lifts the ban and the actor can post again', async () => {
    actor('admin');
    await gotoOk(adminPage, `${app.baseURL}/admin/panel?open=reports#reports`);
    const banRow = adminPage.locator('#active-bans tbody tr').filter({ hasText: banReason });
    await expect(banRow).toHaveCount(1);
    const [response] = await Promise.all([
      adminPage.waitForResponse((candidate) =>
        candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === '/admin/ban/remove'),
      banRow.getByRole('button', { name: 'lift' }).click(),
    ]);
    expect(response.status()).toBe(303);
    const liftFlash = adminPage.locator('.admin-flash.flash-ok').filter({ hasText: 'Ban lifted.' });
    await expect(liftFlash).toHaveCount(1);
    await expect(liftFlash).toContainText('Ban lifted.');
    expect(scalar(app, `SELECT COUNT(*) FROM bans WHERE ip_hash = '${ipHash}';`)).toBe(0);

    actor('owner');
    const restoredThread = await createThread(page, app, short, {
      subject: `restored thread ${short}`,
      body: `restored body ${short}`,
      filePath: app.fixtures().tinyPng,
    });
    expect(restoredThread).toBeGreaterThan(0);
    await expect(page.locator('.post.op .subject')).toContainText(`restored thread ${short}`);
    expect(scalar(app, `SELECT COUNT(*) FROM threads WHERE board_id = ${boardId(app, short)};`)).toBe(2);
  });

  await adminContext.close();
});

// ---------------------------------------------------------------------------
// Contract ledger
// ---------------------------------------------------------------------------
//
// Asserted by this file (browser-visible unless noted):
// * Anonymous identity is cookie-scoped: a fresh context is a different actor,
//   and a second document in the same context shares the identity (duplicate
//   poll vote rejection proves it).
// * Media upload lifecycle: an image OP stores body/file_path/thumb_path
//   and a file_hashes row, renders the download link plus a loadable thumbnail,
//   and appears in board index, catalog, and thread views to another actor.
// * Replies advance threads.reply_count and the rendered
//   [data-role="thread-reply-count"] by exactly one.
// * The owner edit modal POSTs to /{board}/post/{id}/edit, answers with the
//   X-Rustchan-Redirect header, sets posts.edited_at, and the edited body is
//   visible after the modal flow, after reload, and to another actor.
// * Owner self-delete via the confirmation modal removes the post for both
//   actors, decrements reply_count, and leaves the OP media row and file intact.
//   Note: the brief's "owner deletes the unrelated actor's reply" is impossible
//   by design (self-delete authority is the per-post rustchan_owned_posts grant
//   of the author's browser), so the owner deletes their own reply while the
//   unrelated reply acts as the unchanged control.
// * allow_editing=false / allow_self_delete=false hide .self-action-controls;
//   GET /{board}/post/{id}/edit answers 403 and POST .../delete answers 403 with
//   the documented message; the post row is unchanged.
// * Theme and preference cookies persist across navigation, reload, and server
//   restart for the choosing browser, and never leak into a fresh context.
// * Board settings saved in the admin panel update the board page, catalog,
//   homepage card and a fresh visitor; the unrelated board is unchanged; the
//   allow_images toggle adds/removes the user-visible upload input.
// * Search (GET /{board}/search) matches a unique token and a unicode token,
//   hides non-matches, and degrades a whitespace query to the empty state.
// * Board pagination (?page=2) partitions threads without duplicates or loss;
//   ?page=99 stays a safe empty page (HTTP 200).
// * The NSFW gate is a homepage consent gate: before consent the card opens the
//   disclaimer overlay and no board content is rendered; POST /nsfw/accept
//   redirects to the board, persists across reload/navigation for that browser,
//   and leaves other browsers and unrelated boards unaffected.
// * Poll creation through the form stores three poll_options; a vote through
//   the /vote form records one poll_votes row and marks the result; a duplicate
//   vote from the same identity answers 400 and adds no row; a different
//   identity's vote increments only its option.
// * Without JavaScript the noscript "More poll options" rows can create a
//   three-option poll (chromium-nojs and firefox-nojs projects).
// * A sage reply increments reply_count without moving threads.bumped_at; a
//   normal reply moves it forward.
// * Board boundaries: empty index, empty catalog state, one-thread state,
//   over-limit body -> 422 + banner + no row + draft preserved, whitespace body
//   -> 422 + "Post body cannot be empty." + no row, unicode/emoji/long-run body
//   round-trips byte-for-byte, missing thread -> 404 error page.
// * Admin moderation: the report queue counter and row are visible, the delete
//   modal removes the thread from board/catalog/DB (reports cascade), the
//   moderation counter returns to zero, and mod_log records delete_thread.
// * Admin bans a real posts.ip_hash from the panel; the stored reason and
//   duration window match; the banned actor is redirected to /banned with the
//   reason and no post row is created; lifting the ban restores posting.
//
// Left unverified here (covered elsewhere or by design out of scope):
// * View/post-password boards, CAPTCHA, archive/hidden-thread flows, pin/hide
//   thread preferences, catalog sorting, media viewers beyond thumbnails,
//   backups/maintenance, Tor/TLS, and all layout/contrast/responsive checks.
// * The `/theme/{theme}` GET link is a supported theme entry point that this
//   file does not exercise; only the preferences form selector is driven.
// * Multi-board scale limits (max_threads, bump_limit overflow) are not pushed
//   to their caps; only the default configuration is exercised.
// * The NSFW gate is asserted where the app implements it (the homepage card
//   and the /?nsfw= entry point). Direct navigation to /nsfw or /nsfw/catalog
//   is not gated by the server, so the journey documents the real contract
//   rather than an assumed board-level gate.
// * The strict diagnostics publication of console/HTTP declarations for 4xx
//   navigations declares only expectHttpError: diagnostics.ts derives the
//   Chromium "Failed to load resource" console message from the declared HTTP
//   error response, so a separate expectConsoleError is neither needed nor
//   counted.
