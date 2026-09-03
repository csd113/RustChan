import { test, expect, type Page } from '@playwright/test';
import fsp from 'node:fs/promises';
import path from 'node:path';
import {
  ADMIN_PASSWORD,
  adminCsrf,
  adminLogin,
  adminPasswordHash,
  createStandaloneApp,
  createThread,
  expectSafeResponse,
  extractCsrf,
  publicCsrf,
  sqliteQuery,
  type RustChanServer,
} from './helpers';

async function finishFreshSetup(page: Page, app: RustChanServer, options: {
  board?: string;
  pdfLimitMiB?: string;
} = {}): Promise<void> {
  const board = options.board ?? 'b';
  await page.goto(`${app.baseURL}/setup`);
  await expect(page.getByRole('heading', { name: 'RustChan setup' })).toBeVisible();
  await page.getByLabel('Site name').fill('Setup RustChan');
  await page.getByLabel('Username').fill('admin');
  await page.locator('input[name="admin_password"]').fill(ADMIN_PASSWORD);
  await page.locator('input[name="admin_password_confirm"]').fill(ADMIN_PASSWORD);
  await page.locator('input[name="board_slug"]').fill(board);
  await page.locator('input[name="board_name"]').fill('PDF Board');
  await page.getByLabel('Allow PDF uploads').check();
  await page.getByLabel('PDF limit (MiB)').fill(options.pdfLimitMiB ?? '8');
  await page.locator('input[name="post_cooldown_secs"]').fill('0');
  await page.locator('input[name="allow_captcha"]').uncheck();
  await page.locator('select[name="captcha_type"]').selectOption('disabled');
  await page.getByRole('button', { name: 'review setup' }).click();
  await expect(page.getByRole('heading', { name: 'Review setup' })).toBeVisible();
  await expect(page.locator('body')).not.toContainText(ADMIN_PASSWORD);
  await page.getByRole('button', { name: 'finish setup' }).click();
  await expect(page).toHaveURL(new RegExp(`/${board}$`));
}

function setupFinishForm(csrf: string, board: string): Record<string, string> {
  return {
    _csrf: csrf,
    preset: 'public',
    site_name: 'Reopened RustChan',
    site_subtitle: '',
    default_theme: 'terminal',
    admin_username: 'replacement',
    admin_password: 'replacement-password',
    admin_password_confirm: 'replacement-password',
    board_slug: board,
    board_name: 'Reopened Board',
    board_description: '',
    board_visibility: 'public',
    allow_posting: '1',
    allow_uploads: '1',
    allow_pdf: '1',
    allow_video_embeds: '1',
    allow_thread_editing: '1',
    allow_self_delete: '1',
    allow_archive: '1',
    image_limit_mib: '8',
    video_limit_mib: '50',
    audio_limit_mib: '150',
    pdf_limit_mib: '3',
    captcha_type: 'builtin',
    post_cooldown_secs: '0',
    homepage_new_thread_badges_enabled: '1',
    homepage_new_reply_badges_enabled: '1',
    thread_new_reply_badges_enabled: '1',
    backup_retention: '1',
  };
}

async function withApp<T>(app: RustChanServer, run: () => Promise<T>): Promise<T> {
  try {
    return await run();
  } finally {
    await app.dispose();
  }
}

