import { expect, test } from '@playwright/test';
import { ADMIN_PASSWORD, ADMIN_USERNAME, adminLogin, RustChanServer } from './helpers';

function expectHeadlessOutput(logs: string): void {
  // A redirected process must not emit Ratatui cursor/alternate-screen/paste
  // sequences or block on the interactive first-run form. Do not strip ANSI:
  // that would hide a regression in the terminal ownership guard.
  expect(logs).not.toMatch(/[\u001b\u009b]/);
  expect(logs).not.toContain('First-run setup');
  expect(logs).not.toContain('RustChan console');
  expect(logs).not.toMatch(/Device not configured|Failed to render console frame/);
}

test.describe('terminal console / browser boundary', () => {
  test.beforeEach(async ({}, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'headless process coverage runs once in Chromium');
  });

  test('fresh headless startup keeps web setup available without terminal prompts', async ({ page, request }) => {
    const app = await RustChanServer.create(undefined, {
      // Inherited terminal capability/size variables must not enable the TUI
      // when the child streams are redirected.
      env: { RUST_LOG: 'info', TERM: 'xterm-256color', COLUMNS: '40', LINES: '10' },
    });
    try {
      await app.start();
      expect(app.process?.stdin).toBeNull();
      expect((await request.get(`${app.baseURL}/readyz`)).status()).toBe(200);
      await page.goto(`${app.baseURL}/setup`);
      await expect(page.getByRole('heading', { name: 'RustChan setup' })).toBeVisible();
      await expect.poll(() => app.logs()).toContain('No admin accounts exist');
      expectHeadlessOutput(await app.logs());
    } finally {
      await app.dispose();
    }
  });

  test('CLI-seeded administration survives headless restart with readable logs', async ({ page, request }) => {
    const app = await RustChanServer.create(undefined, { env: { RUST_LOG: 'info' } });
    try {
      app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
      app.createBoardCli({ short: 'pub', name: 'Headless Board' });
      await app.start();
      const firstChild = app.process;
      expect(firstChild).toBeDefined();
      await adminLogin(page, app);
      await expect(page.locator('#board-pub')).toContainText('Headless Board');

      await app.restart();

      expect(firstChild?.exitCode).toBe(0);
      expect(firstChild?.signalCode).toBeNull();
      expect(app.process?.pid).not.toBe(firstChild?.pid);
      expect((await request.get(`${app.baseURL}/readyz`)).status()).toBe(200);
      await page.goto(`${app.baseURL}/pub`);
      await expect(page.locator('body')).toContainText('Headless Board');
      await adminLogin(page, app);
      await expect(page.locator('#board-pub')).toContainText('Headless Board');

      await app.stop();
      const logs = await app.logs();
      expect(logs).toContain('HTTP server listening');
      expect(logs).not.toContain('No admin accounts exist');
      expectHeadlessOutput(logs);
    } finally {
      await app.dispose();
    }
  });
});
