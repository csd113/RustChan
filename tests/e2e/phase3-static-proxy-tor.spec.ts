import type { APIResponse } from '@playwright/test';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  RustChanServer,
  adminCsrf,
  adminLogin,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  extractCsrf,
  test,
} from './helpers';

const CHROMIUM_ONLY = new Set(['chromium']);

test.describe('phase 3 static and runtime headers', () => {
  test('static assets, dynamic pages, media, and missing assets use safe cache and framing headers', async ({
    page,
    app,
  }, testInfo) => {
    test.skip(!CHROMIUM_ONLY.has(testInfo.project.name), 'static/runtime header matrix runs once in Chromium');

    const home = await page.request.get(app.baseURL);
    expect(home.status()).toBe(200);
    expect(home.headers()['content-security-policy']).toContain("frame-ancestors 'none'");
    expect(home.headers()['x-frame-options']).toBe('SAMEORIGIN');
    expect(home.headers()['x-content-type-options']).toBe('nosniff');
    expect(home.headers()['cache-control']).toContain('no-cache');

    const html = await home.text();
    const staticUrls = Array.from(html.matchAll(/(?:href|src)="(\/static\/[^"]+)"/g)).map((match) => match[1]);
    expect(staticUrls.length).toBeGreaterThanOrEqual(3);
    for (const url of staticUrls) {
      const response = await page.request.get(`${app.baseURL}${url}`);
      expect(response.status(), url).toBe(200);
      expect(response.headers()['x-content-type-options']).toBe('nosniff');
      expect(response.headers()['cache-control'], url).toContain('immutable');
      if (url.endsWith('.css') || url.includes('.css?')) {
        expect(response.headers()['content-type']).toContain('text/css');
      }
      if (url.endsWith('.js') || url.includes('.js?')) {
        expect(response.headers()['content-type']).toContain('application/javascript');
      }
    }

    const unversionedCss = await page.request.get(`${app.baseURL}/static/style.css`);
    expect(unversionedCss.status()).toBe(200);
    expect(unversionedCss.headers()['cache-control']).not.toContain('immutable');
    const unversionedJs = await page.request.get(`${app.baseURL}/static/main.js`);
    expect(unversionedJs.status()).toBe(200);
    expect(unversionedJs.headers()['cache-control']).not.toContain('immutable');

    const admin = await page.request.get(`${app.baseURL}/admin`);
    expect(admin.status()).toBe(200);
    expect(admin.headers()['cache-control']).toContain('no-store');
    expect(admin.headers()['content-security-policy']).toContain("frame-ancestors 'none'");

    const threadId = await createThread(page, app, 'img', {
      subject: 'phase 3 runtime media headers',
      body: 'media cache and framing headers',
      filePath: app.fixtures().tinyPng,
    });
    const mediaHref = await page.locator('.file-info a[href^="/boards/"]').first().getAttribute('href');
    expect(mediaHref).toBeTruthy();
    const media = await page.request.get(`${app.baseURL}${mediaHref}`);
    expect(media.status()).toBe(200);
    expect(media.headers()['cache-control']).toContain('immutable');
    expect(media.headers()['x-content-type-options']).toBe('nosniff');

    await page.goto(`${app.baseURL}/img/thread/${threadId}`);
    await expectSafePage(page);

    const missingStatic = await page.request.get(`${app.baseURL}/static/phase3-missing.js`);
    expect([404, 405]).toContain(missingStatic.status());
    expect(missingStatic.headers()['x-content-type-options']).toBe('nosniff');
    await expectNoSecretLeak(missingStatic, app);

    const missingMedia = await page.request.get(`${app.baseURL}/boards/img/phase3-missing-media.png`);
    expect(missingMedia.status()).toBe(404);
    expect(missingMedia.headers()['x-content-type-options']).toBe('nosniff');
    await expectNoSecretLeak(missingMedia, app);
  });
});

