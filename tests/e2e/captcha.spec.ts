import {
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  test,
  uniqueShort,
} from './helpers';

test.describe('CAPTCHA posting flow', () => {
  test('renders image CAPTCHA and rejects bad answers in Chromium and WebKit', async ({ page, app }, testInfo) => {
    test.skip(!['chromium', 'webkit'].includes(testInfo.project.name), 'Chromium/WebKit CAPTCHA pass');

    const board = uniqueShort('cap', testInfo);
    app.createBoardCli({ short: board, name: 'Captcha Board' });
    setBoardFixtureSettings(app, board, { allowCaptcha: true, postCooldownSecs: 0 });

    await page.goto(`${app.baseURL}/${board}`);
    await expectSafePage(page);
    const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first();
    if (await toggle.isVisible()) {
      await toggle.click();
    }
    const form = page.locator(`form[action="/${board}"]`).first();
    const captchaId = await form.locator('input[name="captcha_id"]').getAttribute('value');
    expect(captchaId).toMatch(/^[a-f0-9]{32}$/);
    await expect(form.locator('input[name="captcha_answer"]')).toBeVisible();
    await expect(form.locator('.captcha-image')).toBeVisible();

    const imageSrc = await form.locator('.captcha-image').getAttribute('src');
    expect(imageSrc).toMatch(/^\/captcha\/[a-f0-9]{32}\?board=/);
    const image = await page.request.get(`${app.baseURL}${imageSrc}`);
    expect(image.status()).toBe(200);
    expect(image.headers()['content-type']).toContain('image/png');
    expect(image.headers()['cache-control']).toMatch(/private/i);
    expect(image.headers()['cache-control']).toMatch(/no-store/i);
    expect((await image.body()).length).toBeGreaterThan(100);

    await form.locator('input[name="subject"]').fill('bad captcha');
    await form.locator('textarea[name="body"]').fill('this should not post');
    await form.locator('input[name="captcha_answer"]').fill('WRONG');
    await form.getByRole('button', { name: /post thread/i }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/CAPTCHA verification failed/i);
    await expect(form.locator('.captcha-refresh-link')).toHaveAttribute('href', new RegExp(`^/${board}\\?captcha_refresh=[a-f0-9]{32}$`));
  });

  test('no-JS users can reload for a fresh challenge and see server-rendered errors', async ({ browser, app }, testInfo) => {
    test.skip(!['chromium', 'webkit'].includes(testInfo.project.name), 'Chromium/WebKit no-JS CAPTCHA pass');

    const board = uniqueShort('cns', testInfo);
    app.createBoardCli({ short: board, name: 'Captcha NoJS' });
    setBoardFixtureSettings(app, board, { allowCaptcha: true, postCooldownSecs: 0 });

    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    await page.goto(`${app.baseURL}/${board}`);
    await expect(page.locator('html')).toHaveClass(/no-js/);
    await expectSafePage(page);

    const form = page.locator(`form[action="/${board}"]`).first();
    const firstCaptchaId = await form.locator('input[name="captcha_id"]').getAttribute('value');
    expect(firstCaptchaId).toMatch(/^[a-f0-9]{32}$/);
    await form.locator('.captcha-refresh-link').click();
    await page.waitForURL(new RegExp(`/${board}\\?captcha_refresh=[a-f0-9]{32}$`));
    const refreshedCaptchaId = await page
      .locator(`form[action="/${board}"] input[name="captcha_id"]`)
      .first()
      .getAttribute('value');
    expect(refreshedCaptchaId).toMatch(/^[a-f0-9]{32}$/);
    expect(refreshedCaptchaId).not.toBe(firstCaptchaId);

    const refreshedForm = page.locator(`form[action="/${board}"]`).first();
    await refreshedForm.locator('input[name="subject"]').fill('no js captcha');
    await refreshedForm.locator('textarea[name="body"]').fill('server rendered captcha failure');
    await refreshedForm.locator('input[name="captcha_answer"]').fill('WRONG');
    await refreshedForm.getByRole('button', { name: /post thread/i }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/CAPTCHA verification failed/i);
    const errorCaptchaId = await page
      .locator(`form[action="/${board}"] input[name="captcha_id"]`)
      .first()
      .getAttribute('value');
    expect(errorCaptchaId).toMatch(/^[a-f0-9]{32}$/);
    expect(errorCaptchaId).not.toBe(refreshedCaptchaId);
    await expectSafePage(page);
    await context.close();
  });
});
