import {
  adminPasswordHash,
  expect,
  expectSafePage,
  expectSafeResponse,
  setBoardFixtureSettings,
  setPostFixtureCreatedAt,
  setSiteFixtureSettings,
  setThreadFixtureState,
  test,
  threadIdFromUrl,
  uniqueShort,
} from './helpers';
import fsp from 'node:fs/promises';
import type { Page } from '@playwright/test';

test.beforeEach(async ({}, testInfo) => {
  test.skip(testInfo.project.name !== 'firefox-nojs', 'Firefox/no-JS regression project only');
});

test.describe('Firefox no-JS public regression', () => {
  test('public browsing, anchors, quotes, posting validation, and readable errors are server-rendered', async ({ page, app }) => {
    await page.goto(app.baseURL);
    await expectSafePage(page);
    await expect(page.locator('html')).toHaveClass(/no-js/);
    await expect(page.locator('.board-card-link[href="/pub"], .board-card-link[href="/pub/catalog"]').first()).toBeVisible();

    await page.goto(`${app.baseURL}/pub`);
    await expectSafePage(page);
    await expect(page.locator('form[action="/pub"] textarea[name="body"]')).toBeVisible();

    const threadId = await createThreadNoJs(page, app.baseURL, 'pub', {
      name: 'Alice#secret',
      subject: 'Firefox no JS public thread',
      body: 'OP body with >>123 and https://example.test/no-js',
    });
    const op = page.locator('.post.op').first();
    const opPostId = await postId(op);
    await expect(op.locator('.subject')).toContainText('Firefox no JS public thread');
    await expect(op.locator('.name')).toContainText('Alice');
    await expect(op.locator('.tripcode')).toContainText(/^!/);
    await expect(op.locator(`a.post-num[href="#p${opPostId}"]`)).toBeVisible();

    await createReplyNoJs(page, app.baseURL, 'pub', threadId, {
      name: 'Bob',
      body: `replying to >>${opPostId}`,
    });
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
    await expect(page.locator(`.quotelink[href="#p${opPostId}"]`).first()).toBeVisible();

    await page.goto(`${app.baseURL}/pub/catalog`);
    await expectSafePage(page);
    await expect(page.locator(`a[href="/pub/thread/${threadId}"]`).first()).toBeVisible();
    await page.goto(`${app.baseURL}/pub/thread/${threadId}#p${opPostId}`);
    await expect(page.locator(`#p${opPostId}`)).toBeVisible();
    await page.goBack();
    await expect(page).toHaveURL(/\/pub\/catalog/);

    await page.goto(`${app.baseURL}/pub?page=999`);
    await expectSafePage(page);
    await expect(page.locator('body')).not.toContainText(/blank|panic|stack backtrace/i);

    await page.goto(`${app.baseURL}/pub`);
    const emptyForm = page.locator('form[action="/pub"]').first();
    await emptyForm.locator('textarea[name="body"]').fill('');
    await emptyForm.getByRole('button', { name: /post thread/i }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/body|empty|required|attached file/i);
    await expectSafePage(page);

    const missing = await page.request.get(`${app.baseURL}/pub/thread/999999999`);
    expect([404, 410]).toContain(missing.status());
    await expectSafeResponse(missing);
  });

  test('own-post edit and delete fallbacks use ownership cookies and fail closed', async ({ page, browser, app }, testInfo) => {
    const board = uniqueShort('own', testInfo);
    app.createBoardCli({ short: board, name: 'Own Post Fallback' });
    setBoardFixtureSettings(app, board, {
      allowEditing: true,
      allowSelfDelete: true,
      postCooldownSecs: 0,
    });

    const threadId = await createThreadNoJs(page, app.baseURL, board, {
      subject: 'own controls',
      body: 'original own-post body',
    });
    const opPostId = await postId(page.locator('.post.op').first());
    const editHref = await page.locator('.self-action-controls .edit-btn').first().getAttribute('href');
    expect(editHref).toBe(`/${board}/post/${opPostId}/edit`);

    await page.goto(`${app.baseURL}${editHref}`);
    await expect(page.locator('form[action$="/edit"] textarea[name="body"]')).toBeVisible();
    await page.locator('textarea[name="body"]').fill('edited through no-js fallback');
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}#p${opPostId}`)),
      page.getByRole('button', { name: /save edit/i }).click(),
    ]);
    await expect(page.locator(`#p${opPostId}`)).toContainText('edited through no-js fallback');

    const other = await browser.newContext({ javaScriptEnabled: false });
    const otherPage = await other.newPage();
    await otherPage.goto(`${app.baseURL}${editHref}`);
    await expect(otherPage.locator('body')).toContainText(/permission|available|browser|forbidden/i);
    await other.close();

    const expiredId = await createThreadNoJs(page, app.baseURL, board, {
      subject: 'expired grant',
      body: 'created then expired',
    });
    const expiredPostId = await postId(page.locator('.post.op').first());
    setPostFixtureCreatedAt(app, expiredPostId, Math.floor(Date.now() / 1000) - 120);
    await page.goto(`${app.baseURL}/${board}/post/${expiredPostId}/edit`);
    await expect(page.locator('body')).toContainText(/edit window|closed|forbidden/i);

    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    const deleteHref = await page.locator(`#p${opPostId} .self-action-controls .del-btn`).getAttribute('href');
    expect(deleteHref).toBe(`/${board}/post/${opPostId}/delete`);
    await page.goto(`${app.baseURL}${deleteHref}`);
    await expect(page.locator('form[action$="/delete"]')).toBeVisible();
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/catalog|/${board}/thread/${threadId}|/${board}`)),
      page.getByRole('button', { name: /delete post/i }).click(),
    ]);
    await expectSafePage(page);
    await page.goto(`${app.baseURL}/${board}/thread/${expiredId}`);
    await expect(page.locator('body')).toContainText('created then expired');
  });

  test('media upload, render, headers, disabled policy, size, and traversal checks work without JavaScript', async ({ page, app }, testInfo) => {
    const files = app.fixtures();
    const pdfBoard = uniqueShort('pdf', testInfo);
    const anyBoard = uniqueShort('any', testInfo);
    const smallBoard = uniqueShort('lim', testInfo);
    app.createBoardCli({ short: pdfBoard, name: 'PDF NoJS' });
    app.createBoardCli({ short: anyBoard, name: 'Any NoJS' });
    app.createBoardCli({ short: smallBoard, name: 'Small Uploads' });
    setBoardFixtureSettings(app, pdfBoard, { allowPdf: true });
    setBoardFixtureSettings(app, anyBoard, { allowAnyFiles: true });
    setBoardFixtureSettings(app, smallBoard, { maxImageSizeBytes: 128 });

    await createThreadNoJs(page, app.baseURL, 'img', { subject: 'image', body: 'image body', filePath: files.tinyPng });
    const imageHref = await mediaHref(page, 'tiny.png');
    expect(imageHref).toMatch(/^\/boards\/img\//);
    await expect(page.locator('[data-media-thumb="1"], .media-preview').first()).toBeVisible();
    const imageResponse = await page.request.get(`${app.baseURL}${imageHref}`);
    expect(imageResponse.status()).toBe(200);
    expect(imageResponse.headers()['x-content-type-options']).toBe('nosniff');

    await createThreadNoJs(page, app.baseURL, 'vid', { subject: 'video', body: 'video body', filePath: files.fakeMp4 });
    await expect(page.locator('.video-container, .file-container')).toContainText('tiny.mp4');
    await createThreadNoJs(page, app.baseURL, 'aud', { subject: 'audio', body: 'audio body', filePath: files.fakeOgg });
    await expect(page.locator('.audio-container, .file-container')).toContainText('tiny.ogg');
    await createThreadNoJs(page, app.baseURL, pdfBoard, { subject: 'pdf', body: 'pdf body', filePath: files.tinyPdf });
    await expect(page.locator('.pdf-container, .file-container')).toContainText('tiny.pdf');
    const pdfHref = await mediaHref(page, 'tiny.pdf');
    const pdfResponse = await page.request.get(`${app.baseURL}${pdfHref}`);
    expect(pdfResponse.status()).toBe(200);
    expect(pdfResponse.headers()['content-type']).toMatch(/application\/pdf|octet-stream/i);
    expect(pdfResponse.headers()['x-content-type-options']).toBe('nosniff');

    await createThreadNoJs(page, app.baseURL, anyBoard, { subject: 'any file', body: 'any body', filePath: files.invalid });
    await expect(page.locator('.file-download, .file-container').first()).toContainText('invalid.txt');
    const anyHref = await mediaHref(page, 'invalid.txt');
    expect(anyHref).toMatch(new RegExp(`^/boards/${anyBoard}/`));

    await page.goto(`${app.baseURL}/txt`);
    const disabledUpload = await page.request.post(`${app.baseURL}/txt`, {
      multipart: {
        _csrf: await page.locator('input[name="_csrf"], #csrf_global').first().getAttribute('value') ?? '',
        submission_token: 'disabled-image-upload',
        body: 'disabled image',
        file: {
          name: 'tiny.png',
          mimeType: 'image/png',
          buffer: await fsp.readFile(files.tinyPng),
        },
      },
      maxRedirects: 0,
    });
    expect([400, 422]).toContain(disabledUpload.status());
    await expectSafeResponse(disabledUpload);

    await page.goto(`${app.baseURL}/img`);
    await submitThreadNoJs(page, 'img', { body: 'pdf rejected', filePath: files.tinyPdf });
    await expect(page.locator('.post-error-banner').first()).toContainText(/pdf uploads are disabled|only accepts|not allowed/i);

    await page.goto(`${app.baseURL}/${smallBoard}`);
    await submitThreadNoJs(page, smallBoard, { body: 'too large', filePath: files.oversized });
    await expect(page.locator('.post-error-banner, body').first()).toContainText(/too large|maximum/i);

    const traversal = await page.request.get(`${app.baseURL}/boards/img/../../settings.toml`);
    expect([400, 403, 404]).toContain(traversal.status());
    await expectSafeResponse(traversal);
  });

  test('fixture-driven board/site toggles, password boards, thread states, and defaults stay public-only', async ({ page, browser, app }, testInfo) => {
    setSiteFixtureSettings(app, {
      siteName: 'NoJS RustChan',
      siteSubtitle: 'server rendered access',
      defaultTheme: 'deep-orbit',
      homepageNewThreadBadgesEnabled: true,
      homepageNewReplyBadgesEnabled: true,
      threadNewReplyBadgesEnabled: true,
    });
    await app.restart();

    await page.goto(app.baseURL);
    await expect(page.locator('body')).toContainText('NoJS RustChan');
    await expect(page.locator('body')).toContainText('server rendered access');
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'deep-orbit');
    await expect(page.locator('.nsfw-badge').first()).toBeVisible();

    const viewBoard = uniqueShort('view', testInfo);
    const postBoard = uniqueShort('post', testInfo);
    app.createBoardCli({ short: viewBoard, name: 'View Password' });
    app.createBoardCli({ short: postBoard, name: 'Post Password' });
    const passwordHash = adminPasswordHash(app);
    setBoardFixtureSettings(app, viewBoard, {
      accessMode: 'view_password',
      accessPasswordHash: passwordHash,
    });
    setBoardFixtureSettings(app, postBoard, {
      accessMode: 'post_password',
      accessPasswordHash: passwordHash,
    });

    await page.goto(`${app.baseURL}/${viewBoard}`);
    await expect(page.locator('body')).toContainText(/password protected/i);
    await page.locator('input[name="password"]').fill('wrong-password');
    await page.getByRole('button', { name: /unlock board/i }).click();
    await expect(page.locator('body')).toContainText(/invalid|wrong|password/i);
    await page.locator('input[name="password"]').fill('AdminPass123!');
    await Promise.all([
      page.waitForURL(new RegExp(`/${viewBoard}`)),
      page.getByRole('button', { name: /unlock board/i }).click(),
    ]);
    await expectSafePage(page);

    const fresh = await browser.newContext({ javaScriptEnabled: false });
    const freshPage = await fresh.newPage();
    await freshPage.goto(`${app.baseURL}/${viewBoard}`);
    await expect(freshPage.locator('body')).toContainText(/password protected/i);
    await fresh.close();

    await page.goto(`${app.baseURL}/${postBoard}`);
    await expect(page.locator('#board-access-gate')).toContainText(/posting is password protected|unlock posting/i);
    const deniedPost = await page.request.post(`${app.baseURL}/${postBoard}`, {
      multipart: {
        _csrf: await page.locator('input[name="_csrf"], #csrf_global').first().getAttribute('value') ?? '',
        submission_token: 'post-password-denied',
        body: 'blocked post password',
      },
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(deniedPost.status());
    expect(deniedPost.headers()['location']).toContain(`/${postBoard}/unlock`);
    await page.goto(`${app.baseURL}/${postBoard}/unlock`);
    await page.locator('input[name="password"]').fill('AdminPass123!');
    await Promise.all([
      page.waitForURL(new RegExp(`/${postBoard}$`)),
      page.getByRole('button', { name: /unlock posting|unlock board/i }).click(),
    ]);
    await expect(page.locator('form[action="/' + postBoard + '"] textarea[name="body"]')).toBeVisible();
    await createThreadNoJs(page, app.baseURL, postBoard, { subject: 'post password ok', body: 'created after unlock' });

    const stateBoard = uniqueShort('state', testInfo);
    app.createBoardCli({ short: stateBoard, name: 'State Board' });
    setBoardFixtureSettings(app, stateBoard, { allowArchive: true, maxThreads: 1, maxArchivedThreads: 2 });
    const lockedThread = await createThreadNoJs(page, app.baseURL, stateBoard, { subject: 'locked thread', body: 'locked body' });
    setThreadFixtureState(app, lockedThread, { locked: true });
    await page.goto(`${app.baseURL}/${stateBoard}/thread/${lockedThread}`);
    await expect(page.locator('body')).toContainText(/locked/i);
    await expect(page.locator('form[action$="/thread/' + lockedThread + '"]')).toHaveCount(0);

    const archivedThread = await createThreadNoJs(page, app.baseURL, stateBoard, { subject: 'archived thread', body: 'archived body' });
    setThreadFixtureState(app, archivedThread, { locked: true, archived: true });
    await page.goto(`${app.baseURL}/${stateBoard}/archive`);
    await expectSafePage(page);
    await expect(page.locator('body')).toContainText('archived thread');
  });

  test('themes, no-JS preferences, activity read state, and degraded HTTP behavior are safe', async ({ page, browser, app }, testInfo) => {
    await page.goto(app.baseURL);
    const labels = await page.locator('.user-preferences-noscript-form button[name="theme"]').evaluateAll((nodes) =>
      nodes.map((node) => node.textContent?.trim() ?? '').filter(Boolean),
    );
    expect(labels.slice(0, 3)).toEqual(['Forest', 'Blue Sky', 'Deep Orbit']);

    for (const slug of ['forest', 'blue-sky', 'deep-orbit', 'terminal', 'dorfic', 'chanclassic', 'aero', 'neoncubicle', 'fluorogrid']) {
      await page.goto(`${app.baseURL}/theme/${slug}?return_to=/pub`);
      await expect(page).toHaveURL(/\/pub$/);
      await expect(page.locator('html')).toHaveAttribute('data-active-theme', slug);
      const css = await page.request.get(`${app.baseURL}/theme-css/${slug}`);
      expect(css.status()).toBe(200);
      expect(css.headers()['content-type']).toMatch(/text\/css/i);
    }

    await page.goto(`${app.baseURL}/pub`);
    await page.locator('.user-preferences-summary').click();
    const noJsPreferences = page.locator('.user-preferences-noscript');
    await expect(noJsPreferences).toBeVisible();
    await Promise.all([
      page.waitForResponse((response) => response.url() === `${app.baseURL}/preferences`
        && response.request().method() === 'POST'),
      noJsPreferences.locator('button[name="theme"][value="forest"]').click(),
    ]);
    await expect(page).toHaveURL(/\/pub$/);
    await page.locator('.user-preferences-summary').click();
    await Promise.all([
      page.waitForResponse((response) => response.url() === `${app.baseURL}/preferences`
        && response.request().method() === 'POST'),
      page.locator('.user-preferences-noscript button[name="show_activity_badges"][value="0"]').click(),
    ]);
    await expect(page).toHaveURL(/\/pub$/);
    const cookies = await page.context().cookies();
    expect(cookies.find((cookie) => cookie.name === 'rustchan_activity_badges')?.value).toBe('0');
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');

    const activityBoard = uniqueShort('act', testInfo);
    app.createBoardCli({ short: activityBoard, name: 'Activity Board' });
    setBoardFixtureSettings(app, activityBoard, { postCooldownSecs: 0 });
    await createThreadNoJs(page, app.baseURL, activityBoard, { subject: 'activity baseline', body: 'baseline activity' });
    const reader = await browser.newContext({ javaScriptEnabled: false });
    const readerPage = await reader.newPage();
    await readerPage.goto(`${app.baseURL}/${activityBoard}`);
    await expectSafePage(readerPage);

    const writer = await browser.newContext({ javaScriptEnabled: false });
    const writerPage = await writer.newPage();
    await writerPage.goto(`${app.baseURL}/${activityBoard}`);
    await createThreadNoJs(writerPage, app.baseURL, activityBoard, { subject: 'activity appears', body: 'new activity' });
    await writer.close();

    await readerPage.goto(app.baseURL);
    await expect(readerPage.locator('.board-card-new-thread-badge').first()).toContainText(/1 New Threads/i);
    await readerPage.goto(`${app.baseURL}/${activityBoard}`);
    await readerPage.goto(app.baseURL);
    await expect(readerPage.locator('.board-card-new-thread-badge')).toHaveCount(0);
    await reader.close();

    const csrfFailure = await page.request.post(`${app.baseURL}/pub`, {
      multipart: {
        submission_token: 'missing-csrf',
        body: 'csrf should fail',
      },
      maxRedirects: 0,
    });
    expect(csrfFailure.status()).toBe(403);
    await expectSafeResponse(csrfFailure);

    const directPostGet = await page.request.get(`${app.baseURL}/pub/post/1/delete`, { maxRedirects: 0 });
    expect([403, 404]).toContain(directPostGet.status());
    await expectSafeResponse(directPostGet);

    const updates = await page.request.get(`${app.baseURL}/pub/thread/1/updates?since=0`, { maxRedirects: 0 });
    expect([200, 404]).toContain(updates.status());
    await expectSafeResponse(updates);
  });
});

async function createThreadNoJs(
  page: Page,
  baseURL: string,
  board: string,
  options: { name?: string; subject?: string; body?: string; filePath?: string } = {},
): Promise<number> {
  await page.goto(`${baseURL}/${board}`);
  await submitThreadNoJs(page, board, options);
  await page.waitForURL(new RegExp(`/${board}/thread/\\d+`));
  await expectSafePage(page);
  return threadIdFromUrl(page.url());
}

async function submitThreadNoJs(
  page: Page,
  board: string,
  options: { name?: string; subject?: string; body?: string; filePath?: string } = {},
): Promise<void> {
  const form = page.locator(`form[action="/${board}"]`).first();
  await expect(form.locator('textarea[name="body"]')).toBeVisible();
  if (options.name !== undefined) {
    await form.locator('input[name="name"]').fill(options.name);
  }
  if (options.subject !== undefined) {
    await form.locator('input[name="subject"]').fill(options.subject);
  }
  await form.locator('textarea[name="body"]').fill(options.body ?? `thread body ${Date.now()}`);
  if (options.filePath) {
    await fsp.access(options.filePath);
    await form.locator('input[type="file"]').first().setInputFiles(options.filePath);
  }
  await form.getByRole('button', { name: /post thread/i }).click();
}

async function createReplyNoJs(
  page: Page,
  baseURL: string,
  board: string,
  threadId: number,
  options: { name?: string; body?: string; filePath?: string } = {},
): Promise<void> {
  await page.goto(`${baseURL}/${board}/thread/${threadId}`);
  await expectSafePage(page);
  const form = page.locator(`form[action="/${board}/thread/${threadId}"]`).first();
  await expect(form.locator('textarea[name="body"]')).toBeVisible();
  if (options.name !== undefined) {
    await form.locator('input[name="name"]').fill(options.name);
  }
  await form.locator('textarea[name="body"]').fill(options.body ?? `reply body ${Date.now()}`);
  if (options.filePath) {
    await fsp.access(options.filePath);
    await form.locator('input[type="file"]').first().setInputFiles(options.filePath);
  }
  await Promise.all([
    page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
    form.getByRole('button', { name: /post reply/i }).click(),
  ]);
  await expectSafePage(page);
}

async function postId(locator: ReturnType<Page['locator']>): Promise<number> {
  const id = await locator.getAttribute('id');
  const numeric = Number(id?.replace(/^p/, ''));
  if (!Number.isInteger(numeric) || numeric <= 0) {
    throw new Error(`invalid post id: ${id}`);
  }
  return numeric;
}

async function mediaHref(page: Page, fileName: string): Promise<string> {
  const href = await page.locator(`.file-info a[title="${fileName}"]`).first().getAttribute('href');
  if (!href) {
    throw new Error(`media link for ${fileName} not found`);
  }
  return href;
}
