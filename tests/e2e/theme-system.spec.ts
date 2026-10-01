import { BUILTIN_THEMES, expectReadableContrast, expectNoHorizontalOverflow } from './phase4-helpers';
import { adminCsrf, adminLogin, createThreadViaRequest, expect, test, RustChanServer, setBoardFixtureSettings, isNoJsProject, } from './helpers';
import { expectConsoleError, expectHttpError, expectTransportFailure } from './diagnostics';

// Each test owns an isolated application/database through the app fixture.
test('rate-limit pages retain board defaults, visitor cookies, and signed preference state', async ({ page }, info) => {
  const app = await RustChanServer.create(info, { env: { CHAN_RATE_GETS: '10', CHAN_RATE_WINDOW: '3600' } });
  try {
    await app.initializeDefaultData();
    setBoardFixtureSettings(app, 'pub', { defaultTheme: 'blue-sky' });
    await app.start();
    for (let request = 0; request < 11; request++) await page.request.get(`${app.baseURL}/pub`);
    expectHttpError({
      method: 'GET',
      path: '/pub',
      status: 429,
      reason: 'the rate-limited page itself is the subject: it must retain board defaults',
    });
    const response = await page.goto(`${app.baseURL}/pub`);
    expect(response!.status()).toBe(429);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
    await expect(page.locator('body')).toHaveAttribute('data-rate-limit-page', '1');
    const token = await page.locator('.user-preferences-form input[name="_csrf"]').inputValue();
    expect(token).toMatch(/^[0-9a-f]+\.[0-9a-f]{64}$/);
    const cookie = (await page.context().cookies()).find(cookie => cookie.name === 'csrf_token');
    expect(token.split('.')[0]).toBe(cookie?.value);
    await page.context().addCookies([{ name: 'rustchan_theme', value: 'chanclassic', url: app.baseURL }]);
    expectHttpError({
      method: 'GET',
      path: '/pub',
      status: 429,
      reason: 'the rate limit is still active for the cookie-theme check on the rate-limited page',
    });
    await page.goto(`${app.baseURL}/pub`);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'chanclassic');
  } finally {
    await app.dispose();
  }
});

test('live theme selection updates every state marker and preserves the initial stylesheet', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'live switching requires JavaScript');
  const response = await page.goto(app.baseURL);
  const html = await response!.text();
  expect(html).not.toContain('id="active-theme-stylesheet"');
  await expect(page.locator('#active-theme-stylesheet')).toHaveCount(0);
  await page.locator('#theme-picker-btn').click();
  await page.locator('.user-preferences-form select[name="theme"]').selectOption('blue-sky');
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
  await expect(page.locator('.user-preferences-status')).toHaveText('Saved.');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
});

test('error responses retain the visitor theme with and without JavaScript', async ({ page, app }) => {
  await page.context().addCookies([{ name: 'rustchan_theme', value: 'blue-sky', url: app.baseURL }]);
  expectHttpError({
    method: 'GET',
    path: '/missing-board',
    status: 404,
    reason: 'the 404 page itself is the subject: it must retain the visitor theme',
  });
  const response = await page.goto(`${app.baseURL}/missing-board`);
  expect(response!.status()).toBe(404);
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
});

test('disabled Terminal cannot remain a configured default', async ({ page, app }) => {
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  for (const [route, form] of [
    ['/admin/site/settings', { default_theme: 'terminal' }],
    ['/admin/theme/update', { existing_slug: 'terminal', slug: 'terminal', display_name: 'Terminal' }],
  ] as const) {
    const response = await page.request.post(`${app.baseURL}${route}`, {
      form: { _csrf: csrf, ...form }, maxRedirects: 0,
    });
    expect(response.status()).toBe(303);
  }
  await page.context().clearCookies();
  await page.goto(app.baseURL);
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');
});

test('rapid choices, invalid input, stylesheet failure, and save failure keep a coherent theme', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'JavaScript failure paths');
  await adminLogin(page, app);
  const create = await page.request.post(`${app.baseURL}/admin/theme/create`, {
    form: { _csrf: await adminCsrf(page, app), slug: 'failure-custom', display_name: 'Failure Custom', theme_mode: 'legacy', custom_css: '--bg: #123456;', enabled: '1' }, maxRedirects: 0,
  });
  expect(create.status()).toBe(303);
  await page.goto(app.baseURL);
  await page.locator('#theme-picker-btn').click();
  const select = page.locator('.user-preferences-form select[name="theme"]');
  await page.route('**/theme-css/failure-custom', route => route.abort());
  expectTransportFailure({
    url: `${app.baseURL}/theme-css/failure-custom`,
    reason: 'the deliberately aborted stylesheet request must surface as "Could not load theme"',
  });
  expectConsoleError({
    pattern: /net::ERR_FAILED/,
    reason: 'Chromium logs the aborted resource; WebKit reports only the transport failure',
    optional: true,
  });
  await select.selectOption('failure-custom');
  await expect(page.locator('.user-preferences-status')).toContainText('Could not load theme');
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');
  await expect(select).toHaveValue('forest');
  await page.unroute('**/theme-css/failure-custom');
  expectHttpError({
    method: 'POST',
    path: '/preferences',
    status: 403,
    reason: 'the injected save failure must surface as "Could not save" without changing the theme',
  });
  await page.route('**/preferences', route => route.fulfill({ status: 403, body: 'expired CSRF token' }));
  await select.selectOption('blue-sky');
  await expect(page.locator('.user-preferences-status')).toContainText('Could not save');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
  await page.unroute('**/preferences');
  await page.evaluate(() => {
    (window as any).setTheme('deep-orbit');
    (window as any).setTheme('terminal');
    (window as any).setTheme('chanclassic');
    (window as any).setTheme('not-a-theme');
  });
  await expect(page.locator('.user-preferences-status')).toHaveText('Saved.');
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'chanclassic');
  await expect(select).toHaveValue('chanclassic');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'chanclassic');
});

