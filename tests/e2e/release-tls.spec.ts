import { newAuditedContext } from './diagnostics';
import { test, expect } from './diagnostics';
import { RustChanServer, ADMIN_USERNAME, ADMIN_PASSWORD, sqliteQuery } from './helpers';
import fs from 'node:fs/promises';
import path from 'node:path';
import net from 'node:net';

async function freePort() {
  const socket = net.createServer();
  await new Promise<void>((resolve, reject) => { socket.once('error', reject); socket.listen(0, '127.0.0.1', resolve); });
  const port = (socket.address() as net.AddressInfo).port;
  await new Promise<void>((resolve, reject) => socket.close(e => e ? reject(e) : resolve()));
  return port;
}

test('release: native HTTPS login, posting, session restoration and logout remain secure', async ({ browser, javaScriptEnabled }, testInfo) => {
  const app = await RustChanServer.create(undefined, { env: { CHAN_HTTPS_COOKIES: '1' } });
  const tlsPort = await freePort();
  const redirectPort = await freePort();
  const baseURL = `https://127.0.0.1:${tlsPort}`;
  const { viewport, deviceScaleFactor, isMobile, hasTouch, userAgent } = testInfo.project.use;
  const browserOptions = { ignoreHTTPSErrors: true, javaScriptEnabled, viewport, deviceScaleFactor, isMobile, hasTouch, userAgent };
  const context = await newAuditedContext(browser, browserOptions);
  const ready = async child => {
    await expect.poll(async () => {
      if (child.exitCode !== null) throw new Error(`TLS server exited: ${await app.logs()}`);
      try { return (await context.request.get(`${baseURL}/readyz`, { timeout: 1000 })).status(); }
      catch { return 0; }
    }, { timeout: 20000 }).toBe(200);
  };
  try {
    await app.initializeDefaultData();
    const settings = path.join(app.dataDir, 'settings.toml');
    await fs.writeFile(settings, (await fs.readFile(settings, 'utf8')).replace(/\[tls\][\s\S]*$/, `[tls]\nenabled = true\nrequire_https = true\nport = ${tlsPort}\nredirect_http = true\nhttp_port = ${redirectPort}\n`));
    await app.start({ ready });
    const page = await context.newPage();
    const errors: string[] = []; page.on('pageerror', e => errors.push(e.message));
    await page.goto(`${baseURL}/admin`);
    await page.getByLabel('Username').fill(ADMIN_USERNAME);
    await page.getByLabel('Password').fill(ADMIN_PASSWORD);
    await page.getByRole('button', { name: 'authenticate' }).click();
    await expect(page).toHaveURL(`${baseURL}/admin/panel`);
    const cookie = (await context.cookies()).find(c => c.name === 'chan_admin_session');
    expect(cookie?.secure).toBe(true); expect(cookie?.httpOnly).toBe(true); expect(cookie?.sameSite).toBe('Lax');
    const state = await context.storageState();
    await page.goto(`${baseURL}/pub`);
    const toggle = page.locator('[data-action="toggle-post-form"]').first();
    if (await toggle.isVisible()) await toggle.click();
    const form = page.locator('form[action="/pub"]');
    await form.getByLabel('body', { exact: true }).fill('native HTTPS browser post');
    await form.getByRole('button', { name: /post thread/i }).click();
    await expect(page).toHaveURL(/\/pub\/thread\/\d+/);
    await expect(page.locator('.post')).toContainText('native HTTPS browser post');
    await app.stop(); await app.start({ ready });
    const restored = await newAuditedContext(browser, { ...browserOptions, storageState: state });
    try {
      const tab = await restored.newPage();
      await tab.goto(`${baseURL}/admin/panel`);
      await expect(tab.locator('form[action="/admin/logout"]')).toBeVisible();
      await tab.locator('form[action="/admin/logout"]').getByRole('button', { name: /logout/i }).click();
      await expect(tab).toHaveURL(/\/admin/);
      const denied = await context.request.get(`${baseURL}/admin/panel`, { maxRedirects: 0 });
      expect(denied.status()).toBe(403);
    } finally { await restored.close(); }
    expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('1');
    const redirect = await context.request.get(`http://127.0.0.1:${redirectPort}/pub?audit=1`, { headers: { Host: 'untrusted.invalid' }, maxRedirects: 0 });
    expect(redirect.status()).toBe(308);
    expect(redirect.headers().location).toBe(`${baseURL}/pub?audit=1`);
    await expect(context.request.get(`${app.baseURL}/pub`, { timeout: 2000 })).rejects.toThrow(/ECONNREFUSED/);
    expect(errors).toEqual([]);
  } finally {
    if (testInfo.status !== testInfo.expectedStatus) await testInfo.attach('tls-server.log', { body: await app.logs(), contentType: 'text/plain' });
    await context.close(); await app.dispose();
  }
});
