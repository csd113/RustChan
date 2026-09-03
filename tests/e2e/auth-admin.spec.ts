import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  adminCsrf,
  adminLogin,
  adminLogout,
  createBoard,
  expect,
  expectNoDialog,
  expectSafePage,
  expectSafeResponse,
  RustChanServer,
  test,
  uniqueShort,
  updateBoardSettings,
} from './helpers';

test.describe('first run and admin authentication', () => {
  test('fresh runtime creates exe-local structure and reports missing admin without leaking secrets', async ({ page }) => {
    const app = await RustChanServer.create();
    try {
      await app.start({ cwd: app.rootDir });
      await page.goto(app.baseURL);
      await expectSafePage(page);
      await expect(page.locator('body')).toContainText('RustChan');

      await page.goto(`${app.baseURL}/admin`);
      await expectSafePage(page, { allowAdminInternals: true });
      await expect(page.getByRole('heading', { name: /admin login/i })).toBeVisible();

      expect(fs.existsSync(path.join(app.dataDir, 'settings.toml'))).toBe(true);
      expect(fs.existsSync(path.join(app.dataDir, 'chan.db'))).toBe(true);
      expect(fs.existsSync(path.join(app.dataDir, 'boards'))).toBe(true);
      expect(fs.existsSync(path.join(app.dataDir, 'logs'))).toBe(true);
      for (const rel of [
        'runtime',
        'runtime/tmp',
        'runtime/tmp/board-downloads',
        'runtime/favicon',
        'runtime/banner',
        'backups',
        'backups/full',
        'backups/boards',
      ]) {
        expect(fs.existsSync(path.join(app.dataDir, rel)), `${rel} should be exe-local`).toBe(true);
      }
      expect(fs.existsSync(path.join(app.rootDir, 'rustchan-data', 'settings.toml'))).toBe(false);

      const logs = await app.logs();
      expect(logs).toMatch(/Created settings\.toml|settings\.toml/);
      expect(logs).toMatch(/HTTP server listening|Admin panel|No admin accounts exist/);
      expect(logs).not.toContain('cookie_secret');
    } finally {
      await app.dispose();
    }
  });

  test('invalid settings fail closed without leaking configured secrets', async () => {
    const cases = [
      {
        name: 'malformed-type',
        body: [
          `cookie_secret = "${'a'.repeat(64)}"`,
          'port = "SECRET_SENTINEL_DO_NOT_LEAK"',
          '',
        ].join('\n'),
      },
      {
        name: 'short-secret',
        body: [
          'cookie_secret = "SECRET_SENTINEL_DO_NOT_LEAK"',
          '',
        ].join('\n'),
      },
    ];

    for (const invalid of cases) {
      const app = await RustChanServer.create();
      try {
        await fsp.mkdir(app.dataDir, { recursive: true });
        await fsp.writeFile(path.join(app.dataDir, 'settings.toml'), invalid.body);
        await expect(app.start(), invalid.name).rejects.toThrow(
          /rustchan exited before ready[\s\S]*Recent server output:[\s\S]*(CONFIG ERROR|settings\.toml)/i,
        );
        const logs = await app.logs();
        expect(logs, invalid.name).toMatch(/CONFIG ERROR|settings\.toml/i);
        expect(logs, invalid.name).not.toContain('SECRET_SENTINEL_DO_NOT_LEAK');
        expect(logs, invalid.name).not.toMatch(/cookie_secret\s*=/i);
      } finally {
        await app.dispose();
      }
    }
  });

  test('admin login, logout, stale back page, cookie attributes, and context isolation', async ({ page, browser, app }) => {
    await page.goto(`${app.baseURL}/admin`);
    await page.getByLabel('Username').fill(ADMIN_USERNAME);
    await page.getByLabel('Password').fill('wrong-password');
    await page.getByRole('button', { name: 'authenticate' }).click();
    await expect(page.getByRole('alert')).toContainText(/invalid username or password/i);
    await expectSafePage(page, { allowAdminInternals: true });

    await adminLogin(page, app);
    const cookies = await page.context().cookies(app.baseURL);
    const session = cookies.find((cookie) => cookie.name === 'chan_admin_session');
    expect(session).toBeTruthy();
    expect(session?.httpOnly).toBe(true);
    expect(session?.sameSite).toBe('Lax');
    expect(session?.secure).toBe(false);

    const secondContext = await browser.newContext();
    const secondPage = await secondContext.newPage();
    await secondPage.goto(`${app.baseURL}/admin/panel`);
    await expect(secondPage.locator('body')).not.toContainText('[ admin panel ]');
    await expect(secondPage.locator('body')).toContainText(/not logged in|forbidden|admin login/i);
    await secondContext.close();

    const staleCsrf = await adminCsrf(page, app);
    await adminLogout(page);
    await page.goBack();
    await expect(page.locator('body')).toBeVisible();
    const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
      form: {
        _csrf: staleCsrf,
        short_name: uniqueShort('stale', test.info()),
        name: 'stale action',
        description: '',
      },
      maxRedirects: 0,
    });
    expect(response.status()).toBe(403);
    await expectSafeResponse(response);
  });
});

test.describe('admin board management', () => {
  test('creates, edits, protects against injection, persists, and deletes boards', async ({ page, app }, testInfo) => {
    const short = uniqueShort('adm', testInfo);
    await createBoard(page, app, {
      short,
      name: 'Admin Managed',
      description: 'created through admin request',
      audio: true,
    });
    await updateBoardSettings(page, app, short, {
      name: '<script>alert(1)</script> Board',
      description: 'quotes " \' and <img src=x onerror=alert(1)>',
      nsfw: true,
      allowImages: true,
      allowVideo: true,
      allowAudio: true,
      allowPdf: true,
      allowArchive: true,
      maxImageSizeMb: 2,
      maxVideoSizeMb: 3,
      maxAudioSizeMb: 4,
    });

    await expectNoDialog(page, async () => {
      await page.goto(`${app.baseURL}/?nsfw=${short}`);
      await expectSafePage(page);
    });
    expect(await page.locator('script:text("alert(1)")').count()).toBe(0);
    await app.restart();
    await page.goto(`${app.baseURL}/admin/panel`);
    await adminLogin(page, app);
    await expect(page.locator(`#board-${short}`)).toContainText('/' + short + '/');

    const csrf = await adminCsrf(page, app);
    const panel = await page.request.get(`${app.baseURL}/admin/panel`).then((response) => response.text());
    const boardId = Number(panel.match(new RegExp(`id="board-${short}"[\\s\\S]*?name="board_id" value="(\\d+)"`))?.[1]);
    const deleteResponse = await page.request.post(`${app.baseURL}/admin/board/delete`, {
      form: { _csrf: csrf, board_id: String(boardId) },
      maxRedirects: 0,
    });
    expect(deleteResponse.status()).toBe(303);
    await page.goto(app.baseURL);
    await expect(page.locator(`a[href="/${short}"], a[href="/${short}/catalog"]`)).toHaveCount(0);
  });

  test('rejects invalid, traversal-like, and duplicate board short names safely', async ({ page, app }, testInfo) => {
    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);
    const validShort = uniqueShort('dup', testInfo);
    await createBoard(page, app, { short: validShort, name: 'Duplicate Source' });

    for (const shortName of ['', '../etc', 'bad/name', 'toolongname', validShort]) {
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: {
          _csrf: csrf,
          short_name: shortName,
          name: 'Invalid Board',
          description: 'must fail',
        },
        maxRedirects: 0,
      });
      expect([400, 409, 422]).toContain(response.status());
      await expectSafeResponse(response);
    }
  });
});
