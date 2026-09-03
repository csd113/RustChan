import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  RustChanServer,
  adminCsrf,
  adminLogin,
  createBoard,
  createReply,
  createThread,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  sqliteQuery,
  test,
  uniqueShort,
  unlockBoard,
  updateBoardSettings,
} from './helpers';
import {
  expectNoHorizontalOverflow,
  expectSafeBody,
  expectSafeHeaders,
  phase4SkipUnless,
  watchClientErrors,
} from './phase4-helpers';
import type { Page, TestInfo } from '@playwright/test';

test.describe('phase 4 high-volume and release-smoke journeys', () => {
  test('busy public, protected, admin, search, API, update, and pagination surfaces remain usable', async ({ page, app }, testInfo) => {
    phase4SkipUnless(testInfo, ['chromium'], 'high-volume browser stress runs once in Chromium');
    test.setTimeout(150_000);
    const assertNoClientErrors = watchClientErrors(page);
    const busy = uniqueShort('busy', testInfo);
    const protectedBoard = uniqueShort('prot', testInfo);

    app.createBoardCli({ short: busy, name: 'Busy Public Board', description: 'many threads and replies' });
    app.createBoardCli({ short: protectedBoard, name: 'Protected Busy Board', description: 'must not leak content' });
    setBoardFixtureSettings(app, busy, {
      allowImages: true,
      allowAudio: true,
      allowPdf: true,
      defaultTheme: 'fluorogrid',
      postCooldownSecs: 0,
      maxThreads: 200,
      maxArchivedThreads: 200,
    });
    setBoardFixtureSettings(app, protectedBoard, {
      allowImages: true,
      postCooldownSecs: 0,
      maxThreads: 200,
      maxArchivedThreads: 200,
    });

    for (let index = 0; index < 12; index += 1) {
      app.createBoardCli({
        short: uniqueShort(`b${index}`, testInfo),
        name: `Auxiliary Board ${index}`,
        description: `auxiliary board for home stress ${index}`,
      });
    }

    const threadIds: number[] = [];
    for (let index = 0; index < 14; index += 1) {
      const threadId = await createThread(page, app, busy, {
        subject: `busy thread ${index}`,
        body: `busy instance body ${index} ${'reply counter '.repeat(30)}`,
        filePath: index % 5 === 0 ? app.fixtures().tinyPng : undefined,
      });
      threadIds.push(threadId);
      for (let reply = 0; reply < (index === 0 ? 8 : 2); reply += 1) {
        await createReply(page, app, busy, threadId, `busy reply ${index}/${reply} ${'content '.repeat(25)}`);
      }
    }

    const protectedThread = await createThread(page, app, protectedBoard, {
      subject: 'protected phase4 sentinel',
      body: 'PROTECTED_PHASE4_SENTINEL should never leak to public pages',
      filePath: app.fixtures().tinyPng,
    });
    await updateBoardSettings(page, app, protectedBoard, {
      name: 'Protected Busy Board',
      description: 'must not leak content',
      accessMode: 'view_password',
      accessPassword: 'protected-pass',
      allowImages: true,
    });
    await page.context().clearCookies();

    await page.goto(app.baseURL);
    await expectSafePage(page);
    await expectNoHorizontalOverflow(page, 'busy home');
    await expect(page.locator('body')).not.toContainText('PROTECTED_PHASE4_SENTINEL');
    expect(await page.locator('.board-card').count()).toBeGreaterThanOrEqual(14);
    await expect(page.getByRole('link', { name: new RegExp(`/${busy}/`) })).toBeVisible();
    await expect(page.getByRole('link', { name: new RegExp(`/${protectedBoard}/`) })).toBeVisible();

    await page.goto(`${app.baseURL}/${busy}`);
    await expectSafeBody(page, 'busy board');
    await expect(page.locator('.pagination')).toContainText(/page 1 \/ 2/i);
    await expectNoHorizontalOverflow(page, 'busy board');
    await page.getByRole('link', { name: /\[next\]/i }).click();
    await expect(page).toHaveURL(new RegExp(`/${busy}\\?page=2`));
    await expectSafeBody(page, 'busy board page 2');

    await page.goto(`${app.baseURL}/${busy}/catalog`);
    await expectSafeBody(page, 'busy catalog');
    await expect(page.locator('.catalog-item')).toHaveCount(14);
    await expectNoHorizontalOverflow(page, 'busy catalog');

    await page.goto(`${app.baseURL}/${busy}/thread/${threadIds[0]}`);
    await expectSafeBody(page, 'busy thread');
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('8');
    const postId = Number((await page.locator('.post').first().getAttribute('id'))?.replace(/^p/, ''));
    expect(Number.isInteger(postId)).toBe(true);

    const api = await page.request.get(`${app.baseURL}/api/post/${busy}/${postId}`);
    expect(api.status()).toBe(200);
    await expectSafeHeaders(api, 'post preview API');
    expect(await api.text()).toContain('busy instance body');

    const updates = await page.request.get(`${app.baseURL}/${busy}/thread/${threadIds[0]}/updates?since=0`);
    expect(updates.status()).toBe(200);
    await expectSafeHeaders(updates, 'thread updates');
    expect(await updates.text()).toContain('reply_count');

    await page.goto(`${app.baseURL}/${busy}/search?q=busy`);
    await expectSafeBody(page, 'busy search');
    await expect(page.locator('body')).toContainText(/results|busy/i);
    await expect(page.locator('body')).not.toContainText('PROTECTED_PHASE4_SENTINEL');

    const deniedProtectedThread = await page.request.get(`${app.baseURL}/${protectedBoard}/thread/${protectedThread}`, { maxRedirects: 0 });
    expect(deniedProtectedThread.status()).toBe(403);
    expect(await deniedProtectedThread.text()).not.toContain('PROTECTED_PHASE4_SENTINEL');

    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel`);
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'busy admin dashboard');
    await expect(page.locator('body')).toContainText(/site health|boards|moderation/i);

    assertNoClientErrors();
  });

  test('fresh runtime can become a usable site, moderate content, back up, and restore', async ({ page }, testInfo) => {
    phase4SkipUnless(testInfo, ['chromium'], 'full release smoke runs once in Chromium');
    test.setTimeout(180_000);

    const app = await RustChanServer.create(undefined, { env: { CHAN_AUTO_FULL_BACKUP_COPIES: '10' } });
    try {
      app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
      await app.start();
      testInfo.annotations.push({ type: 'phase4-release-root', description: app.rootDir });

      await page.goto(app.baseURL);
      await expectSafePage(page);
      await adminLogin(page, app);

      const board = uniqueShort('rel', testInfo);
      await createBoard(page, app, { short: board, name: 'Release Smoke', description: 'fresh usable site' });
      await updateBoardSettings(page, app, board, {
        name: 'Release Smoke',
        description: 'fresh usable site configured through admin',
        allowImages: true,
        allowEditing: true,
        allowSelfDelete: true,
        allowArchive: true,
        defaultTheme: 'aero',
        postCooldownSecs: 0,
      });

      const threadId = await createThread(page, app, board, {
        subject: 'release smoke thread',
        body: 'fresh runtime thread body',
        filePath: app.fixtures().tinyPng,
      });
      await createReply(page, app, board, threadId, 'fresh runtime reply');
      await page.goto(`${app.baseURL}/${board}/catalog`);
      await expect(page.locator(`a[href="/${board}/thread/${threadId}"]`).first()).toBeVisible();
      await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
      await expect(page.locator('body')).toContainText('fresh runtime reply');

      const editButton = page.locator('.self-action-controls .edit-btn').first();
      await editButton.click();
      await page.getByLabel('edit post body').fill('fresh runtime thread body edited');
      await page.locator('#edit-modal-form').getByRole('button', { name: /save edit/i }).click();
      await expect(page.locator('body')).toContainText('fresh runtime thread body edited');

      const deleteThreadId = await createThread(page, app, board, {
        subject: 'release smoke self delete',
        body: 'delete this own OP',
      });
      const deletePostId = Number((await page.locator('.post').first().getAttribute('id'))?.replace(/^p/, ''));
      await page.locator('.self-action-controls .del-btn').first().click();
      await page.locator('#confirm-modal-continue').click();
      await expect.poll(() => sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE id = ${deletePostId};`)).toBe('0');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE id = ${deleteThreadId};`)).toBe('0');

      await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
      await page.locator('.post-controls .report-btn').first().click();
      await page.locator('#report-modal').getByLabel('reason').fill('release smoke report');
      await Promise.all([
        page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
        page.locator('#report-submit-btn').click(),
      ]);
      await adminLogin(page, app);
      await page.goto(`${app.baseURL}/admin/panel?open=reports#reports`);
      const reportRow = page.locator('#reports tbody tr').filter({ hasText: 'release smoke report' }).first();
      await expect(reportRow).toBeVisible();
      await reportRow.getByRole('button', { name: /resolve/i }).click();
      await expect(page.locator('body')).toContainText(/report resolved|no open reports/i);

      const backupRef = await createFullBackupSmoke(page, app);
      await restoreSavedFullBackupSmoke(page, app, backupRef);
      await app.restart();
      await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
      await expectSafePage(page);
      await expect(page.locator('body')).toContainText('fresh runtime thread body edited');
    } finally {
      if (testInfo.status !== testInfo.expectedStatus) {
        await testInfo.attach('phase4-release-smoke.log', {
          body: await app.logs(),
          contentType: 'text/plain',
        });
      }
      await app.dispose();
    }
  });

  test('no-JS public posting, upload, own edit or delete, report, unlock, and moderation smoke works', async ({ page, app }, testInfo) => {
    phase4SkipUnless(testInfo, ['firefox-nojs'], 'no-JS release smoke runs in the firefox-nojs project');
    const board = uniqueShort('njs', testInfo);
    app.createBoardCli({ short: board, name: 'No JS Release', description: 'no-js release smoke' });
    await updateBoardSettings(page, app, board, {
      name: 'No JS Release',
      description: 'no-js release smoke',
      accessMode: 'post_password',
      accessPassword: 'no-js-pass',
      allowImages: true,
      allowEditing: true,
      allowSelfDelete: true,
      postCooldownSecs: 0,
    });
    await page.context().clearCookies();

    await unlockBoard(page, app, board, 'no-js-pass');
    await page.goto(`${app.baseURL}/${board}`);
    const form = page.locator(`form[action="/${board}"]`).first();
    await form.getByLabel('subject').fill('no js release thread');
    await form.getByLabel('body').fill('created with scripts disabled');
    await form.getByLabel('upload').setInputFiles(app.fixtures().tinyPng);
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/\\d+`)),
      form.getByRole('button', { name: /post thread/i }).click(),
    ]);
    const threadId = Number(page.url().match(/\/thread\/(\d+)/)?.[1]);
    expect(Number.isInteger(threadId)).toBe(true);

    const replyForm = page.locator(`form[action="/${board}/thread/${threadId}"]`).first();
    await replyForm.getByLabel('body').fill('no-js reply');
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
      replyForm.getByRole('button', { name: /post reply/i }).click(),
    ]);
    await expect(page.locator('body')).toContainText('no-js reply');

    const postId = Number((await page.locator('.post').first().getAttribute('id'))?.replace(/^p/, ''));
    await page.goto(`${app.baseURL}/${board}/post/${postId}/edit`);
    await page.getByLabel('edit post body').fill('edited without scripts');
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
      page.getByRole('button', { name: /save edit/i }).click(),
    ]);
    await expect(page.locator('body')).toContainText('edited without scripts');

    await page.locator('.report-fallback-summary').first().click();
    await page.locator('.report-fallback-reason').first().fill('no-js release report');
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
      page.locator('.report-fallback-submit').first().click(),
    ]);

    const deleteThreadId = await createThread(page, app, board, {
      subject: 'no-js delete target',
      body: 'delete through no-js confirmation page',
    });
    const deletePostId = Number((await page.locator('.post').first().getAttribute('id'))?.replace(/^p/, ''));
    await page.goto(`${app.baseURL}/${board}/post/${deletePostId}/delete`);
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/catalog|/${board}`)),
      page.getByRole('button', { name: /delete post/i }).click(),
    ]);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE id = ${deleteThreadId};`)).toBe('0');

    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=reports#reports`);
    const reportRow = page.locator('#reports tbody tr').filter({ hasText: 'no-js release report' }).first();
    await expect(reportRow).toBeVisible();
    await reportRow.getByRole('button', { name: /resolve/i }).click();
    await expect(page.locator('body')).toContainText(/report resolved|no open reports/i);
  });
});