test('custom CSS updates revalidate, renamed selectors follow, and deleted preferences fall back', async ({ page, app }) => {
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  const create = await page.request.post(`${app.baseURL}/admin/theme/create`, {
    form: { _csrf: csrf, slug: 'audit-custom', display_name: 'Audit Custom', theme_mode: 'legacy',
      custom_css: 'html[data-theme="audit-custom"] { --bg: #123456; }', enabled: '1' }, maxRedirects: 0,
  });
  expect(create.status()).toBe(303);
  const initial = await page.goto(`${app.baseURL}/theme/audit-custom?return_to=/`);
  const initialHref = (await initial!.text()).match(/id="active-theme-stylesheet" href="([^"]+)"/)![1];
  expect(await page.locator('#active-theme-stylesheet').getAttribute('href')).toBe(initialHref);
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(18, 52, 86)');
  for (const [slug, color] of [['audit-custom', '#654321'], ['audit-renamed', '#654321']]) {
    const update = await page.request.post(`${app.baseURL}/admin/theme/update`, {
      form: { _csrf: csrf, existing_slug: 'audit-custom', slug, display_name: 'Audit Custom', theme_mode: 'legacy',
        custom_css: `html[data-theme="audit-custom"] { --bg: ${color}; }`, enabled: '1' }, maxRedirects: 0,
    });
    expect(update.status()).toBe(303);
    await page.goto(`${app.baseURL}/theme/${slug}?return_to=/`);
    await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(101, 67, 33)');
  }
  const deleted = await page.request.post(`${app.baseURL}/admin/theme/delete`, {
    form: { _csrf: csrf, slug: 'audit-renamed' }, maxRedirects: 0,
  });
  expect(deleted.status()).toBe(303);
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');
});

test('all built-ins remain coherent across public, auth, moderation, and error pages', async ({ page, app, javaScriptEnabled }, info) => {
  test.setTimeout(240_000);
  const id = await createThreadViaRequest(page, app, 'pub', { subject: 'Theme audit', body: '>quote\nTheme audit body with https://example.com' });
  expectHttpError({
    method: 'GET',
    path: '/missing-board',
    status: 404,
    times: 'any',
    reason: 'every built-in theme renders the missing-board 404 page in this route loop',
  });
  expectHttpError({
    method: 'GET',
    path: '/missing/path/unknown',
    status: 404,
    times: 'any',
    reason: 'every built-in theme renders the unknown-path 404 page in this route loop',
  });
  for (const [slug] of BUILTIN_THEMES) {
    await page.goto(`${app.baseURL}/theme/${slug}?return_to=/`);
    for (const route of ['/', '/pub', '/pub/catalog', '/pub/search?q=Theme', `/pub/thread/${id}`, '/admin', '/banned?reason=Theme+audit', '/missing-board', '/missing/path/unknown']) {
      await page.goto(`${app.baseURL}${route}`);
      await expect(page.locator('html'), `${slug} ${route}`).toHaveAttribute('data-active-theme', slug);
      await expectReadableContrast(page, 'body', `${slug} ${route}`, 4.2);
      await expectNoHorizontalOverflow(page, `${slug} ${route}`);
    }
    await page.goto(`${app.baseURL}/pub/thread/${id}`);
    await page.keyboard.press('Tab');
    const focus = await page.locator(':focus').evaluate(el => ({ width: getComputedStyle(el).outlineWidth, style: getComputedStyle(el).outlineStyle }));
    expect(Number.parseFloat(focus.width), `${slug} keyboard focus`).toBeGreaterThan(0);
    expect(focus.style).not.toBe('none');
    // These same components are appended by the posting and polling code.
    await page.evaluate(() => {
      for (const name of ['post-success-banner', 'new-replies-pill']) {
        const el = document.createElement('div'); el.className = name; el.id = 'audit-' + name; el.textContent = 'New reply'; document.body.appendChild(el);
      }
    });
    await expectReadableContrast(page, '#audit-post-success-banner', `${slug} success`, 3);
    await expectReadableContrast(page, '#audit-new-replies-pill', `${slug} new replies`, 3);
    await page.locator('#audit-new-replies-pill').evaluate(el => el.remove());
    if (javaScriptEnabled) {
      await page.locator('#theme-picker-btn').click();
      await expect(page.locator('.user-preferences-form select[name="theme"]')).toHaveValue(slug);
      await expectReadableContrast(page, '.user-preferences-form', `${slug} preferences`, 3);
    }
    if (['blue-sky', 'terminal', 'forest'].includes(slug)) {
      await page.screenshot({ path: info.outputPath(`${slug}-thread.png`), fullPage: true });
    }
  }
  await adminLogin(page, app);
  for (const [slug] of BUILTIN_THEMES) {
    await page.goto(`${app.baseURL}/theme/${slug}?return_to=/admin/panel`);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', slug);
    await expectReadableContrast(page, '.admin-panel', `${slug} admin`, 4.2);
    await expectNoHorizontalOverflow(page, `${slug} admin`);
  }
});

