import { test, expect, adminLogin, createStandaloneApp, RustChanServer, isNoJsProject, ADMIN_USERNAME, ADMIN_PASSWORD } from './helpers';
import { expectHttpError, newAuditedContext } from './diagnostics';
import fsp from 'node:fs/promises';
import path from 'node:path';

// All runtimes are disposable local harness instances. Nothing reaches a public server.
test('network controls preserve comments, report overrides, and apply only after restart', async ({ page, browser }, testInfo) => {
  const app = await createStandaloneApp({ admin: true });
  try {
    const settings = path.join(app.dataDir, 'settings.toml');
    await fsp.appendFile(settings, '\n# operator note: retain this comment\n');
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel#network-security`);
    const section = page.locator('#network-security');
    await expect(section).toBeVisible();
    await expect(section.locator('.admin-setting')).toHaveCount(12);
    await expect(section.locator('#state-rate_limit_gets')).toContainText('CHAN_RATE_GETS');
    await expect(section.locator('#state-bind_addr')).toContainText('CHAN_BIND');
    await section.locator('[name=rate_limit_gets]').fill('180');
    await section.locator('[name=rate_limit_policy]').selectOption('reads');
    await section.locator('[name=trusted_proxy_cidrs]').fill('127.0.0.1/32\n::1/128');
    await section.locator('[name=public_hosts]').fill('example.test');
    await section.getByRole('button', { name: 'Save for next restart' }).click();
    await expect(page.locator('.admin-panel > .admin-flash')).toContainText('Restart required');
    await expect(page.locator('#state-rate_limit_gets')).toContainText('180');
    await expect(page.locator('#state-rate_limit_gets')).toContainText('1000');
    await expect(page.locator('#state-rate_limit_gets')).toContainText('Override prevents');
    await expect(page.locator('#state-rate_limit_policy')).toContainText('restart required');
    const bytes = await fsp.readFile(settings, 'utf8');
    expect(bytes).toContain('# operator note: retain this comment');
    expect(bytes).toContain('rate_limit_gets = 180');
    expect(bytes).toContain('rate_limit_policy = "reads"');
    await app.restart();
    await page.goto(`${app.baseURL}/admin/panel#network-security`);
    await expect(page.locator('#state-rate_limit_policy')).toContainText('Saved configuration matches');
    await expect(page.locator('#setting-rate_limit_policy')).toHaveValue('reads');
    const storageState = await page.context().storageState();
    for (const [layout, viewport] of [['desktop', { width: 1280, height: 900 }], ['mobile', { width: 390, height: 844 }]] as const) {
      const context = await newAuditedContext(browser, { viewport, javaScriptEnabled: !isNoJsProject(testInfo), storageState });
      try {
        const rendered = await context.newPage();
        await rendered.goto(`${app.baseURL}/admin/panel#network-security`);
        await rendered.locator('#network-security-title').scrollIntoViewIfNeeded();
        await expect(rendered.locator('#setting-behind_proxy')).toBeVisible();
        expect(await rendered.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
        await rendered.screenshot({ path: testInfo.outputPath(`network-${layout}.png`) });
        await rendered.locator('#setting-behind_proxy').focus();
        await rendered.keyboard.press('Tab');
        await expect(rendered.locator('#setting-trusted_proxy_cidrs')).toBeFocused();
      } finally { await context.close(); }
    }
  } finally { await app.dispose(); }
});

test('invalid network form retains entered values and makes no partial change', async ({ page }) => {
  const app = await createStandaloneApp({ admin: true });
  try {
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/theme/blue-sky?return_to=/admin/panel%23network-security`);
    const settings = path.join(app.dataDir, 'settings.toml');
    const before = await fsp.readFile(settings, 'utf8');
    await page.locator('#setting-rate_limit_gets').fill('180');
    await page.locator('#setting-trusted_proxy_cidrs').fill('bad proxy');
    expectHttpError({ method: 'POST', path: '/admin/network/settings', status: 422, reason: 'Invalid CIDR must be rejected without mutation.' });
    await page.locator('#admin-network-settings button[type=submit]').click();
    await expect(page.getByRole('alert')).toContainText('No changes saved');
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'blue-sky');
    await expect(page.locator('#setting-rate_limit_gets')).toHaveValue('180');
    await expect(page.locator('#setting-trusted_proxy_cidrs')).toHaveValue('bad proxy');
    expect(await fsp.readFile(settings, 'utf8')).toBe(before);
  } finally { await app.dispose(); }
});

test('network saves reject absent authorization, forged CSRF and cross-origin requests', async ({ page }) => {
  const app = await createStandaloneApp({ admin: true });
  try {
    const settings = path.join(app.dataDir, 'settings.toml');
    const before = await fsp.readFile(settings, 'utf8');
    const anonymous = await page.request.post(`${app.baseURL}/admin/network/settings`, { form: { rate_limit_gets: '100' } });
    expect(anonymous.status()).toBe(403);
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel#network-security`);
    const csrf = await page.locator('#admin-network-settings [name=_csrf]').inputValue();
    const forged = await page.request.post(`${app.baseURL}/admin/network/settings`, { form: { _csrf: 'forged', rate_limit_gets: '100' } });
    expect(forged.status()).toBe(403);
    const crossOrigin = await page.request.post(`${app.baseURL}/admin/network/settings`, { headers: { Origin: 'https://attacker.example' }, form: { _csrf: csrf, rate_limit_gets: '100' } });
    expect(crossOrigin.status()).toBe(403);
    expect(await fsp.readFile(settings, 'utf8')).toBe(before);
  } finally { await app.dispose(); }
});

