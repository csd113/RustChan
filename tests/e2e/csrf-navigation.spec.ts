import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  adminCsrf,
  adminLogin,
  adminLogout,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  publicCsrf,
  test,
  uniqueShort,
} from './helpers';

test.describe('CSRF, Origin, Referer, and browser navigation regressions', () => {
  test('forms include CSRF and missing or invalid CSRF is rejected for mutations', async ({ page, browser, app }, testInfo) => {
    await page.goto(`${app.baseURL}/pub`);
    await expect(page.locator('form[action="/pub"] input[name="_csrf"]')).toHaveCount(1);
    await adminLogin(page, app);

    for (const csrf of ['', 'invalid.csrf']) {
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: {
          _csrf: csrf,
          short_name: uniqueShort('csrf', testInfo),
          name: 'CSRF Fail',
          description: '',
        },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(403);
      await expectSafeResponse(response);
    }

    const loggedOut = await browser.newPage();
    const loginScopeCsrf = await publicCsrf(loggedOut, app, '/admin');
    const loginScoped = await loggedOut.request.post(`${app.baseURL}/admin/board/create`, {
      form: {
        _csrf: loginScopeCsrf,
        short_name: uniqueShort('login', testInfo),
        name: 'Wrong Scope',
        description: '',
      },
      maxRedirects: 0,
    });
    expect(loginScoped.status()).toBe(403);
    await loggedOut.close();
  });

  test('session-scoped admin CSRF cannot be reused across sessions or after logout', async ({ page, browser, app }, testInfo) => {
    await adminLogin(page, app);
    const csrfFromSessionA = await adminCsrf(page, app);

    const contextB = await browser.newContext();
    const pageB = await contextB.newPage();
    await pageB.goto(`${app.baseURL}/admin`);
    await pageB.getByLabel('Username').fill(ADMIN_USERNAME);
    await pageB.getByLabel('Password').fill(ADMIN_PASSWORD);
    await Promise.all([
      pageB.waitForURL(/\/admin\/panel/),
      pageB.getByRole('button', { name: 'authenticate' }).click(),
    ]);

    const crossSession = await pageB.request.post(`${app.baseURL}/admin/board/create`, {
      form: {
        _csrf: csrfFromSessionA,
        short_name: uniqueShort('xcsrf', testInfo),
        name: 'Cross Session',
        description: '',
      },
      maxRedirects: 0,
    });
    expect(crossSession.status()).toBe(403);
    await contextB.close();

    await adminLogout(page);
    const afterLogout = await page.request.post(`${app.baseURL}/admin/board/create`, {
      form: {
        _csrf: csrfFromSessionA,
        short_name: uniqueShort('logout', testInfo),
        name: 'After Logout',
        description: '',
      },
      maxRedirects: 0,
    });
    expect(afterLogout.status()).toBe(403);
  });

  test('Origin, Referer, null Origin, loopback aliases, and GET mutations follow policy', async ({ page, app }, testInfo) => {
    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);
    const body = {
      _csrf: csrf,
      short_name: uniqueShort('orig', testInfo),
      name: 'Origin Check',
      description: '',
    };

    const badOrigin = await page.request.post(`${app.baseURL}/admin/board/create`, {
      form: body,
      headers: { Origin: 'http://evil.example' },
      maxRedirects: 0,
    });
    expect(badOrigin.status()).toBe(403);
    await expectSafeResponse(badOrigin);

    const badReferer = await page.request.post(`${app.baseURL}/admin/board/create`, {
      form: { ...body, short_name: uniqueShort('ref', testInfo) },
      headers: { Referer: 'http://evil.example/path' },
      maxRedirects: 0,
    });
    expect(badReferer.status()).toBe(403);

    const nullLoopback = await page.request.post(`${app.baseURL}/admin/board/create`, {
      form: { ...body, short_name: uniqueShort('null', testInfo) },
      headers: { Origin: 'null' },
      maxRedirects: 0,
    });
    expect(nullLoopback.status()).toBe(303);

    const alias = await page.request.post(`${app.baseURL}/admin/board/create`, {
      form: { ...body, short_name: uniqueShort('loop', testInfo) },
      headers: {
        Host: `localhost:${app.port}`,
        Origin: `http://127.0.0.1:${app.port}`,
      },
      maxRedirects: 0,
    });
    expect(alias.status()).toBe(303);

    const getMutation = await page.request.get(`${app.baseURL}/admin/board/create`);
    expect([404, 405]).toContain(getMutation.status());
    await expectSafeResponse(getMutation);
  });

  test('back-forward and refresh around POST redirects do not duplicate mutations', async ({ page, app }) => {
    const threadId = await createThread(page, app, 'pub', {
      subject: 'navigation csrf',
      body: 'initial post',
    });
    const postCount = await page.locator('.post').count();
    await page.reload();
    await expect(page.locator('.post')).toHaveCount(postCount);
    const targetUrl = `${app.baseURL}/pub/thread/${threadId}`;
    await page.goBack({ waitUntil: 'domcontentloaded', timeout: 5_000 }).catch(() => undefined);
    await page.goForward({ waitUntil: 'domcontentloaded', timeout: 5_000 }).catch(() => undefined);
    await page.waitForURL(targetUrl, { timeout: 2_000 }).catch(() => undefined);
    if (!page.url().includes(`/pub/thread/${threadId}`)) {
      await page.evaluate(() => window.stop()).catch(() => undefined);
      await page.goto(targetUrl);
    }
    await expect(page.locator('.post')).toHaveCount(postCount);
    await expectSafePage(page);
    expect(page.url()).toContain(`/pub/thread/${threadId}`);
  });
});
