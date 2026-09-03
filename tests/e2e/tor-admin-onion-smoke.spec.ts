import {
  chromium,
  expect,
  request,
  test,
  type APIRequestContext,
  type APIResponse,
  type Page,
  type Response,
} from '@playwright/test';
import net from 'node:net';

const RUN_ENV = 'RUSTCHAN_TOR_E2E';
const ONION_URL_ENV = 'RUSTCHAN_ONION_URL';
const TOR_PROXY_ENV = 'RUSTCHAN_TOR_PROXY';
const ADMIN_USER_ENV = 'RUSTCHAN_UPLOAD_ADMIN_USERNAME';
const ADMIN_PASSWORD_ENV = 'RUSTCHAN_UPLOAD_ADMIN_PASSWORD';
const DEFAULT_TOR_PROXY = 'socks5://127.0.0.1:9050';
const SPOOFED_ONION_HOST = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.onion';
const ALT_SPOOFED_ONION_HOST = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.onion';
const TOR_NAVIGATION_TIMEOUT_MS = 120_000;
const TOR_REQUEST_TIMEOUT_MS = 120_000;

test.describe('Tor onion admin smoke (local opt-in)', () => {
  test.skip(process.env[RUN_ENV] !== '1', `set ${RUN_ENV}=1 to run the local Tor/onion smoke test`);

  test('admin login and same-origin checks work over plain onion HTTP', async ({}, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'Tor/onion smoke runs once in Chromium');
    test.setTimeout(240_000);

    const rawBaseURL = process.env[ONION_URL_ENV] ?? '';
    test.skip(
      rawBaseURL.length === 0,
      `set ${ONION_URL_ENV}=http://<service>.onion to run the Tor/onion admin smoke test`,
    );

    const adminUsername = process.env[ADMIN_USER_ENV] ?? '';
    const adminPassword = process.env[ADMIN_PASSWORD_ENV] ?? '';
    test.skip(
      adminUsername.length === 0 || adminPassword.length === 0,
      `set ${ADMIN_USER_ENV} and ${ADMIN_PASSWORD_ENV} for onion admin credentials; these are the existing external-target E2E credential env vars`,
    );

    const baseURL = normalizeOnionBaseURL(rawBaseURL);
    const torProxy = process.env[TOR_PROXY_ENV] ?? DEFAULT_TOR_PROXY;
    const proxyEndpoint = parseSocksProxyEndpoint(torProxy);

    await expect(
      tcpReachable(proxyEndpoint.host, proxyEndpoint.port, 5_000),
      `${TOR_PROXY_ENV}=${torProxy} is not reachable. Start Tor or set ${TOR_PROXY_ENV} to a reachable SOCKS proxy.`,
    ).resolves.toBe(true);

    const browser = await chromium.launch({
      proxy: { server: torProxy },
      timeout: 60_000,
    });
    let api: APIRequestContext | undefined;

    try {
      const context = await browser.newContext();
      context.setDefaultTimeout(60_000);
      context.setDefaultNavigationTimeout(TOR_NAVIGATION_TIMEOUT_MS);
      const page = await context.newPage();

      const loginResponse = await gotoWithDiagnostics(page, `${baseURL}/admin`, torProxy);
      assertPlainOnionHTTP(loginResponse.url(), 'admin login response URL');
      assertPlainOnionHTTP(page.url(), 'admin login page URL');
      expect(loginResponse.status(), 'admin login page should load').toBeLessThan(500);
      await expect(page.getByRole('heading', { name: /admin login/i })).toBeVisible({ timeout: 30_000 });

      await page.getByLabel('Username').fill(adminUsername);
      await page.getByLabel('Password').fill(adminPassword);
      await Promise.all([
        page.waitForURL(/\/admin\/panel/, { timeout: TOR_NAVIGATION_TIMEOUT_MS }),
        page.getByRole('button', { name: 'authenticate' }).click(),
      ]).catch(async (error: unknown) => {
        const body = await page.locator('body').innerText({ timeout: 5_000 }).catch(() => '');
        throw new Error(
          [
            `Admin login did not reach /admin/panel over ${baseURL}.`,
            `Check ${ADMIN_USER_ENV}/${ADMIN_PASSWORD_ENV} and the onion service admin account.`,
            `Current URL: ${page.url()}`,
            `Page body: ${body.slice(0, 1_000)}`,
            `Original error: ${error instanceof Error ? error.message : String(error)}`,
          ].join('\n'),
        );
      });

      assertPlainOnionHTTP(page.url(), 'post-login admin URL');
      await expect(page.locator('body')).toContainText(/admin panel/i, { timeout: 30_000 });

      const cookies = await context.cookies(baseURL);
      const session = cookies.find((cookie) => cookie.name === 'chan_admin_session');
      expect(session, 'chan_admin_session cookie should be set on the onion origin').toBeTruthy();
      expect(session?.httpOnly, 'chan_admin_session should remain HttpOnly').toBe(true);
      expect(session?.sameSite, 'chan_admin_session should remain SameSite=Lax').toBe('Lax');
      expect(session?.secure, 'plain http://*.onion admin session cookie must not be marked Secure').toBe(false);

      api = await request.newContext({
        baseURL,
        proxy: { server: torProxy },
        storageState: await context.storageState(),
        timeout: TOR_REQUEST_TIMEOUT_MS,
      });

      const sessionCheck = await apiGetWithDiagnostics(api, '/admin/panel', baseURL, torProxy);
      expect(sessionCheck.status(), 'stored admin session cookie should authorize /admin/panel on onion').toBe(200);
      const panelHTML = await sessionCheck.text();
      expect(panelHTML, 'admin panel should render with the onion session cookie').toMatch(/admin panel/i);
      const csrf = extractCsrf(panelHTML);

      const badOrigin = spoofedOnionOrigin(baseURL);
      const rejected = await api.post('/admin/logout', {
        form: logoutForm(csrf),
        headers: {
          Origin: badOrigin,
          Referer: `${badOrigin}/admin/panel`,
        },
        maxRedirects: 0,
        timeout: TOR_REQUEST_TIMEOUT_MS,
      });
      expect(rejected.status(), 'mismatched onion Origin/Referer must still be rejected').toBe(403);

      const accepted = await api.post('/admin/logout', {
        form: logoutForm(csrf),
        headers: {
          Origin: baseURL,
          Referer: `${baseURL}/admin/panel`,
        },
        maxRedirects: 0,
        timeout: TOR_REQUEST_TIMEOUT_MS,
      });
      expect(accepted.status(), 'same-origin onion admin POST should be accepted').toBe(303);
    } finally {
      await api?.dispose();
      await browser.close();
    }
  });
});