test('builder presets use matching native controls and malformed preview input stays scoped', async ({ page, app, javaScriptEnabled }) => {
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  const response = await page.request.post(`${app.baseURL}/admin/theme/create`, {
    form: { _csrf: csrf, slug: 'builder-light', display_name: 'Builder Light', theme_mode: 'builder', base_preset: 'blue-sky', enabled: '1' }, maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  await page.goto(`${app.baseURL}/theme/builder-light?return_to=/pub`);
  await expect(page.locator('html')).toHaveCSS('color-scheme', 'light');
  await page.goto(`${app.baseURL}/admin/panel?open=theme-catalog#theme-catalog`);
  const card = page.locator('#theme-builder-light');
  await card.locator('summary').first().click();
  await expect(card.locator('[data-theme-preview]')).toHaveCSS('background-color', 'rgb(223, 234, 242)');
  await expect(card.locator('[data-theme-preview]')).toHaveCSS('font-family', /sans-serif/);
  expect(await card.locator('[data-theme-preview]').evaluate(el => getComputedStyle(el).getPropertyValue('--theme-preview-pad').trim())).toBe('0.75rem');
  for (const [selector, field] of [
    ['.theme-preview-meta', 'meta_text_color'],
    ['.flash-ok', 'success_color'],
    ['.flash-error', 'danger_color'],
  ]) {
    const value = await card.locator(`[name="${field}"]`).inputValue();
    const channels = value.slice(1).match(/../g)!.map(channel => Number.parseInt(channel, 16));
    await expect(card.locator(selector).first()).toHaveCSS('color', `rgb(${channels.join(', ')})`);
  }
  if (!javaScriptEnabled) {
    await card.locator('[name="base_preset"]').selectOption('forest');
    await card.getByRole('button', { name: 'Save preset defaults', exact: true }).click();
    await page.goto(`${app.baseURL}/theme/builder-light?return_to=/pub`);
    await expect(page.locator('html')).toHaveCSS('color-scheme', 'dark');
  }
  if (javaScriptEnabled) {
    const field = card.locator('[name="background_color"]');
    await field.fill('#ffffff; } body { --preview-escaped: yes; } /*');
    await field.dispatchEvent('input');
    expect(await page.locator('body').evaluate(el => getComputedStyle(el).getPropertyValue('--preview-escaped'))).toBe('');
  }
});

test('invalid cookies, storage denial, and all-disabled catalogs render a usable fallback', async ({ page, app, javaScriptEnabled }) => {
  if (javaScriptEnabled) await page.addInitScript(() => {
    Object.defineProperty(window, 'localStorage', { get() { throw new DOMException('Blocked', 'SecurityError'); } });
  });
  await page.context().addCookies([{ name: 'rustchan_theme', value: 'deleted-theme', url: app.baseURL }]);
  await page.goto(app.baseURL);
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  for (const [slug, display_name] of BUILTIN_THEMES) {
    const response = await page.request.post(`${app.baseURL}/admin/theme/update`, {
      form: { _csrf: csrf, existing_slug: slug, slug, display_name }, maxRedirects: 0,
    });
    expect(response.status()).toBe(303);
  }
  const failed: string[] = [];
  page.on('response', response => { if (response.url().includes('/theme-css/') && response.status() >= 400) failed.push(response.url()); });
  await page.goto(app.baseURL);
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');
  expect(failed).toEqual([]);
  await expect(page.locator('body')).toHaveCSS('color-scheme', 'dark');
});

test('theme catalog changes invalidate cached HTML even when the active theme stays the same', async ({ page, app }) => {
  await adminLogin(page, app);
  await page.context().addCookies([{ name: 'rustchan_theme', value: 'forest', url: app.baseURL }]);
  const first = await page.request.get(`${app.baseURL}/pub`);
  const etag = first.headers().etag;
  expect(etag).toBeTruthy();
  const update = await page.request.post(`${app.baseURL}/admin/theme/create`, {
    form: { _csrf: await adminCsrf(page, app), slug: 'cache-new', display_name: 'Cache New', theme_mode: 'legacy', custom_css: '', enabled: '1' }, maxRedirects: 0,
  });
  expect(update.status()).toBe(303);
  const second = await page.request.get(`${app.baseURL}/pub`, { headers: { 'If-None-Match': etag } });
  expect(second.status()).toBe(200);
  expect(second.headers().etag).not.toBe(etag);
  expect(await second.text()).toContain('Cache New');
});
