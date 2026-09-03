import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  adminPasswordHash,
  createReply,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  RustChanServer,
  setBoardFixtureSettings,
  sqliteQuery,
  test,
  uniqueShort,
  unlockBoard,
  updateBoardSettings,
} from './helpers';

test.describe('password-protected boards', () => {
  test('view-password boards require per-context unlock and resist back-forward bypasses', async ({ page, browser, app }, testInfo) => {
    const board = uniqueShort('view', testInfo);
    app.createBoardCli({ short: board, name: 'View Password' });
    const threadId = await createThread(page, app, board, {
      subject: 'view protected subject',
      body: 'view protected body',
    });
    await updateBoardSettings(page, app, board, {
      accessMode: 'view_password',
      accessPassword: 'view-secret',
    });
    await page.context().clearCookies();

    await page.goto(`${app.baseURL}/${board}`);
    await expect(page.locator('body')).toContainText(/password protected/i);
    await expect(page.locator('input[name="password"]')).toBeVisible();
    await page.locator('input[name="password"]').fill('wrong');
    await page.getByRole('button', { name: /unlock board/i }).click();
    await expect(page.locator('body')).toContainText(/invalid|wrong|password/i);
    await expectSafePage(page);

    await unlockBoard(page, app, board, 'view-secret');
    await expect(page).toHaveURL(new RegExp(`/${board}`));
    await expectSafePage(page);
    await page.goto(`${app.baseURL}/${board}`);
    await expect(page.locator('body')).toContainText('view protected body');
    await page.goto(`${app.baseURL}/${board}/catalog`);
    await expect(page.locator('body')).toContainText('view protected body');
    await page.locator(`a[href="/${board}/thread/${threadId}"]`).first().click();
    await expect(page).toHaveURL(new RegExp(`/${board}/thread/${threadId}`));
    await expect(page.locator('body')).toContainText('view protected body');
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.locator('body')).toContainText('view protected body');
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await expect(page.locator('body')).toContainText('view protected body');
    await page.goBack({ waitUntil: 'domcontentloaded', timeout: 5_000 }).catch(() => undefined);
    await page.goForward({ waitUntil: 'domcontentloaded', timeout: 5_000 }).catch(() => undefined);
    if (!page.url().includes(`/${board}`)) {
      await page.goto(`${app.baseURL}/${board}`);
    }
    await expectSafePage(page);

    const otherContext = await browser.newContext();
    const other = await otherContext.newPage();
    await other.goto(`${app.baseURL}/${board}`);
    await expect(other.locator('body')).toContainText(/password protected/i);
    await expect(other.locator('input[name="password"]')).toBeVisible();
    await otherContext.close();
  });

  test('post-password boards allow viewing but require password for threads and replies', async ({ page, app }, testInfo) => {
    const board = uniqueShort('post', testInfo);
    app.createBoardCli({ short: board, name: 'Post Password' });
    await updateBoardSettings(page, app, board, {
      accessMode: 'post_password',
      accessPassword: 'post-secret',
      allowEditing: true,
      allowSelfDelete: true,
    });
    await page.context().clearCookies();

    await page.goto(`${app.baseURL}/${board}`);
    await expectSafePage(page);
    await expect(page.locator('#board-access-gate')).toContainText(/posting is password protected|unlock posting/i);

    const denied = await page.request.post(`${app.baseURL}/${board}`, {
      multipart: {
        _csrf: await page.locator('input[name="_csrf"]').first().getAttribute('value') ?? '',
        submission_token: 'missing-password',
        body: 'should not post',
      },
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(denied.status());
    expect(denied.headers()['location']).toContain(`/${board}/unlock`);

    await unlockBoard(page, app, board, 'post-secret');
    const threadId = await createThread(page, app, board, {
      subject: 'post password ok',
      body: 'created after unlock',
    });
    const editHref = await page.locator('.self-action-controls .edit-btn').first().getAttribute('href');
    expect(editHref).toBeTruthy();
    const deleteOnlyThreadId = await createThread(page, app, board, {
      subject: 'post password delete gate',
      body: 'delete should require posting unlock',
    });
    const deleteHref = await page.locator('.self-action-controls .del-btn').first().getAttribute('href');
    expect(deleteHref).toBeTruthy();
    await createReply(page, app, board, threadId, 'reply after unlock');
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await expect(page.locator('#board-access-gate')).toHaveCount(0);

    await page.context().clearCookies({ name: `rustchan_board_access_${board}` });
    await page.goto(`${app.baseURL}${editHref}`);
    await expect(page.locator('body')).toContainText(/password|unlock/i);
    await expect(page.locator('body')).not.toContainText('created after unlock');
    await page.goto(`${app.baseURL}${deleteHref}`);
    await expect(page.locator('body')).toContainText(/password|unlock/i);
    await expect(page.locator('body')).not.toContainText('delete should require posting unlock');
    await unlockBoard(page, app, board, 'post-secret');
    await page.goto(`${app.baseURL}/${board}/thread/${deleteOnlyThreadId}`);
    await expect(page.locator('body')).toContainText('delete should require posting unlock');
  });

  test('view-password boards do not leak protected content before unlock', async ({ page, app }, testInfo) => {
    const board = uniqueShort('leak', testInfo);
    app.createBoardCli({ short: board, name: 'Leak Audit' });
    const threadId = await createThread(page, app, board, {
      subject: 'leak audit subject',
      body: 'leak audit body',
      filePath: app.fixtures().tinyPng,
    });
    const mediaHref = await page.locator('.post.op .file-info a').first().getAttribute('href');
    expect(mediaHref).toMatch(/^\/boards\//);
    expect(mediaHref).toContain(`/boards/${board}/`);
    const postId = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 1 LIMIT 1;`));
    expect(postId).toBeGreaterThan(0);

    await updateBoardSettings(page, app, board, {
      accessMode: 'view_password',
      accessPassword: 'leak-secret',
      allowArchive: true,
    });
    await page.context().clearCookies();

    const forbiddenHtml = [
      `/${board}`,
      `/${board}/catalog`,
      `/${board}/hidden`,
      `/${board}/archive`,
      `/${board}/search?q=leak`,
      `/${board}/thread/${threadId}`,
      `/${board}/thread/999999`,
    ];
    for (const path of forbiddenHtml) {
      const response = await page.request.get(`${app.baseURL}${path}`, { maxRedirects: 0 });
      expect(response.status(), path).toBe(403);
      const body = await expectSafeResponse(response);
      expect(body, path).not.toContain('leak audit body');
      expect(body, path).not.toContain('leak audit subject');
    }

    const homepage = await page.request.get(app.baseURL);
    expect(homepage.status()).toBe(200);
    const homepageBody = await expectSafeResponse(homepage);
    expect(homepageBody).not.toContain('leak audit body');
    expect(homepageBody).not.toContain('leak audit subject');

    const preview = await page.request.get(`${app.baseURL}/api/post/${board}/${postId}`);
    expect(preview.status()).toBe(404);
    const previewBody = await expectSafeResponse(preview);
    expect(previewBody).not.toContain('leak audit body');

    const postRedirect = await page.request.get(`${app.baseURL}/${board}/post/${postId}`, { maxRedirects: 0 });
    expect([302, 303]).toContain(postRedirect.status());
    expect(postRedirect.headers()['location']).toContain(`/${board}/unlock`);
    expect(await postRedirect.text()).not.toContain('leak audit body');

    const updates = await page.request.get(`${app.baseURL}/${board}/thread/${threadId}/updates?since=0`);
    expect(updates.status()).toBe(403);
    expect(await updates.text()).not.toContain('leak audit body');

    const mediaUrl = new URL(mediaHref, app.baseURL);
    mediaUrl.searchParams.set('access-check', String(postId));
    const media = await page.request.get(mediaUrl.toString());
    expect(media.status()).toBe(403);
    expect(await media.text()).not.toContain('leak audit body');
  });

  test('password changes retire the old password and accept the new one', async ({ page, app }, testInfo) => {
    const board = uniqueShort('pwchg', testInfo);
    app.createBoardCli({ short: board, name: 'Password Change' });
    await updateBoardSettings(page, app, board, {
      accessMode: 'view_password',
      accessPassword: 'old-secret',
    });
    await page.context().clearCookies();
    await unlockBoard(page, app, board, 'old-secret');

    await updateBoardSettings(page, app, board, {
      accessMode: 'view_password',
      accessPassword: 'new-secret',
    });
    await page.context().clearCookies();
    const fresh = await page.context().browser()!.newContext();
    const freshPage = await fresh.newPage();
    await freshPage.goto(`${app.baseURL}/${board}/unlock`);
    await freshPage.locator('input[name="password"]').fill('old-secret');
    await freshPage.getByRole('button', { name: /unlock board/i }).click();
    await expect(freshPage.locator('body')).toContainText(/invalid|wrong|password/i);
    await freshPage.locator('input[name="password"]').fill('new-secret');
    await Promise.all([
      freshPage.waitForURL(new RegExp(`/${board}`)),
      freshPage.getByRole('button', { name: /unlock board/i }).click(),
    ]);
    await expectSafePage(freshPage);
    await fresh.close();
  });

  test('mobile WebKit persists board unlock over HTTP localhost when HTTPS cookies are configured', async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'mobile-webkit', 'mobile WebKit cookie persistence regression');

    const secureCookieApp = await RustChanServer.create(undefined, {
      env: { CHAN_HTTPS_COOKIES: '1' },
    });
    const board = uniqueShort('mwview', testInfo);
    const postBoard = uniqueShort('mwpost', testInfo);
    const spoofBoard = uniqueShort('mwspuf', testInfo);
    try {
      secureCookieApp.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
      secureCookieApp.createBoardCli({ short: board, name: 'Mobile WebKit View Password' });
      secureCookieApp.createBoardCli({ short: postBoard, name: 'Mobile WebKit Post Password' });
      secureCookieApp.createBoardCli({ short: spoofBoard, name: 'Mobile WebKit Spoofed Headers' });
      setBoardFixtureSettings(secureCookieApp, board, {
        accessMode: 'view_password',
        accessPasswordHash: adminPasswordHash(secureCookieApp),
      });
      setBoardFixtureSettings(secureCookieApp, postBoard, {
        accessMode: 'post_password',
        accessPasswordHash: adminPasswordHash(secureCookieApp),
      });
      setBoardFixtureSettings(secureCookieApp, spoofBoard, {
        accessMode: 'view_password',
        accessPasswordHash: adminPasswordHash(secureCookieApp),
      });
      await secureCookieApp.start();

      await page.context().clearCookies();
      await page.goto(`${secureCookieApp.baseURL}/${board}/unlock`);
      await expect(page.locator('input[name="password"]')).toBeVisible();
      await page.locator('input[name="password"]').fill(ADMIN_PASSWORD);
      await Promise.all([
        page.waitForURL(new RegExp(`/${board}/catalog$`)),
        page.getByRole('button', { name: /unlock board/i }).click(),
      ]);
      await expect(page.locator('input[name="password"]')).toHaveCount(0);
      await expectSafePage(page);

      await page.reload({ waitUntil: 'domcontentloaded' });
      await expect(page.locator('input[name="password"]')).toHaveCount(0);
      await page.goto(`${secureCookieApp.baseURL}/${board}`);
      await expect(page.locator('input[name="password"]')).toHaveCount(0);

      const cookies = await page.context().cookies(secureCookieApp.baseURL);
      const unlockCookie = cookies.find((cookie) => cookie.name === `rustchan_board_access_${board}`);
      expect(unlockCookie).toBeTruthy();
      expect(unlockCookie?.httpOnly).toBe(true);
      expect(unlockCookie?.sameSite).toBe('Lax');
      expect(unlockCookie?.secure).toBe(false);

      await page.context().clearCookies();
      await page.goto(`${secureCookieApp.baseURL}/${postBoard}`);
      await expectSafePage(page);
      await expect(page.locator('#board-access-gate')).toContainText(/posting is password protected|unlock posting/i);
      await unlockBoard(page, secureCookieApp, postBoard, ADMIN_PASSWORD);
      const threadId = await createThread(page, secureCookieApp, postBoard, {
        subject: 'mobile post password ok',
        body: 'mobile thread after unlock',
      });
      await createReply(page, secureCookieApp, postBoard, threadId, 'mobile reply after unlock');
      await page.reload({ waitUntil: 'domcontentloaded' });
      await expect(page.locator('#board-access-gate')).toHaveCount(0);

      const postCookies = await page.context().cookies(secureCookieApp.baseURL);
      const postUnlockCookie = postCookies.find((cookie) => cookie.name === `rustchan_board_access_${postBoard}`);
      expect(postUnlockCookie).toBeTruthy();
      expect(postUnlockCookie?.httpOnly).toBe(true);
      expect(postUnlockCookie?.sameSite).toBe('Lax');
      expect(postUnlockCookie?.secure).toBe(false);

      await page.context().clearCookies();
      await page.goto(`${secureCookieApp.baseURL}/${spoofBoard}/unlock`);
      const csrf = await page.locator('input[name="_csrf"]').first().getAttribute('value');
      const spoofedUnlock = await page.request.post(`${secureCookieApp.baseURL}/${spoofBoard}/unlock`, {
        form: {
          _csrf: csrf ?? '',
          password: ADMIN_PASSWORD,
          return_to: `/${spoofBoard}`,
        },
        headers: {
          origin: `https://127.0.0.1:${secureCookieApp.port}`,
          referer: `https://127.0.0.1:${secureCookieApp.port}/${spoofBoard}/unlock`,
          'x-forwarded-proto': 'https',
        },
        maxRedirects: 0,
      });
      expect([302, 303]).toContain(spoofedUnlock.status());
      const accessSetCookie = spoofedUnlock
        .headersArray()
        .filter((header) => header.name.toLowerCase() === 'set-cookie')
        .map((header) => header.value)
        .find((value) => value.includes(`rustchan_board_access_${spoofBoard}=`));
      expect(accessSetCookie).toBeTruthy();
      expect(accessSetCookie).not.toMatch(/;\s*Secure/i);

      await page.goto(`${secureCookieApp.baseURL}/${spoofBoard}`);
      await expect(page.locator('input[name="password"]')).toHaveCount(0);
      await expectSafePage(page);
    } finally {
      await secureCookieApp.dispose();
    }
  });
});