async function createFullBackupSmoke(page: Page, app: RustChanServer): Promise<string> {
  await adminLogin(page, app);
  const before = new Set(await listBackupRefs(app));
  const response = await page.request.post(`${app.baseURL}/admin/backup/create`, {
    form: {
      _csrf: await adminCsrf(page, app),
      storage_mode: 'directory',
    },
    headers: { Origin: app.baseURL, Referer: `${app.baseURL}/admin/panel` },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  return waitForNewBackupRef(app, before);
}

async function restoreSavedFullBackupSmoke(page: Page, app: RustChanServer, backupRef: string): Promise<void> {
  await adminLogin(page, app);
  const response = await page.request.post(`${app.baseURL}/admin/backup/restore-saved`, {
    form: {
      _csrf: await adminCsrf(page, app),
      filename: backupRef,
    },
    headers: { Origin: app.baseURL, Referer: `${app.baseURL}/admin/panel` },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  expect(response.headers().location ?? '').toContain('restored=1');
}

async function listBackupRefs(app: RustChanServer): Promise<string[]> {
  const root = path.join(app.dataDir, 'backups');
  const entries = await fsp.readdir(root, { withFileTypes: true }).catch(() => []);
  return entries
    .filter((entry) => entry.isDirectory() && fs.existsSync(path.join(root, entry.name, 'backup.json')))
    .map((entry) => entry.name)
    .sort();
}

async function waitForNewBackupRef(app: RustChanServer, before: Set<string>): Promise<string> {
  let latest: string | undefined;
  await expect.poll(async () => {
    const refs = await listBackupRefs(app);
    const created = refs.filter((ref) => !before.has(ref));
    latest = created.sort().at(-1);
    return created.length;
  }, { timeout: 15_000, intervals: [250, 500, 1_000, 2_000] }).toBeGreaterThan(0);
  if (!latest) {
    throw new Error('new backup was not found');
  }
  return latest;
}
