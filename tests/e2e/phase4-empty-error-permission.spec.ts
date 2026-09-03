import {
  adminCsrf,
  adminLogin,
  createReply,
  createThread,
  expect,
  publicCsrf,
  setThreadFixtureState,
  sqliteExec,
  sqliteQuery,
  test,
  uniqueShort,
  updateBoardSettings,
} from './helpers';
import {
  expectNoHorizontalOverflow,
  expectSafeBody,
  expectSafeHeaders,
  expectSafeHtmlResponse,
  expectUsableTarget,
  phase4SkipUnless,
} from './phase4-helpers';
import type { Page } from '@playwright/test';

test.describe('phase 4 empty, error, and permission states', () => {
  test.beforeEach(async ({}, testInfo) => {
    phase4SkipUnless(
      testInfo,
      ['chromium', 'firefox-nojs', 'mobile-webkit'],
      'phase 4 error-state coverage runs on Chromium, no-JS Firefox, and mobile WebKit',
    );
  });

  test('standalone 404, 400-ish, 403, locked-board, and banned pages stay clear and safe', async ({ page, app }, testInfo) => {
    const missing = await page.request.get(`${app.baseURL}/definitely-missing-phase4`, { maxRedirects: 0 });
    expect(missing.status()).toBe(404);
    await expectSafeHeaders(missing, '404');
    const missingText = await expectSafeHtmlResponse(missing, '404');
    expect(missingText).toContain('error 404');
    expect(missingText).toContain('return home');

    await page.goto(`${app.baseURL}/definitely-missing-phase4`);
    await expectSafeBody(page, '404 page');
    await expect(page.getByRole('link', { name: /return home|home/i }).first()).toBeVisible();
    await expectNoHorizontalOverflow(page, '404 page');

    const badThread = await page.request.get(`${app.baseURL}/pub/thread/not-a-number`, { maxRedirects: 0 });
    expect([400, 404]).toContain(badThread.status());
    await expectSafeHeaders(badThread, 'bad thread route');
    await expectSafeHtmlResponse(badThread, 'bad thread route');

    const protectedBoard = uniqueShort('lock', testInfo);
    app.createBoardCli({
      short: protectedBoard,
      name: 'Locked Phase Four Board',
      description: 'Permission page coverage',
    });
    await updateBoardSettings(page, app, protectedBoard, {
      accessMode: 'view_password',
      accessPassword: 'phase4-pass',
      allowImages: true,
    });
    await page.context().clearCookies();

    const lockedCatalog = await page.request.get(`${app.baseURL}/${protectedBoard}/catalog`, { maxRedirects: 0 });
    expect(lockedCatalog.status()).toBe(403);
    await expectSafeHeaders(lockedCatalog, 'locked board catalog');
    const lockedHtml = await expectSafeHtmlResponse(lockedCatalog, 'locked board catalog');
    expect(lockedHtml).toContain('password protected board');
    expect(lockedHtml).toContain('return home');

    await page.goto(`${app.baseURL}/${protectedBoard}/catalog`);
    await expectSafeBody(page, 'locked board page');
    await expectUsableTarget(page.getByLabel('board password'), 'locked board password field', testInfo);
    await page.getByLabel('board password').fill('wrong-pass');
    await page.getByRole('button', { name: /unlock board/i }).click();
    await expect(page.locator('.post-error-banner')).toContainText(/incorrect|password|try/i);
    await expectSafeBody(page, 'locked board wrong password');

    await page.goto(`${app.baseURL}/banned?reason=phase4%20ban%20reason%20%2Ftmp%2Fsecret`);
    await expectSafeBody(page, 'banned page');
    await expect(page.getByRole('heading', { name: /you are banned/i })).toBeVisible();
    await expect(page.locator('body')).toContainText('phase4 ban reason /tmp/secret');
    await expect(page.getByRole('button', { name: /submit appeal/i })).toBeVisible();
  });

  test('posting, invalid upload, locked or archived thread, deleted post or thread, and active ban errors stay safe', async ({ page, app }, testInfo) => {
    const board = uniqueShort('err', testInfo);
    app.createBoardCli({ short: board, name: 'Error State Board', description: 'phase 4 error fixtures' });
    await updateBoardSettings(page, app, board, {
      allowImages: true,
      allowEditing: true,
      allowSelfDelete: true,
      allowArchive: true,
      postCooldownSecs: 0,
    });

    await page.goto(`${app.baseURL}/${board}`);
    await openPostForm(page);
    const invalidUploadForm = page.locator(`form[action="/${board}"]`).first();
    await invalidUploadForm.getByLabel('subject').fill('invalid upload');
    await invalidUploadForm.getByLabel('body').fill('invalid upload body');
    await invalidUploadForm.locator('input[type="file"]').setInputFiles(app.fixtures().invalid);
    await invalidUploadForm.getByRole('button', { name: /post thread/i }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/file|media|type|invalid|accepted/i);
    await expectSafeBody(page, 'invalid upload state');

    const threadId = await createThread(page, app, board, {
      subject: 'thread state errors',
      body: 'thread that will be locked and archived',
    });
    await createReply(page, app, board, threadId, 'reply before closure');

    setThreadFixtureState(app, threadId, { locked: true });
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    await expect(page.locator('body')).toContainText(/locked/i);
    const lockedReply = await postReplyDirect(page, app.baseURL, board, threadId, 'reply rejected by locked thread');
    expect([403, 422]).toContain(lockedReply.status());
    await expectSafeHtmlResponse(lockedReply, 'locked thread reply');

    setThreadFixtureState(app, threadId, { archived: true });
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    await expect(page.locator('body')).toContainText(/archived|locked/i);
    const archivedReply = await postReplyDirect(page, app.baseURL, board, threadId, 'reply rejected by archived thread');
    expect([403, 422]).toContain(archivedReply.status());
    await expectSafeHtmlResponse(archivedReply, 'archived thread reply');

    const deletedPostThread = await createThread(page, app, board, {
      subject: 'deleted post fixture',
      body: 'post row will disappear',
    });
    const deletedPostId = await firstVisiblePostId(page);
    sqliteExec(app, [
      `DELETE FROM post_submissions WHERE post_id = ${deletedPostId};`,
      `DELETE FROM posts WHERE id = ${deletedPostId};`,
    ].join('\n'));
    const deletedPost = await page.request.get(`${app.baseURL}/${board}/post/${deletedPostId}/edit`, { maxRedirects: 0 });
    expect([404, 410]).toContain(deletedPost.status());
    await expectSafeHtmlResponse(deletedPost, 'deleted post edit');

    sqliteExec(app, [
      `DELETE FROM post_submissions WHERE thread_id = ${deletedPostThread};`,
      `DELETE FROM posts WHERE thread_id = ${deletedPostThread};`,
      `DELETE FROM threads WHERE id = ${deletedPostThread};`,
    ].join('\n'));
    const deletedThread = await page.request.get(`${app.baseURL}/${board}/thread/${deletedPostThread}`, { maxRedirects: 0 });
    expect([404, 410]).toContain(deletedThread.status());
    await expectSafeHeaders(deletedThread, 'deleted thread');
    await expectSafeHtmlResponse(deletedThread, 'deleted thread');

    const bannedThread = await createThread(page, app, board, {
      subject: 'ban target',
      body: 'this browser will be banned before replying',
    });
    const ipHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE thread_id = ${bannedThread} ORDER BY id ASC LIMIT 1;`);
    sqliteExec(app, `INSERT INTO bans (ip_hash, reason, expires_at) VALUES ('${ipHash}', 'phase4 active ban', NULL);`);
    const bannedReply = await postReplyDirect(page, app.baseURL, board, bannedThread, 'banned reply should not land');
    expect([302, 303, 403]).toContain(bannedReply.status());
    if ([302, 303].includes(bannedReply.status())) {
      expect(bannedReply.headers().location ?? '').toContain('/banned');
      await page.goto(`${app.baseURL}${bannedReply.headers().location}`);
      await expect(page.locator('body')).toContainText(/you are banned|phase4 active ban/i);
    } else {
      await expectSafeHtmlResponse(bannedReply, 'banned reply');
    }
  });

  test('invalid admin forms and restore uploads fail closed with useful admin navigation', async ({ page, app }) => {
    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);

    const invalidBoard = await page.request.post(`${app.baseURL}/admin/board/create`, {
      form: {
        _csrf: csrf,
        short_name: '../bad',
        name: 'Invalid Admin Board',
        description: 'should fail closed',
      },
      maxRedirects: 0,
    });
    expect([400, 409, 422]).toContain(invalidBoard.status());
    await expectSafeHtmlResponse(invalidBoard, 'invalid admin board form');

    const invalidFullRestore = await page.request.post(`${app.baseURL}/admin/restore`, {
      multipart: {
        _csrf: csrf,
        backup_file: {
          name: 'phase4-not-a-full-backup.zip',
          mimeType: 'application/zip',
          buffer: Buffer.from('not a zip backup'),
        },
      },
      headers: { Origin: app.baseURL, Referer: `${app.baseURL}/admin/panel` },
      maxRedirects: 0,
    });
    expect([200, 303, 400]).toContain(invalidFullRestore.status());
    await expectSafeHtmlResponse(invalidFullRestore, 'invalid full restore');
    if (invalidFullRestore.status() === 303) {
      await page.goto(`${app.baseURL}${invalidFullRestore.headers().location ?? '/admin/panel'}`);
      await expectSafeBody(page, 'invalid full restore admin page');
      await expect(page.locator('body')).toContainText(/restore|invalid|zip|backup/i);
      await expect(page.getByRole('link', { name: /admin/i }).first()).toBeVisible();
    }

    const invalidBoardRestore = await page.request.post(`${app.baseURL}/admin/board/restore`, {
      multipart: {
        _csrf: await adminCsrf(page, app),
        backup_file: {
          name: 'phase4-not-a-board-backup.json',
          mimeType: 'application/json',
          buffer: Buffer.from('{"not":"a board backup"}'),
        },
      },
      headers: { Origin: app.baseURL, Referer: `${app.baseURL}/admin/panel` },
      maxRedirects: 0,
    });
    expect([200, 303, 400]).toContain(invalidBoardRestore.status());
    await expectSafeHtmlResponse(invalidBoardRestore, 'invalid board restore');
  });
});

async function openPostForm(page: Page): Promise<void> {
  const toggle = page.locator('[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
}

async function postReplyDirect(page: Page, baseURL: string, board: string, threadId: number, body: string) {
  const csrf = await publicCsrf(page, { baseURL } as Parameters<typeof publicCsrf>[1], `/${board}/thread/${threadId}`);
  return page.request.post(`${baseURL}/${board}/thread/${threadId}`, {
    multipart: {
      _csrf: csrf,
      submission_token: `phase4-${Date.now()}-${Math.random()}`,
      body,
    },
    maxRedirects: 0,
  });
}

async function firstVisiblePostId(page: Page): Promise<number> {
  const id = await page.locator('.post').first().getAttribute('id');
  const numeric = Number(id?.replace(/^p/, ''));
  expect(Number.isInteger(numeric) && numeric > 0).toBe(true);
  return numeric;
}