test.describe('first-run setup wizard', () => {
  test('fresh setup persists PDF/media limits and enforces PDF limit', async ({ page }) => {
    const app = await createStandaloneApp();
    await withApp(app, async () => {
      await finishFreshSetup(page, app, { board: 'pdf', pdfLimitMiB: '1' });

      expect(sqliteQuery(app, "SELECT max_pdf_size FROM boards WHERE short_name = 'pdf';"))
        .toBe(String(1024 * 1024));
      await createThread(page, app, 'pdf', {
        subject: 'pdf',
        body: 'small pdf upload',
        filePath: app.fixtures().tinyPdf,
      });
      await expect(page.locator('.pdf-container, .file-container')).toContainText('tiny.pdf');

      const largePdf = path.join(app.fixtureDir, 'large.pdf');
      await fsp.writeFile(largePdf, Buffer.concat([
        Buffer.from('%PDF-1.1\n'),
        Buffer.alloc(1024 * 1024 + 1, 0x20),
        Buffer.from('\n%%EOF\n'),
      ]));
      const csrf = await publicCsrf(page, app, '/pdf');
      const oversized = await page.request.post(`${app.baseURL}/pdf`, {
        multipart: {
          _csrf: csrf,
          submission_token: `large-pdf-${Date.now()}`,
          body: 'oversized pdf',
          file: {
            name: 'large.pdf',
            mimeType: 'application/pdf',
            buffer: await fsp.readFile(largePdf),
          },
        },
        maxRedirects: 0,
      });
      expect([413, 422]).toContain(oversized.status());
      expect(await oversized.text()).toMatch(/Maximum PDF upload size/i);
    });
  });

  test('initialized instance cannot access setup routes', async ({ page }) => {
    const app = await createStandaloneApp({ admin: true, boards: [{ short: 'pub', name: 'Public' }] });
    await withApp(app, async () => {
      const getSetup = await page.request.get(`${app.baseURL}/setup`);
      expect(getSetup.status()).toBe(404);
      await expectSafeResponse(getSetup);

      for (const route of ['review', 'finish']) {
        const response = await page.request.post(`${app.baseURL}/setup/${route}`, {
          form: setupFinishForm('bad', 'new'),
          maxRedirects: 0,
        });
        expect(response.status()).toBe(404);
        await expectSafeResponse(response);
      }
    });
  });

  test('admin can reopen setup and close it without changing settings', async ({ page }) => {
    const app = await createStandaloneApp({ admin: true, boards: [{ short: 'pub', name: 'Public' }] });
    await withApp(app, async () => {
      await adminLogin(page, app);
      const csrf = await adminCsrf(page, app);
      const reopen = await page.request.post(`${app.baseURL}/admin/setup/reopen`, {
        form: { _csrf: csrf },
        maxRedirects: 0,
      });
      expect(reopen.status()).toBe(303);

      await page.goto(`${app.baseURL}/setup`);
      await expect(page.getByText('Setup was reopened by an admin')).toBeVisible();

      const closeCsrf = await adminCsrf(page, app);
      const close = await page.request.post(`${app.baseURL}/admin/setup/close`, {
        form: { _csrf: closeCsrf },
        maxRedirects: 0,
      });
      expect(close.status()).toBe(303);
      const locked = await page.request.get(`${app.baseURL}/setup`);
      expect(locked.status()).toBe(404);
    });
  });

  test('reopened setup cannot replace existing admin credentials', async ({ page }) => {
    const app = await createStandaloneApp({ admin: true, boards: [{ short: 'pub', name: 'Public' }] });
    await withApp(app, async () => {
      const beforeHash = adminPasswordHash(app);
      await adminLogin(page, app);
      const csrf = await adminCsrf(page, app);
      await page.request.post(`${app.baseURL}/admin/setup/reopen`, {
        form: { _csrf: csrf },
        maxRedirects: 0,
      });

      const setupHtml = await (await page.request.get(`${app.baseURL}/setup`)).text();
      const setupCsrf = extractCsrf(setupHtml);
      const finish = await page.request.post(`${app.baseURL}/setup/finish`, {
        form: setupFinishForm(setupCsrf, 'new'),
        maxRedirects: 0,
      });
      expect(finish.status()).toBe(303);
      expect(adminPasswordHash(app)).toBe(beforeHash);
      expect(sqliteQuery(app, 'SELECT COUNT(*) FROM admin_users;')).toBe('1');
    });
  });

  test('no-JS setup flow still works', async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'firefox-nojs', 'covered by the firefox-nojs project');
    const app = await createStandaloneApp();
    await withApp(app, async () => {
      await finishFreshSetup(page, app, { board: 'nojs' });
    });
  });

  test('mobile wizard review and finish layout remains usable', async ({ page }) => {
    const app = await createStandaloneApp();
    await withApp(app, async () => {
      await page.setViewportSize({ width: 390, height: 844 });
      await page.goto(`${app.baseURL}/setup`);
      await expect(page.getByRole('region', { name: 'Instance mode' })).toBeVisible();
      await expect(page.getByRole('region', { name: 'Review and finish' })).toBeVisible();
      await finishFreshSetup(page, app, { board: 'mobi' });
    });
  });
});
