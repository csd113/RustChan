import {
  adminCsrf,
  adminLogin,
  expect,
  expectSafePage,
  test,
  uniqueShort,
  updateBoardSettings,
} from './helpers';
import {
  BUILTIN_THEMES,
  expectNoHorizontalOverflow,
  expectReadableContrast,
  expectSafeBody,
  phase4SkipUnless,
  watchClientErrors,
} from './phase4-helpers';

test.describe('phase 4 built-in theme and contrast polish', () => {
  test.beforeEach(async ({}, testInfo) => {
    phase4SkipUnless(testInfo, ['chromium'], 'theme polish coverage runs once in Chromium');
  });

  test('theme picker order, selected state, site default, board override, and inherit behavior stay stable', async ({ page, app }, testInfo) => {
    await page.goto(app.baseURL);
    await expectSafeBody(page, 'home theme picker');
    const pickerOptions = await page.locator('.user-preferences-form select[name="theme"] option').evaluateAll((options) => options.map((option) => ({
      value: (option as HTMLOptionElement).value,
      label: option.textContent?.trim() ?? '',
      selected: (option as HTMLOptionElement).selected,
    })));
    expect(pickerOptions.map((option) => [option.value, option.label])).toEqual(BUILTIN_THEMES);
    expect(pickerOptions.find((option) => option.value === 'forest')?.selected).toBe(true);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');

    await page.goto(`${app.baseURL}/theme/blue-sky?return_to=/`);
    await page.waitForURL(app.baseURL + '/');
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
    const selectedAfterCookie = await page.locator('.user-preferences-form select[name="theme"]').inputValue();
    expect(selectedAfterCookie).toBe('blue-sky');

    await adminLogin(page, app);
    const siteSettings = await page.request.post(`${app.baseURL}/admin/site/settings`, {
      form: {
        _csrf: await adminCsrf(page, app),
        site_name: 'RustChan Phase 4 Themes',
        site_subtitle: 'theme default smoke',
        default_theme: 'deep-orbit',
        homepage_new_thread_badges_enabled: '1',
        homepage_new_reply_badges_enabled: '1',
        thread_new_reply_badges_enabled: '1',
        banner_rotation_interval_minutes: '5',
      },
      maxRedirects: 0,
    });
    expect(siteSettings.status()).toBe(303);

    await page.context().clearCookies();
    await page.goto(app.baseURL);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'deep-orbit');
    expect(await page.locator('.user-preferences-form select[name="theme"]').inputValue()).toBe('deep-orbit');

    const board = uniqueShort('theme', testInfo);
    app.createBoardCli({ short: board, name: 'Theme Override Board', description: 'board theme override coverage' });
    await updateBoardSettings(page, app, board, {
      name: 'Theme Override Board',
      description: 'board theme override coverage',
      defaultTheme: 'chanclassic',
    });
    await page.context().clearCookies();
    await page.goto(`${app.baseURL}/${board}`);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'chanclassic');

    await updateBoardSettings(page, app, board, {
      name: 'Theme Override Board',
      description: 'board inherits the site default again',
      defaultTheme: '',
    });
    await page.context().clearCookies();
    await page.goto(`${app.baseURL}/${board}`);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'deep-orbit');

    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=theme-catalog#theme-catalog`);
    const adminOptions = await page.locator('select[name="default_theme"]').first().locator('option').evaluateAll((options) => options.map((option) => [
      (option as HTMLOptionElement).value,
      option.textContent?.trim() ?? '',
    ]));
    expect(adminOptions).toEqual(BUILTIN_THEMES);
    expect(await page.locator('select[name="default_theme"]').first().inputValue()).toBe('deep-orbit');
  });

  test('built-in themes keep core public and admin states readable without console errors', async ({ page, app }) => {
    const assertNoClientErrors = watchClientErrors(page);
    await adminLogin(page, app);

    for (const [slug, label] of BUILTIN_THEMES) {
      await page.goto(`${app.baseURL}/theme/${slug}?return_to=/pub`);
      await page.waitForURL(`${app.baseURL}/pub`);
      await expectSafePage(page, { allowAdminInternals: true });
      await expect(page.locator('html')).toHaveAttribute('data-active-theme', slug);
      await expectNoHorizontalOverflow(page, `${label} board`);
      await expectReadableContrast(page, 'body', `${label} body text`, 4.2);
      await expectReadableContrast(page, '.board-nav-link', `${label} board nav link`, 3);
      await expectReadableContrast(page, '.post-toggle-btn', `${label} post toggle`, 3);

      const toggle = page.locator('[data-action="toggle-post-form"]').first();
      if (await toggle.isVisible()) {
        await toggle.click();
      }
      await expectReadableContrast(page, 'input[name="subject"]', `${label} post subject input`, 3);
      await expectReadableContrast(page, 'textarea[name="body"]', `${label} post body input`, 3);
      const placeholder = await page.locator('input[name="name"]').first().evaluate((element) => {
        const style = window.getComputedStyle(element, '::placeholder');
        return { color: style.color, opacity: style.opacity };
      });
      expect(placeholder.color, `${label} placeholder color`).toMatch(/rgb/);
      expect(Number.parseFloat(placeholder.opacity || '1'), `${label} placeholder opacity`).toBeGreaterThan(0);

      await page.locator(`form[action="/pub"]`).first().getByRole('button', { name: /post thread/i }).click();
      await expect(page.locator('.post-error-banner').first()).toBeVisible();
      await expectReadableContrast(page, '.post-error-banner', `${label} post error`, 3);

      await page.goto(`${app.baseURL}/admin/panel?open=site-settings#site-settings`);
      await expectSafePage(page, { allowAdminInternals: true });
      await expectNoHorizontalOverflow(page, `${label} admin settings`);
      await expectReadableContrast(page, '.admin-panel', `${label} admin panel`, 4.2);
      await expectReadableContrast(page, 'select[name="default_theme"]', `${label} admin theme select`, 3);
      await expectReadableContrast(page, '#site-settings button[type="submit"]', `${label} admin save button`, 3);
    }

    assertNoClientErrors();
  });
});
