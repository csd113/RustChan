import { test, expect, Page } from '@playwright/test';
import { spawn, spawnSync, ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';

// A fixed executable and a disposable data directory simulate Docker's restart policy.
// The HTTP process cannot select or spawn its replacement, and no host service is touched.
const binary = process.env.RUSTCHAN_E2E_BINARY || path.resolve('target/debug/rustchan-cli');
let directory: string;
let baseURL: string;
let child: ChildProcess;
let stopped: boolean;
let starts: number;
let output: string;

async function freePort(): Promise<number> {
  const server = net.createServer();
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address() as net.AddressInfo;
  await new Promise<void>((resolve) => server.close(() => resolve()));
  return address.port;
}

function launch() {
  if (stopped) return;
  starts += 1;
  child = spawn(binary, ['--data-dir', directory, 'serve'], {
    env: { ...process.env, CHAN_HOST: '127.0.0.1', CHAN_PORT: new URL(baseURL).port,
      CHAN_TOR_SUPPORT: 'false', RUSTCHAN_CONTAINER: '1', RUSTCHAN_RESTART_ON_EXIT: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout?.on('data', (chunk) => { output += chunk.toString(); });
  child.stderr?.on('data', (chunk) => { output += chunk.toString(); });
  child.on('exit', () => { if (!stopped) setTimeout(launch, 100); });
}

async function healthy(page: Page) {
  await expect.poll(async () => {
    try {
      const response = await page.request.get(`${baseURL}/readyz`, { timeout: 2000 });
      if (!response.ok()) return false;
      // Public health may be reached during initialization; the private observation is final.
      const login = await page.request.get(`${baseURL}/admin`, { timeout: 2000 });
      return login.ok() && fs.existsSync(path.join(directory, 'runtime/settings-restart/running.json'));
    } catch { return false; }
  }, { timeout: 50_000, message: `RustChan should reach readiness; logs: ${output}` }).toBe(true);
}

async function login(page: Page) {
  await page.goto(`${baseURL}/admin`);
  await page.locator('input[name=username]').fill('admin');
  await page.locator('input[name=password]').fill('restart-test-password');
  await page.locator('form[action="/admin/login"] button[type=submit]').click();
  await expect(page.locator('#settings-restart')).toBeVisible();
}

async function saveNetwork(page: Page, overrides: Record<string, string>) {
  await page.goto(`${baseURL}/admin/panel?open=network-security#network-security`);
  const form = page.locator('#admin-network-settings');
  const fields = await form.locator('input[name],select[name],textarea[name]').evaluateAll((controls) =>
    Object.fromEntries(controls.map((control) => [(control as HTMLInputElement).name, (control as HTMLInputElement).value])));
  const response = await page.request.post(`${baseURL}/admin/network/settings`, {
    form: { ...fields, ...overrides }, headers: { origin: baseURL },
  });
  expect(response.status(), await response.text()).toBe(200);
  await page.goto(`${baseURL}/admin/panel#settings-restart`);
}

async function restartAndWait(page: Page, previous: string) {
  // No-JS form navigation can overlap listener closure. Progress remains in the durable journal.
  await page.locator('#admin-restart-form button').click({ noWaitAfter: true });
  await expect.poll(() => {
    try {
      const record = JSON.parse(fs.readFileSync(path.join(directory, 'runtime/settings-restart/running.json'), 'utf8'));
      return record.instance !== previous;
    } catch { return false; }
  }, { timeout: 75_000 }).toBe(true);
  await healthy(page);
  if (test.info().project.use.javaScriptEnabled !== false) {
    // The enhanced UI must reconnect itself, without a test-driven navigation.
    await expect(page.locator('#settings-restart')).not.toHaveAttribute('data-restart-instance', previous);
  } else {
    await page.goto(`${baseURL}/admin/panel#settings-restart`);
    await page.reload();
  }
}

test.beforeEach(async ({ page }) => {
  directory = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'rustchan-restart-e2e-')));
  baseURL = `http://127.0.0.1:${await freePort()}`;
  starts = 0; output = ''; stopped = false;
  const admin = spawnSync(binary, ['--data-dir', directory, 'admin', 'create-admin', 'admin', 'restart-test-password'], {
    env: { ...process.env, CHAN_TOR_SUPPORT: 'false' }, encoding: 'utf8', timeout: 15_000,
  });
  expect(admin.status, `${admin.stdout}\n${admin.stderr}`).toBe(0);
  launch();
  await healthy(page);
  await login(page);
});

test.afterEach(async () => {
  stopped = true;
  if (child && child.exitCode === null) {
    const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()));
    child.kill('SIGTERM');
    await exited;
  }
  fs.rmSync(directory, { recursive: true, force: true });
});

test('no-op and live saves do not restart; pending changes persist across forms until verified restart', async ({ page }) => {
  await expect(page.locator('#admin-restart-form')).toHaveCount(0);
  await saveNetwork(page, {});
  await expect(page.locator('#admin-restart-form')).toHaveCount(0);
  expect(starts).toBe(1);
  await page.goto(`${baseURL}/admin/panel?open=appearance`);
  const liveForm = page.locator('form[action="/admin/site/settings"]').first();
  const liveFields = await liveForm.locator('input[name],select[name],textarea[name]').evaluateAll((controls) =>
    Object.fromEntries(controls.filter((control) => !(control instanceof HTMLInputElement && control.type === 'checkbox' && !control.checked))
      .map((control) => [(control as HTMLInputElement).name, (control as HTMLInputElement).value])));
  const live = await page.request.post(`${baseURL}/admin/site/settings`, { form: { ...liveFields, site_name: 'Live restart test' }, headers: { origin: baseURL } });
  expect(live.status(), await live.text()).toBe(200);
  await page.goto(`${baseURL}/admin/panel`);
  await expect(page.locator('#admin-restart-form')).toHaveCount(0);
  expect(starts).toBe(1);
  await saveNetwork(page, { rate_limit_gets: '1234' });
  await expect(page.locator('#admin-restart-status')).toContainText('Settings saved. Restart RustChan');
  await saveNetwork(page, { session_duration: '7200' });
  await expect(page.locator('#admin-restart-form')).toBeVisible();
  await page.locator('#settings-restart').screenshot({ path: test.info().outputPath('settings-restart.png') });
  expect(starts).toBe(1);
  const previous = await page.locator('#settings-restart').getAttribute('data-restart-instance');
  await restartAndWait(page, previous!);
  await expect(page.locator('#admin-restart-status')).toContainText('passed readiness verification');
  await expect(page.locator('#admin-restart-form')).toHaveCount(0);
  expect(starts).toBe(2);
});

test('failed listener startup restores the last healthy configuration and preserves admin access', async ({ page }) => {
  const occupied = net.createServer();
  await new Promise<void>((resolve) => occupied.listen(0, '127.0.0.1', resolve));
  const port = (occupied.address() as net.AddressInfo).port;
  try {
    // CHAN_HOST keeps the interface fixed; an explicit bind still selects its port.
    await saveNetwork(page, { bind_addr: `127.0.0.1:${port}` });
    const previous = await page.locator('#settings-restart').getAttribute('data-restart-instance');
    await restartAndWait(page, previous!);
    await expect(page.locator('#admin-restart-status')).toContainText('Previous configuration restored');
    await expect(page.locator('#admin-restart-form')).toHaveCount(0);
    expect(starts).toBe(3);
  } finally {
    await new Promise<void>((resolve) => occupied.close(() => resolve()));
  }
});
