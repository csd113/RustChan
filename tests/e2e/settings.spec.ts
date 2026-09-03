import {
  adminCsrf,
  adminLogin,
  createReply,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  publicCsrf,
  setThreadFixtureState,
  sqliteExec,
  sqliteQuery,
  test,
  uniqueShort,
  updateBoardSettings,
} from './helpers';

test.describe('settings, catalog, archive, search, and maintenance', () => {
  test('site settings persist to settings.toml and survive restart', async ({ page, app }) => {
    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);
    const response = await page.request.post(`${app.baseURL}/admin/site/settings`, {
      form: {
        _csrf: csrf,
        site_name: 'RustChan E2E',
        site_subtitle: 'regression suite subtitle',
        default_theme: 'blue-sky',
        homepage_new_thread_badges_enabled: '1',
        homepage_new_reply_badges_enabled: '1',
        thread_new_reply_badges_enabled: '1',
        banner_rotation_interval_minutes: '5',
      },
      maxRedirects: 0,
    });
    expect(response.status()).toBe(303);
    await app.restart();
    await page.goto(app.baseURL);
    await expect(page.locator('body')).toContainText('RustChan E2E');
    await expect(page.locator('body')).toContainText('regression suite subtitle');
    await expectSafePage(page);
  });

  test('search and catalog handle unicode, punctuation, empty, long, and SQL-ish queries safely', async ({ page, app }) => {
    const threadId = await createThread(page, app, 'pub', {
      subject: 'search catalog',
      body: 'needle unicode café #tag "quotes" SQL-ish \'; DROP TABLE posts; --',
    });
    await page.goto(`${app.baseURL}/pub/catalog`);
    await expectSafePage(page);
    await expect(page.locator(`a[href="/pub/thread/${threadId}"]`).first()).toBeVisible();

    for (const query of ['needle', 'café', '', 'x'.repeat(300), '\'; DROP TABLE posts; --', '#tag "quotes"']) {
      await page.goto(`${app.baseURL}/pub/search?q=${encodeURIComponent(query)}`);
      await expectSafePage(page);
      await expect(page.locator('body')).not.toContainText(/SQLITE_|syntax error|stack backtrace/i);
    }
  });

  test('bump/archive behavior and maintenance controls are admin-only and CSRF protected', async ({ page, app }, testInfo) => {
    const board = uniqueShort('arch', testInfo);
    app.createBoardCli({ short: board, name: 'Archive Board' });
    await updateBoardSettings(page, app, board, {
      allowArchive: true,
      bumpLimit: 1,
      maxThreads: 3,
      maxArchivedThreads: 5,
    });
    const threadId = await createThread(page, app, board, { subject: 'archive target', body: 'op' });
    await createReply(page, app, board, threadId, 'reply one');
    await createReply(page, app, board, threadId, 'reply two after bump limit');

    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);
    const archive = await page.request.post(`${app.baseURL}/admin/thread/action`, {
      form: {
        _csrf: csrf,
        thread_id: String(threadId),
        board,
        action: 'archive',
      },
      maxRedirects: 0,
    });
    expect(archive.status()).toBe(303);
    await page.goto(`${app.baseURL}/${board}/archive`);
    await expectSafePage(page);
    await expect(page.locator('body')).toContainText('archive target');

    const replyRejected = await page.request.post(`${app.baseURL}/${board}/thread/${threadId}`, {
      multipart: {
        _csrf: await page.locator('input[name="_csrf"]').first().getAttribute('value') ?? '',
        submission_token: 'archived-reply',
        body: 'should reject archived thread',
      },
      maxRedirects: 0,
    });
    expect([403, 404, 409, 422]).toContain(replyRejected.status());

    await page.context().clearCookies();
    const publicRepair = await page.request.get(`${app.baseURL}/admin/db/repair`, { maxRedirects: 0 });
    expect([302, 303, 403]).toContain(publicRepair.status());
    await adminLogin(page, app);
    const repairCsrf = await adminCsrf(page, app);
    const missingCsrf = await page.request.post(`${app.baseURL}/admin/db/repair`, {
      form: {},
      maxRedirects: 0,
    });
    expect(missingCsrf.status()).toBe(403);
    await expectSafeResponse(missingCsrf);
    const repair = await page.request.post(`${app.baseURL}/admin/db/repair`, {
      form: { _csrf: repairCsrf },
      maxRedirects: 0,
    });
    expect([200, 303]).toContain(repair.status());
  });

  test('thread sage, bump limits, sticky, lock, archive, and ban denials stay deterministic', async ({ page, app }, testInfo) => {
    const board = uniqueShort('life', testInfo);
    app.createBoardCli({ short: board, name: 'Lifecycle Board' });
    await updateBoardSettings(page, app, board, {
      allowArchive: true,
      bumpLimit: 2,
      maxThreads: 5,
      maxArchivedThreads: 5,
    });
    await page.context().clearCookies();

    const sageThread = await createThread(page, app, board, {
      subject: 'sage target',
      body: 'sage target body',
    });
    setThreadFixtureState(app, sageThread, { bumpedAt: 1000 });
    const sageCsrf = await publicCsrf(page, app, `/${board}/thread/${sageThread}`);
    const sageReply = await page.request.post(`${app.baseURL}/${board}/thread/${sageThread}`, {
      multipart: {
        _csrf: sageCsrf,
        submission_token: 'sage-reply',
        body: 'sage reply does not bump',
        sage: '1',
      },
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(sageReply.status());
    expect(sqliteQuery(app, `SELECT bumped_at FROM threads WHERE id = ${sageThread};`)).toBe('1000');

    const regularCsrf = await publicCsrf(page, app, `/${board}/thread/${sageThread}`);
    const regularReply = await page.request.post(`${app.baseURL}/${board}/thread/${sageThread}`, {
      multipart: {
        _csrf: regularCsrf,
        submission_token: 'regular-bump',
        body: 'regular reply bumps',
      },
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(regularReply.status());
    const bumpedAfterRegular = Number(sqliteQuery(app, `SELECT bumped_at FROM threads WHERE id = ${sageThread};`));
    expect(bumpedAfterRegular).toBeGreaterThan(1000);

    const overLimitCsrf = await publicCsrf(page, app, `/${board}/thread/${sageThread}`);
    const overLimitReply = await page.request.post(`${app.baseURL}/${board}/thread/${sageThread}`, {
      multipart: {
        _csrf: overLimitCsrf,
        submission_token: 'over-bump-limit',
        body: 'reply after bump limit does not bump',
      },
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(overLimitReply.status());
    expect(Number(sqliteQuery(app, `SELECT bumped_at FROM threads WHERE id = ${sageThread};`))).toBe(bumpedAfterRegular);

    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);
    const sticky = await page.request.post(`${app.baseURL}/admin/thread/action`, {
      form: {
        _csrf: csrf,
        thread_id: String(sageThread),
        board,
        action: 'sticky',
      },
      maxRedirects: 0,
    });
    expect(sticky.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT sticky FROM threads WHERE id = ${sageThread};`)).toBe('1');

    const lock = await page.request.post(`${app.baseURL}/admin/thread/action`, {
      form: {
        _csrf: await adminCsrf(page, app),
        thread_id: String(sageThread),
        board,
        action: 'lock',
      },
      maxRedirects: 0,
    });
    expect(lock.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT locked FROM threads WHERE id = ${sageThread};`)).toBe('1');

    await page.context().clearCookies();
    await page.goto(`${app.baseURL}/${board}/thread/${sageThread}`);
    await expect(page.locator('body')).toContainText(/locked/i);
    await expect(page.locator(`form[action="/${board}/thread/${sageThread}"]`)).toHaveCount(0);
    const lockedCsrf = await page.locator('input[name="_csrf"], #csrf_global').first().getAttribute('value');
    const lockedReply = await page.request.post(`${app.baseURL}/${board}/thread/${sageThread}`, {
      multipart: {
        _csrf: lockedCsrf ?? '',
        submission_token: 'locked-reply',
        body: 'locked reply denied',
      },
      maxRedirects: 0,
    });
    expect([403, 409, 422]).toContain(lockedReply.status());
    await expectSafeResponse(lockedReply);

    const archivedThread = await createThread(page, app, board, {
      subject: 'archive denied target',
      body: 'archive denied body',
    });
    await adminLogin(page, app);
    const archive = await page.request.post(`${app.baseURL}/admin/thread/action`, {
      form: {
        _csrf: await adminCsrf(page, app),
        thread_id: String(archivedThread),
        board,
        action: 'archive',
      },
      maxRedirects: 0,
    });
    expect(archive.status()).toBe(303);
    await page.goto(`${app.baseURL}/${board}/archive`);
    await expect(page.locator('body')).toContainText('archive denied target');

    await page.context().clearCookies();
    const bannedThread = await createThread(page, app, board, {
      subject: 'ban source',
      body: 'ban source body',
    });
    const ipHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE thread_id = ${bannedThread} AND is_op = 1 LIMIT 1;`);
    expect(ipHash).toMatch(/^[0-9a-f]{64}$/i);
    sqliteExec(app, `INSERT INTO bans (ip_hash, reason) VALUES ('${ipHash}', 'release-blocking ban');`);
    const bannedCsrf = await publicCsrf(page, app, `/${board}`);
    const bannedPost = await page.request.post(`${app.baseURL}/${board}`, {
      multipart: {
        _csrf: bannedCsrf,
        submission_token: 'banned-thread',
        subject: 'banned should fail',
        body: 'banned should fail',
      },
      maxRedirects: 0,
    });
    expect(bannedPost.status()).toBe(403);
    const bannedBody = await expectSafeResponse(bannedPost);
    expect(bannedBody).toContain('release-blocking ban');
  });
});
