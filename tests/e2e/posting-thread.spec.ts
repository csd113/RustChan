import {
  createReply,
  createThread,
  expect,
  expectNoDialog,
  expectSafePage,
  expectSafeResponse,
  setPostFixtureCreatedAt,
  setThreadFixtureState,
  sqliteExec,
  sqliteQuery,
  test,
  uniqueShort,
  updateBoardSettings,
} from './helpers';
import type { Page } from '@playwright/test';

test.describe('logged-out navigation and posting', () => {
  test('home, board, catalog, thread, assets, theme, and back-forward navigation are stable', async ({ page, app }) => {
    await page.goto(app.baseURL);
    await expectSafePage(page);
    await expect(page.locator('.board-card-link[href="/pub/catalog"], .board-card-link[href="/pub"]').first()).toBeVisible();

    const css = await page.request.get(`${app.baseURL}/static/style.css`);
    expect(css.status()).toBe(200);
    const js = await page.request.get(`${app.baseURL}/static/main.js`);
    expect(js.status()).toBe(200);

    await page.goto(`${app.baseURL}/theme/blue-sky`);
    await page.goto(`${app.baseURL}/pub`);
    await expectSafePage(page);
    const threadId = await createThread(page, app, 'pub', {
      subject: 'Navigation thread',
      body: 'body for catalog and back-forward checks',
    });

    await page.goto(`${app.baseURL}/pub/catalog`);
    await expectSafePage(page);
    await expect(page.locator(`a[href="/pub/thread/${threadId}"]`).first()).toBeVisible();
    await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
    await expectSafePage(page);
    await page.goBack();
    await expect(page).toHaveURL(/\/pub\/catalog/);
    await expectSafePage(page);
    await page.goForward();
    await expect(page).toHaveURL(new RegExp(`/pub/thread/${threadId}`));
    await expectSafePage(page);
  });

  test('thread creation, reply, escaping, quote-ish text, unicode, and refresh do not duplicate posts', async ({ page, app }) => {
    const payload = '<script>alert(1)</script>\n>>123\nhttps://example.test/a?b=c\nlongword_' +
      'x'.repeat(80) + '\nemoji 😀 quotes "` and bidi \u202E';
    await expectNoDialog(page, async () => {
      const threadId = await createThread(page, app, 'pub', {
        subject: '<img src=x onerror=alert(1)>',
        body: payload,
      });
      await expect(page.locator('body')).toContainText('<script>alert(1)</script>');
      expect(await page.locator('script:text("alert(1)")').count()).toBe(0);
      await createReply(page, app, 'pub', threadId, 'reply with **bold** and __italic__');
      await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
      const postCount = await page.locator('.post').count();
      await page.reload();
      await expect(page.locator('.post')).toHaveCount(postCount);
    });
  });

  test('empty, oversized, duplicate, invalid id, and stale-form posting errors stay safe', async ({ page, app }) => {
    await page.goto(`${app.baseURL}/pub`);
    await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').click();
    const form = page.locator('form[action="/pub"]').first();
    await form.locator('textarea[name="body"]').fill('');
    await form.getByRole('button', { name: /post thread/i }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/body|empty|required|attached file/i);
    await expectSafePage(page);

    await page.goto(`${app.baseURL}/pub`);
    await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').click();
    const longForm = page.locator('form[action="/pub"]').first();
    const csrf = await longForm.locator('input[name="_csrf"]').getAttribute('value');
    const oversized = await page.request.post(`${app.baseURL}/pub`, {
      multipart: {
        _csrf: csrf ?? '',
        submission_token: `oversized-${Date.now()}`,
        body: 'x'.repeat(5000),
      },
      maxRedirects: 0,
    });
    expect([400, 422]).toContain(oversized.status());
    await expectSafeResponse(oversized);
    await expectSafePage(page);

    const threadId = await createThread(page, app, 'pub', { subject: 'duplicate guard', body: 'one submit' });
    const count = await page.locator('.post').count();
    await page.goBack({ waitUntil: 'domcontentloaded', timeout: 5_000 }).catch(() => undefined);
    await page.goForward({ waitUntil: 'domcontentloaded', timeout: 5_000 }).catch(() => undefined);
    await page.waitForURL(new RegExp(`/pub/thread/${threadId}`), { timeout: 2_000 }).catch(() => undefined);
    if (!page.url().includes(`/pub/thread/${threadId}`)) {
      await page.evaluate(() => window.stop()).catch(() => undefined);
      await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
    }
    await expect(page.locator('.post')).toHaveCount(count);

    const invalidBoard = await page.request.get(`${app.baseURL}/../../etc/passwd`);
    expect([400, 404]).toContain(invalidBoard.status());
    await expectSafeResponse(invalidBoard);
    const invalidThread = await page.request.get(`${app.baseURL}/pub/thread/999999999`);
    expect([404, 410]).toContain(invalidThread.status());
    await expectSafeResponse(invalidThread);

    await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
    const replyCsrf = await page.locator('form[action$="/thread/' + threadId + '"] input[name="_csrf"]').first().getAttribute('value');
    const response = await page.request.post(`${app.baseURL}/pub/thread/${threadId}`, {
      multipart: {
        _csrf: replyCsrf ?? '',
        submission_token: 'stale-reuse-check',
        body: 'stale direct reply',
      },
      maxRedirects: 0,
    });
    expect([303, 403, 422]).toContain(response.status());
  });

  test('own edit modal and delete confirmation are limited to the posting browser context', async ({ page, browser, app }, testInfo) => {
    test.skip(testInfo.project.name === 'firefox-nojs', 'scripted own-post modal flow; no-JS fallback is covered separately');

    const board = uniqueShort('ed', testInfo);
    app.createBoardCli({ short: board, name: 'Edit Delete' });
    await updateBoardSettings(page, app, board, {
      allowEditing: true,
      allowSelfDelete: true,
    });
    const threadId = await createThread(page, app, board, { subject: 'editable', body: 'original body' });
    const opPostId = await visiblePostId(page, '.post.op');
    const ownCookie = (await page.context().cookies(app.baseURL)).find((cookie) => cookie.name === 'rustchan_owned_posts');
    expect(ownCookie).toBeTruthy();
    expect(ownCookie?.httpOnly).toBe(true);
    expect(ownCookie?.sameSite).toBe('Lax');
    expect(ownCookie?.secure).toBe(false);

    const editHref = await page.locator('.self-action-controls .edit-btn').first().getAttribute('href');
    expect(editHref).toBe(`/${board}/post/${opPostId}/edit`);
    await page.locator(`#p${opPostId} .self-action-controls .edit-btn`).click();
    await expect(page.locator('#edit-modal.is-open')).toBeVisible();
    await page.locator('#edit-modal-body').fill('edited body through modal');
    await page.locator('#edit-modal-form').getByRole('button', { name: /save edit/i }).click();
    await expect(page.locator(`#p${opPostId}`)).toContainText('edited body through modal');

    const other = await browser.newPage();
    await other.goto(`${app.baseURL}${editHref}`);
    await expect(other.locator('body')).toContainText(/not allowed|expired|forbidden|edit/i);
    await other.close();

    const deleteThreadId = await createThread(page, app, board, {
      subject: 'delete through modal',
      body: 'delete target body',
    });
    const deletePostId = await visiblePostId(page, '.post.op');
    await page.locator(`#p${deletePostId} .self-action-controls .del-btn`).click();
    await expect(page.locator('#confirm-modal')).toBeVisible();
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/catalog|/${board}`)),
      page.locator('#confirm-modal-continue').click(),
    ]);
    await expectSafePage(page);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE id = ${deleteThreadId};`)).toBe('0');
  });

  test('own-post controls deny expired, replied, locked, archived, and deleted posts safely', async ({ page, app }, testInfo) => {
    const board = uniqueShort('deny', testInfo);
    app.createBoardCli({ short: board, name: 'Own Post Denials' });
    await updateBoardSettings(page, app, board, {
      allowEditing: true,
      allowSelfDelete: true,
    });

    const repliedThread = await createThread(page, app, board, {
      subject: 'op with replies',
      body: 'starter should not self-delete after replies',
    });
    const repliedOpId = await visiblePostId(page, '.post.op');
    await createReply(page, app, board, repliedThread, 'reply blocks OP self-delete');
    await page.goto(`${app.baseURL}/${board}/post/${repliedOpId}/delete`);
    await page.getByRole('button', { name: /delete post/i }).click();
    await expect(page.locator('body')).toContainText(/thread starter before anyone replies/i);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE id = ${repliedOpId};`)).toBe('1');

    const lockedThread = await createThread(page, app, board, {
      subject: 'locked own-post',
      body: 'locked action denied',
    });
    const lockedPostId = await visiblePostId(page, '.post.op');
    const lockedEditHref = await page.locator(`#p${lockedPostId} .self-action-controls .edit-btn`).getAttribute('href');
    const lockedDeleteHref = await page.locator(`#p${lockedPostId} .self-action-controls .del-btn`).getAttribute('href');
    expect(lockedEditHref).toBeTruthy();
    expect(lockedDeleteHref).toBeTruthy();
    setThreadFixtureState(app, lockedThread, { locked: true });
    await page.goto(`${app.baseURL}${lockedEditHref}`);
    await expect(page.locator('body')).toContainText(/locked or archived/i);
    await page.goto(`${app.baseURL}${lockedDeleteHref}`);
    await expect(page.locator('body')).toContainText(/locked or archived/i);

    const archivedThread = await createThread(page, app, board, {
      subject: 'archived own-post',
      body: 'archived action denied',
    });
    const archivedPostId = await visiblePostId(page, '.post.op');
    const archivedEditHref = await page.locator(`#p${archivedPostId} .self-action-controls .edit-btn`).getAttribute('href');
    expect(archivedEditHref).toBeTruthy();
    setThreadFixtureState(app, archivedThread, { archived: true });
    await page.goto(`${app.baseURL}${archivedEditHref}`);
    await expect(page.locator('body')).toContainText(/locked or archived/i);

    const expiredThread = await createThread(page, app, board, {
      subject: 'expired own-post',
      body: 'expired action denied',
    });
    const expiredPostId = await visiblePostId(page, '.post.op');
    setPostFixtureCreatedAt(app, expiredPostId, Math.floor(Date.now() / 1000) - 120);
    await page.goto(`${app.baseURL}/${board}/post/${expiredPostId}/edit`);
    await expect(page.locator('body')).toContainText(/edit window|closed|forbidden/i);
    await page.goto(`${app.baseURL}/${board}/post/${expiredPostId}/delete`);
    await expect(page.locator('body')).toContainText(/self-delete window|closed|forbidden/i);

    const deletedThread = await createThread(page, app, board, {
      subject: 'deleted own-post',
      body: 'deleted action denied',
    });
    const deletedPostId = await visiblePostId(page, '.post.op');
    const deletedEditHref = await page.locator(`#p${deletedPostId} .self-action-controls .edit-btn`).getAttribute('href');
    expect(deletedEditHref).toBeTruthy();
    sqliteExec(app, [
      `DELETE FROM post_submissions WHERE thread_id = ${deletedThread};`,
      `DELETE FROM posts WHERE thread_id = ${deletedThread};`,
      `DELETE FROM threads WHERE id = ${deletedThread};`,
    ].join('\n'));
    const deletedEdit = await page.request.get(`${app.baseURL}${deletedEditHref}`, { maxRedirects: 0 });
    expect([404, 410]).toContain(deletedEdit.status());
    await expectSafeResponse(deletedEdit);
  });
});

async function visiblePostId(page: Page, selector: string): Promise<number> {
  const id = await page.locator(selector).first().getAttribute('id');
  const numeric = Number(id?.replace(/^p/, ''));
  expect(Number.isInteger(numeric) && numeric > 0, `post id for ${selector}`).toBe(true);
  return numeric;
}