test.describe('phase 3 proxy, public-host, and secure-cookie checks', () => {
  test('trusted forwarded HTTPS sets secure admin cookies and HSTS only for configured public hosts', async ({
    page,
  }, testInfo) => {
    test.skip(!CHROMIUM_ONLY.has(testInfo.project.name), 'request-level proxy/TLS checks run once in Chromium');

    const proxyApp = await RustChanServer.create(undefined, {
      env: {
        CHAN_BEHIND_PROXY: '1',
        CHAN_HTTPS_COOKIES: '1',
        CHAN_PUBLIC_HOSTS: 'example.test',
        CHAN_TRUSTED_PROXY_CIDRS: '127.0.0.1/32',
      },
    });
    try {
      await proxyApp.initializeDefaultData();
      await proxyApp.start();

      const publicHost = await page.request.get(`${proxyApp.baseURL}/admin`, {
        headers: {
          Host: 'example.test',
          'x-forwarded-proto': 'https',
        },
      });
      expect(publicHost.status()).toBe(200);
      expect(publicHost.headers()['strict-transport-security']).toContain('max-age=31536000');
      const csrfCookie = setCookiePair(publicHost, 'csrf_token=');
      const csrf = extractCsrf(await publicHost.text());
      const login = await page.request.post(`${proxyApp.baseURL}/admin/login`, {
        form: {
          username: ADMIN_USERNAME,
          password: ADMIN_PASSWORD,
          _csrf: csrf,
        },
        headers: {
          Host: 'example.test',
          Cookie: csrfCookie,
          Origin: 'https://example.test',
          Referer: 'https://example.test/admin',
          'x-forwarded-proto': 'https',
        },
        maxRedirects: 0,
      });
      expect(login.status()).toBe(303);
      expect(setCookieHeader(login, 'chan_admin_session=')).toMatch(/;\s*Secure/i);

      const loopbackHost = await page.request.get(`${proxyApp.baseURL}/admin`, {
        headers: {
          Host: '127.0.0.1',
          'x-forwarded-proto': 'https',
        },
      });
      expect(loopbackHost.status()).toBe(200);
      expect(loopbackHost.headers()['strict-transport-security'] ?? '').toBe('');

      const plainLoginPage = await page.request.get(`${proxyApp.baseURL}/admin`, {
        headers: { Host: 'example.test' },
      });
      const plainCsrfCookie = setCookiePair(plainLoginPage, 'csrf_token=');
      const plainLogin = await page.request.post(`${proxyApp.baseURL}/admin/login`, {
        form: {
          username: ADMIN_USERNAME,
          password: ADMIN_PASSWORD,
          _csrf: extractCsrf(await plainLoginPage.text()),
        },
        headers: {
          Host: 'example.test',
          Cookie: plainCsrfCookie,
          Origin: `http://example.test`,
          Referer: `http://example.test/admin`,
        },
        maxRedirects: 0,
      });
      expect(plainLogin.status()).toBe(303);
      expect(setCookieHeader(plainLogin, 'chan_admin_session=')).not.toMatch(/;\s*Secure/i);
    } finally {
      await proxyApp.dispose();
    }
  });

  test('local TLS listener, self-signed browser trust, and HTTP redirect host rejection remain opt-in', async ({}, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'manual note is attached to the Chromium phase-3 run only');
    test.skip(
      true,
      'requires deterministic local TLS ports/cert trust or redirect listener config; covered by Rust unit tests and should be run manually in release validation',
    );
  });
});

test.describe('phase 3 Tor and onion UI checks', () => {
  test('Tor-disabled runtime keeps home/admin layouts stable and hides Tor-key backup controls', async ({
    page,
    app,
  }, testInfo) => {
    test.skip(!CHROMIUM_ONLY.has(testInfo.project.name), 'Tor-disabled UI check runs once in Chromium');

    await page.goto(app.baseURL);
    await expect(page.locator('.index-onion-section')).toHaveCount(0);
    await expectSafePage(page);

    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=full-backup-restore#full-backup-restore`);
    await expect(page.locator('input[name="include_tor_hidden_service_keys"]')).toHaveCount(0);
    await expect(page.locator('input[name="restore_tor_hidden_service_keys"]')).toHaveCount(0);
    await expect(page.locator('body')).toContainText(/Tor bootstrap state|Tor/i);
    await expectSafePage(page, { allowAdminInternals: true });

    const csrf = await adminCsrf(page, app);
    const backupSettings = await page.request.post(`${app.baseURL}/admin/backup/settings`, {
      form: {
        _csrf: csrf,
        auto_full_backup_interval_hours: '0',
        auto_full_backup_copies_to_keep: '2',
        auto_full_backup_storage_mode: 'directory',
      },
      maxRedirects: 0,
    });
    expect(backupSettings.status()).toBe(303);
  });

  test('active onion pill, copy behavior, Onion-Location, and already-onion suppression remain opt-in', async ({}, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'manual note is attached to the Chromium phase-3 run only');
    test.skip(
      true,
      'requires starting Arti/Tor and waiting for a live onion address; default e2e keeps Tor disabled for deterministic local runs',
    );
  });
});

async function expectNoSecretLeak(response: APIResponse, app: { dataDir: string }): Promise<void> {
  const body = await expectSafeResponse(response);
  expect(body).not.toContain(app.dataDir);
  expect(body).not.toMatch(/\/Users\/|target\/debug|cookie_secret|CHAN_/i);
}

function setCookiePair(response: APIResponse, prefix: string): string {
  return setCookieHeader(response, prefix).split(';')[0];
}

function setCookieHeader(response: APIResponse, prefix: string): string {
  const header = response
    .headersArray()
    .filter((item) => item.name.toLowerCase() === 'set-cookie')
    .map((item) => item.value)
    .find((value) => value.startsWith(prefix));
  if (!header) {
    throw new Error(`set-cookie ${prefix} not found`);
  }
  return header;
}
