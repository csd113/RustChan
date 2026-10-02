import { expectHttpError, newAuditedContext } from './diagnostics';
import { test, expect, adminLogin, createStandaloneApp, sqliteQuery, isNoJsProject } from './helpers';
import fsp from 'node:fs/promises';
import path from 'node:path';

// Shares the repository's disposable local runtime contract and no-JS projects.
test('Tor, media, schedules and system controls save supported settings with pending restart state', async ({ page }) => {
  const app = await createStandaloneApp({ admin: true });
  try {
    await adminLogin(page, app);
    const cases = [
      { section: 'tor', field: 'tor_bootstrap_timeout_secs', value: '300', entries: 5 },
      { section: 'media', field: 'thumb_size', value: '512', entries: 16 },
      { section: 'schedules', field: 'db_warn_threshold_mb', value: '3000', entries: 4 },
      { section: 'system', field: 'db_pool_size', value: '12', entries: 2 },
    ];
    for (const item of cases) {
      await page.goto(`${app.baseURL}/admin/panel#${item.section}`);
      const section = page.locator(`#${item.section}`);
      await expect(section.locator('.admin-setting')).toHaveCount(item.entries);
      await section.locator(`[name=${item.field}]`).fill(item.value);
      await section.getByRole('button', { name: 'Save for next restart' }).click();
      await expect(page.locator('.admin-panel > .admin-flash')).toContainText('Restart required');
      await expect(page.locator(`#state-${item.field}`)).toContainText('restart required');
    }
    const bytes = await fsp.readFile(path.join(app.dataDir, 'settings.toml'), 'utf8');
    for (const item of cases) expect(bytes).toContain(`${item.field} = ${item.value}`);
    expect(bytes).toContain('blocking_threads = 0');
    await app.restart();
    await page.goto(`${app.baseURL}/admin/panel#media`);
    await expect(page.locator('#state-thumb_size')).toContainText('Saved configuration matches');
    await expect(page.locator('#setting-thumb_size')).toHaveValue('512');
  } finally { await app.dispose(); }
});

test('settings search finds operator synonyms through real GET forms', async ({ page }) => {
  const app = await createStandaloneApp({ admin: true });
  try {
    await adminLogin(page, app);
    for (const [query, target] of [['Serveo', 'setting-behind_proxy'], ['proxy', 'setting-trusted_proxy_cidrs'], ['slow down', 'setting-rate_limit_gets'], ['upload limit', 'setting-max_image_size_mb']]) {
      await page.locator('#admin-settings-query').fill(query);
      await page.getByRole('button', { name: 'Search settings' }).click();
      const link = page.locator(`.admin-settings-search-results a[href$="#${target}"]`);
      await expect(link).toBeVisible();
      await link.click();
      await expect(page.locator(`#${target}`)).toBeVisible();
    }
  } finally { await app.dispose(); }
});

test('new-board upload defaults require restart and do not rewrite existing board caps', async ({ page }) => {
  const app = await createStandaloneApp({ admin: true, boards: [{ short: 'existing', name: 'Existing' }] });
  try {
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel#media`);
    await page.locator('#setting-max_image_size_mb').fill('12');
    await page.locator('#admin-media-settings button[type=submit]').click();
    await expect(page.locator('#state-max_image_size_mb')).toContainText('restart required');
    const create = async (short: string) => {
      await page.goto(`${app.baseURL}/admin/panel?open=boards#boards`);
      const form = page.locator('form.admin-board-create-form');
      await form.locator('[name=short_name]').fill(short);
      await form.locator('[name=name]').fill(short);
      await form.getByRole('button', { name: 'create', exact: true }).click();
      await expect(page.locator('.admin-panel')).toBeVisible();
    };
    await create('before');
    expect(sqliteQuery(app, "SELECT max_image_size FROM boards WHERE short_name='before';")).toBe(String(8 * 1024 * 1024));
    await app.restart();
    await create('after');
    expect(sqliteQuery(app, "SELECT max_image_size FROM boards WHERE short_name='after';")).toBe(String(12 * 1024 * 1024));
    expect(sqliteQuery(app, "SELECT max_image_size FROM boards WHERE short_name='existing';")).toBe(String(8 * 1024 * 1024));
  } finally { await app.dispose(); }
});

test('new network controls remain usable in every built-in theme on desktop and mobile', async ({ page, browser }, testInfo) => {
  const app = await createStandaloneApp({ admin: true });
  try {
    await adminLogin(page, app);
    const storageState = await page.context().storageState();
    // Fixed-size contexts exercise actual desktop/mobile and no-JS rendering
    // without Playwright's disabled-script document timing after viewport resize.
    for (const [layout, viewport] of [['desktop', { width: 1280, height: 900 }], ['mobile', { width: 390, height: 844 }]] as const) {
      const context = await newAuditedContext(browser, { viewport, javaScriptEnabled: !isNoJsProject(testInfo), storageState });
      try {
        const themed = await context.newPage();
        for (const theme of ['forest', 'blue-sky', 'deep-orbit', 'terminal', 'dorfic', 'chanclassic', 'aero', 'neoncubicle', 'fluorogrid']) {
          await themed.goto(`${app.baseURL}/theme/${theme}?return_to=${encodeURIComponent('/admin/panel#network-security')}`);
          await expect(themed.locator('html')).toHaveAttribute('data-active-theme', theme);
          await themed.locator('#network-security-title').scrollIntoViewIfNeeded();
          await expect(themed.locator('#setting-behind_proxy')).toBeVisible();
          expect(await themed.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
          await themed.screenshot({ path: testInfo.outputPath(`network-${theme}-${layout}.png`) });
        }
      } finally { await context.close(); }
    }
  } finally { await app.dispose(); }
});

test('rejected runtime forms preserve the selected theme, entered fields and saved settings', async ({ page }) => {
  const app = await createStandaloneApp({ admin: true });
  try {
    await adminLogin(page, app);
    const settings = path.join(app.dataDir, 'settings.toml');
    const before = await fsp.readFile(settings, 'utf8');
    for (const theme of ['blue-sky', 'deep-orbit']) {
      await page.goto(`${app.baseURL}/theme/${theme}?return_to=/admin/panel%23tor`);
      await page.locator('#setting-tor_service_nickname').fill('invalid nickname');
      expectHttpError({ method: 'POST', path: '/admin/config/tor', status: 422, reason: 'Invalid onion-service nickname must be rejected without mutation.' });
      await page.locator('#admin-tor-settings button[type=submit]').click();
      await expect(page.getByRole('alert')).toContainText('No changes saved');
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(page.locator('#setting-tor_service_nickname')).toHaveValue('invalid nickname');
      expect(await fsp.readFile(settings, 'utf8')).toBe(before);
    }
  } finally { await app.dispose(); }
});