function normalizeOnionBaseURL(raw: string): string {
  let parsed: URL;
  try {
    parsed = new URL(raw);
  } catch (error) {
    throw new Error(`${ONION_URL_ENV} must be an absolute URL like http://<service>.onion: ${String(error)}`);
  }

  if (parsed.protocol !== 'http:') {
    throw new Error(`${ONION_URL_ENV} must use plain http:// for the onion smoke test, got ${parsed.protocol}`);
  }
  if (!parsed.hostname.endsWith('.onion')) {
    throw new Error(`${ONION_URL_ENV} must point at a .onion host, got ${parsed.hostname}`);
  }
  return parsed.origin;
}

function assertPlainOnionHTTP(value: string, label: string): void {
  const parsed = new URL(value);
  expect(parsed.protocol, `${label} must remain plain http://`).toBe('http:');
  expect(parsed.hostname, `${label} must be a .onion host`).toMatch(/\.onion$/);
}

function parseSocksProxyEndpoint(proxy: string): { host: string; port: number } {
  let parsed: URL;
  try {
    parsed = new URL(proxy);
  } catch (error) {
    throw new Error(`${TOR_PROXY_ENV} must be a SOCKS proxy URL like ${DEFAULT_TOR_PROXY}: ${String(error)}`);
  }

  if (!['socks4:', 'socks5:'].includes(parsed.protocol)) {
    throw new Error(`${TOR_PROXY_ENV} must use a SOCKS proxy scheme, got ${parsed.protocol}`);
  }

  const port = Number(parsed.port || '9050');
  if (!Number.isInteger(port) || port <= 0 || port > 65_535) {
    throw new Error(`${TOR_PROXY_ENV} must include a valid TCP port, got ${parsed.port || '(empty)'}`);
  }

  return {
    host: parsed.hostname.replace(/^\[|\]$/g, ''),
    port,
  };
}

async function tcpReachable(host: string, port: number, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host, port, timeout: timeoutMs }, () => {
      socket.destroy();
      resolve(true);
    });
    socket.once('error', () => {
      socket.destroy();
      resolve(false);
    });
    socket.once('timeout', () => {
      socket.destroy();
      resolve(false);
    });
  });
}

async function gotoWithDiagnostics(page: Page, url: string, torProxy: string): Promise<Response> {
  let response: Response | null;
  try {
    response = await page.goto(url, {
      waitUntil: 'domcontentloaded',
      timeout: TOR_NAVIGATION_TIMEOUT_MS,
    });
  } catch (error) {
    throw new Error(
      [
        `Unable to reach onion service at ${url}.`,
        `Tor proxy: ${torProxy}.`,
        `Check that Tor is running, ${ONION_URL_ENV} is correct, and the onion service is reachable.`,
        `Original error: ${error instanceof Error ? error.message : String(error)}`,
      ].join('\n'),
    );
  }

  if (!response) {
    throw new Error(`Navigation to ${url} completed without a response through ${torProxy}`);
  }
  return response;
}

async function apiGetWithDiagnostics(
  api: APIRequestContext,
  path: string,
  baseURL: string,
  torProxy: string,
): Promise<APIResponse> {
  try {
    return await api.get(path, {
      timeout: TOR_REQUEST_TIMEOUT_MS,
    });
  } catch (error) {
    throw new Error(
      [
        `Unable to request ${baseURL}${path} through Tor API context.`,
        `Tor proxy: ${torProxy}.`,
        `Check that the onion service is still reachable and that Playwright API requests are using the SOCKS proxy.`,
        `Original error: ${error instanceof Error ? error.message : String(error)}`,
      ].join('\n'),
    );
  }
}

function extractCsrf(html: string): string {
  const match = html.match(/name="_csrf"\s+value="([^"]+)"/);
  if (!match) {
    throw new Error('Admin CSRF token not found on /admin/panel');
  }
  return decodeHtml(match[1]);
}

function decodeHtml(value: string): string {
  return value
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>');
}

function logoutForm(csrf: string): Record<string, string> {
  return {
    _csrf: csrf,
    return_to: '/admin',
  };
}

function spoofedOnionOrigin(baseURL: string): string {
  const parsed = new URL(baseURL);
  const spoofedHost = parsed.hostname === SPOOFED_ONION_HOST ? ALT_SPOOFED_ONION_HOST : SPOOFED_ONION_HOST;
  return `${parsed.protocol}//${spoofedHost}${parsed.port ? `:${parsed.port}` : ''}`;
}
