import {
  adminCsrf,
  adminLogin,
  createReply,
  createThread,
  expect,
  expectSafePage,
  test,
} from './helpers';

test.describe('mobile responsive smoke', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('home, board, thread composer, media preview, admin login, and board edit are usable on mobile', async ({ page, app }) => {
    const threadId = await createThread(page, app, 'img', {
      subject: 'mobile media',
      body: 'mobile preview',
      filePath: app.fixtures().tinyPng,
    });
    await page.goto(app.baseURL);
    await expectSafePage(page);
    await page.goto(`${app.baseURL}/img`);
    await expectSafePage(page);
    await page.goto(`${app.baseURL}/img/thread/${threadId}`);
    await expectSafePage(page);
    await expect(page.locator('[data-action="toggle-post-form"]')).toBeVisible();
    await expect(page.locator('.media-preview').first()).toBeVisible();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(8);

    await page.goto(`${app.baseURL}/admin`);
    await expect(page.getByLabel('Username')).toBeVisible();
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=board-pub#board-pub`);
    await expect(page.locator('#board-pub summary')).toBeVisible();
    await expect(page.locator('#board-pub input[name="name"]').first()).toBeVisible();
  });
});

test.describe('no-JS fallback', () => {
  test.use({ javaScriptEnabled: false });

  test('public navigation, thread creation, admin login, settings POST, and CSRF work without JavaScript', async ({ page, app }) => {
    await page.goto(app.baseURL);
    await expectSafePage(page);
    await page.goto(`${app.baseURL}/pub`);
    await expectSafePage(page);

    const form = page.locator('form[action="/pub"]').first();
    await form.locator('input[name="subject"]').fill('no js thread');
    await form.locator('textarea[name="body"]').fill('created with JavaScript disabled');
    await Promise.all([
      page.waitForURL(/\/pub\/thread\/\d+/),
      form.getByRole('button', { name: /post thread/i }).click(),
    ]);
    await expectSafePage(page);

    const threadUrl = page.url();
    const threadId = Number(threadUrl.match(/\/thread\/(\d+)/)?.[1]);
    const csrf = await page.locator(`form[action="/pub/thread/${threadId}"] input[name="_csrf"]`).first().getAttribute('value');
    const reply = await page.request.post(`${app.baseURL}/pub/thread/${threadId}`, {
      multipart: {
        _csrf: csrf ?? '',
        submission_token: 'no-js-reply',
        body: 'no js reply via same browser session',
      },
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(reply.status());
    await page.reload();
    await expect(page.locator('body')).toContainText('no js reply via same browser session');

    await page.goto(`${app.baseURL}/admin`);
    await page.getByLabel('Username').fill('admin');
    await page.getByLabel('Password').fill('AdminPass123!');
    await Promise.all([
      page.waitForURL(/\/admin\/panel/),
      page.getByRole('button', { name: 'authenticate' }).click(),
    ]);
    const adminToken = await adminCsrf(page, app);
    const settings = await page.request.post(`${app.baseURL}/admin/site/settings`, {
      form: {
        _csrf: adminToken,
        site_name: 'No JS RustChan',
        site_subtitle: 'forms still work',
        default_theme: 'forest',
        homepage_new_thread_badges_enabled: '1',
        homepage_new_reply_badges_enabled: '1',
        thread_new_reply_badges_enabled: '1',
      },
      maxRedirects: 0,
    });
    expect(settings.status()).toBe(303);
  });
});