test('failed persistence is reported honestly and preserves the original file', async ({ page }) => {
  const app = await createStandaloneApp({ admin: true });
  const settings = path.join(app.dataDir, 'settings.toml');
  const preserved = path.join(app.dataDir, 'preserved-settings.toml');
  let replaced = false;
  try {
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel#network-security`);
    const before = await fsp.readFile(settings, 'utf8');
    await fsp.rename(settings, preserved);
    await fsp.mkdir(settings);
    replaced = true;
    await page.locator('#setting-rate_limit_gets').fill('180');
    expectHttpError({ method: 'POST', path: '/admin/network/settings', status: 422, reason: 'Disposable fixture replaces settings path with a directory to force a real I/O failure.' });
    await page.locator('#admin-network-settings button[type=submit]').click();
    await expect(page.getByRole('alert').first()).toContainText('No changes saved');
    expect(await fsp.readFile(preserved, 'utf8')).toBe(before);
  } finally {
    if (replaced) { await fsp.rmdir(settings); await fsp.rename(preserved, settings); }
    await app.dispose();
  }
});

test('local trusted proxy simulation gives visitors independent browsing counters', async ({ request }) => {
  const app = await RustChanServer.create(undefined, { env: { CHAN_BEHIND_PROXY: '1', CHAN_TRUSTED_PROXY_CIDRS: '127.0.0.1/32', CHAN_RATE_GETS: '3', CHAN_RATE_WINDOW: '600' } });
  try {
    app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
    await app.start();
    const visitor = (ip: string) => request.get(`${app.baseURL}/healthz`, { headers: { 'X-Real-IP': ip } });
    for (let count = 0; count < 3; count += 1) expect((await visitor('198.51.100.20')).status()).toBe(200);
    expect((await visitor('198.51.100.20')).status()).toBe(429);
    expect((await visitor('198.51.100.21')).status()).toBe(200);
  } finally { await app.dispose(); }
});

test('local untrusted peer cannot split its counter with spoofed forwarding headers', async ({ request }) => {
  const app = await RustChanServer.create(undefined, { env: { CHAN_BEHIND_PROXY: '1', CHAN_TRUSTED_PROXY_CIDRS: '192.0.2.0/24', CHAN_RATE_GETS: '6', CHAN_RATE_WINDOW: '600' } });
  try {
    app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
    await app.start();
    const statuses: number[] = [];
    for (let count = 0; count < 7; count += 1) {
      const response = await request.get(`${app.baseURL}/healthz`, { headers: { 'X-Real-IP': `198.51.100.${count + 1}`, 'X-Forwarded-For': `203.0.113.${count + 1}` } });
      statuses.push(response.status());
    }
    expect(statuses).toContain(200);
    expect(statuses[statuses.length - 1]).toBe(429);
  } finally { await app.dispose(); }
});
