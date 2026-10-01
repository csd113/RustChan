import fs from 'node:fs';
import path from 'node:path';
import * as zlib from 'node:zlib';
import {
  adminCsrf,
  adminLogin,
  boardId,
  createBoard,
  expect,
  expectSafePage,
  sqliteQuery,
  test,
  uniqueShort,
  unlockBoard,
  updateBoardSettings,
} from './helpers';

test.describe('phase 2 assets, favicons, banners, and themes', () => {
  test('global and board favicons upload, replace, cache, protect, and clear without stale files', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'asset workflow coverage runs on Chromium first');

    const faviconOne = path.join(app.fixtureDir, 'phase2-favicon-one.png');
    const faviconTwo = path.join(app.fixtureDir, 'phase2-favicon-two.png');
    fs.writeFileSync(faviconOne, pngRgba(512, 512, (index) => [12, 72, 180, 255][index % 4]));
    fs.writeFileSync(faviconTwo, pngRgba(512, 512, (index) => [180, 42, 30, 255][index % 4]));

    await adminLogin(page, app);
    const invalid = await page.request.post(`${app.baseURL}/admin/site/favicon`, {
      multipart: {
        _csrf: await adminCsrf(page, app),
        favicon: {
          name: 'tiny.png',
          mimeType: 'image/png',
          buffer: fs.readFileSync(app.fixtures().tinyPng),
        },
      },
      headers: sameOriginHeaders(app),
      maxRedirects: 0,
    });
    expect(invalid.status()).toBe(303);
    expect(fs.existsSync(path.join(app.dataDir, 'runtime', 'favicon', 'version.txt'))).toBe(false);

    const uploadOne = await uploadFavicon(page, app, '/admin/site/favicon', faviconOne);
    expect(uploadOne.status()).toBe(303);
    const globalDir = path.join(app.dataDir, 'runtime', 'favicon');
    const firstVersion = fs.readFileSync(path.join(globalDir, 'version.txt'), 'utf8').trim();
    expect(firstVersion).toMatch(/[0-9a-f-]{36}/i);
    const versioned = await page.request.get(`${app.baseURL}/favicon-32x32.png?v=${firstVersion}`);
    expect(versioned.status()).toBe(200);
    expect(versioned.headers()['cache-control']).toContain('immutable');
    const unversioned = await page.request.get(`${app.baseURL}/favicon-32x32.png`);
    expect(unversioned.status()).toBe(200);
    expect(unversioned.headers()['cache-control']).not.toContain('immutable');

    fs.writeFileSync(path.join(globalDir, 'stale-sibling.tmp'), 'stale');
    const uploadTwo = await uploadFavicon(page, app, '/admin/site/favicon', faviconTwo);
    expect(uploadTwo.status()).toBe(303);
    const secondVersion = fs.readFileSync(path.join(globalDir, 'version.txt'), 'utf8').trim();
    expect(secondVersion).not.toBe(firstVersion);
    expect(fs.existsSync(path.join(globalDir, 'stale-sibling.tmp'))).toBe(false);

    const board = uniqueShort('fav', testInfo);
    await createBoard(page, app, { short: board, name: 'Favicon Protected' });
    await updateBoardSettings(page, app, board, {
      accessMode: 'view_password',
      accessPassword: 'favicon-pass',
    });
    const boardUpload = await uploadFavicon(page, app, '/admin/board/favicon', faviconOne, {
      board_id: String(boardId(app, board)),
    });
    expect(boardUpload.status()).toBe(303);
    const boardFaviconDir = path.join(app.dataDir, 'boards', board, '_favicon');
    const boardVersion = fs.readFileSync(path.join(boardFaviconDir, 'version.txt'), 'utf8').trim();

    await page.context().clearCookies();
    const protectedFavicon = await page.request.get(`${app.baseURL}/boards/${board}/_favicon/favicon-32x32.png?v=${boardVersion}`);
    expect(protectedFavicon.status()).toBe(403);
    await unlockBoard(page, app, board, 'favicon-pass');
    const accessibleFavicon = await page.request.get(`${app.baseURL}/boards/${board}/_favicon/favicon-32x32.png?v=${boardVersion}`);
    expect(accessibleFavicon.status()).toBe(200);
    expect(accessibleFavicon.headers()['cache-control']).toContain('private');

    await adminLogin(page, app);
    const clear = await page.request.post(`${app.baseURL}/admin/board/favicon/clear`, {
      form: {
        _csrf: await adminCsrf(page, app),
        board_id: String(boardId(app, board)),
      },
      maxRedirects: 0,
    });
    expect(clear.status()).toBe(303);
    expect(fs.existsSync(boardFaviconDir)).toBe(false);
  });

  test('banners upload, reject invalid assets, cache-bust, protect board assets, delete, and restore inherit mode', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'banner workflow coverage runs on Chromium first');

    const bannerPng = path.join(app.fixtureDir, 'phase2-banner.png');
    fs.writeFileSync(bannerPng, pngRgba(468, 60, (index) => [20, 120, 80, 255][index % 4]));
    await adminLogin(page, app);
    const beforeGlobalCount = Number(sqliteQuery(app, "SELECT COUNT(*) FROM banner_assets WHERE scope_type = 'global';"));
    const invalid = await uploadBanner(page, app, '/admin/site/banner', app.fixtures().tinyPng);
    expect(invalid.status()).toBe(303);
    expect(Number(sqliteQuery(app, "SELECT COUNT(*) FROM banner_assets WHERE scope_type = 'global';"))).toBe(beforeGlobalCount);

    const globalUpload = await uploadBanner(page, app, '/admin/site/banner', bannerPng, {
      target_type: 'internal_board',
      target_board_value: 'pub',
    });
    expect(globalUpload.status()).toBe(303);
    const global = bannerRow(app, "scope_type = 'global'");
    const globalPath = path.join(app.dataDir, 'runtime', 'banner', 'global', `${global.storageKey}.webp`);
    expect(fs.existsSync(globalPath)).toBe(true);
    await page.goto(`${app.baseURL}/pub`);
    await expect(page.locator(`img[data-banner-id="${global.id}"]`)).toBeVisible();

    const globalUnversioned = await page.request.get(`${app.baseURL}/banner/assets/${global.id}`);
    expect(globalUnversioned.status()).toBe(200);
    expect(globalUnversioned.headers()['cache-control']).not.toContain('immutable');
    const globalVersioned = await page.request.get(`${app.baseURL}/banner/assets/${global.id}?v=${global.createdAt}`);
    expect(globalVersioned.status()).toBe(200);
    expect(globalVersioned.headers()['cache-control']).toContain('immutable');

    const board = uniqueShort('banr', testInfo);
    await createBoard(page, app, { short: board, name: 'Protected Banner' });
    await updateBoardSettings(page, app, board, {
      accessMode: 'view_password',
      accessPassword: 'banner-pass',
    });
    const boardUpload = await uploadBanner(page, app, '/admin/board/banner', bannerPng, {
      board_id: String(boardId(app, board)),
    });
    expect(boardUpload.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT banner_mode FROM boards WHERE short_name = '${board}';`)).toBe('override');
    const boardBanner = bannerRow(app, `scope_type = 'board' AND board_id = ${boardId(app, board)}`);
    const boardPath = path.join(app.dataDir, 'boards', board, '_banner', `${boardBanner.storageKey}.webp`);
    expect(fs.existsSync(boardPath)).toBe(true);

    await page.context().clearCookies();
    const protectedBanner = await page.request.get(`${app.baseURL}/banner/assets/${boardBanner.id}?v=${boardBanner.createdAt}`);
    expect(protectedBanner.status()).toBe(403);
    await unlockBoard(page, app, board, 'banner-pass');
    const accessibleBanner = await page.request.get(`${app.baseURL}/banner/assets/${boardBanner.id}?v=${boardBanner.createdAt}`);
    expect(accessibleBanner.status()).toBe(200);
    expect(accessibleBanner.headers()['cache-control']).toContain('private');

    await adminLogin(page, app);
    const clearBoardBanner = await page.request.post(`${app.baseURL}/admin/board/banner/clear`, {
      form: {
        _csrf: await adminCsrf(page, app),
        board_id: String(boardId(app, board)),
      },
      maxRedirects: 0,
    });
    expect(clearBoardBanner.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT banner_mode FROM boards WHERE short_name = '${board}';`)).toBe('inherit');
    expect(fs.existsSync(boardPath)).toBe(false);

    const deleteGlobal = await page.request.post(`${app.baseURL}/admin/banner/delete`, {
      form: {
        _csrf: await adminCsrf(page, app),
        banner_id: String(global.id),
      },
      maxRedirects: 0,
    });
    expect(deleteGlobal.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM banner_assets WHERE id = ${global.id};`)).toBe('0');
    expect(fs.existsSync(globalPath)).toBe(false);
  });

  test('custom themes create, edit, delete, and clear default references safely', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'theme management coverage runs on Chromium first');

    await adminLogin(page, app);
    const slug = uniqueShort('thm', testInfo);
    const create = await page.request.post(`${app.baseURL}/admin/theme/create`, {
      form: {
        _csrf: await adminCsrf(page, app),
        slug,
        display_name: 'Phase 2 Theme',
        description: 'created in e2e',
        swatch_hex: '#336699',
        theme_mode: 'legacy',
        custom_css: `html[data-theme="${slug}"] body { --phase2-theme-test: 1; }`,
        enabled: '1',
      },
      maxRedirects: 0,
    });
    expect(create.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT display_name FROM themes WHERE slug = '${slug}';`)).toBe('Phase 2 Theme');
    const css = await page.request.get(`${app.baseURL}/theme-css/${slug}`);
    expect(css.status()).toBe(200);
    expect(await css.text()).toContain('--phase2-theme-test');

    const update = await page.request.post(`${app.baseURL}/admin/theme/update`, {
      form: {
        _csrf: await adminCsrf(page, app),
        existing_slug: slug,
        slug,
        display_name: 'Phase 2 Theme Edited',
        description: 'edited in e2e',
        swatch_hex: '#663399',
        theme_mode: 'legacy',
        custom_css: `html[data-theme="${slug}"] body { --phase2-theme-edited: 1; }`,
        enabled: '1',
      },
      maxRedirects: 0,
    });
    expect(update.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT display_name FROM themes WHERE slug = '${slug}';`)).toBe('Phase 2 Theme Edited');
    await page.request.post(`${app.baseURL}/admin/site/settings`, {
      form: {
        _csrf: await adminCsrf(page, app),
        site_name: 'RustChan',
        site_subtitle: 'theme fallback',
        default_theme: slug,
        homepage_new_thread_badges_enabled: '1',
        homepage_new_reply_badges_enabled: '1',
        thread_new_reply_badges_enabled: '1',
      },
      maxRedirects: 0,
    });
    expect(sqliteQuery(app, "SELECT value FROM site_settings WHERE key = 'default_theme';")).toBe(slug);

    const deleteTheme = await page.request.post(`${app.baseURL}/admin/theme/delete`, {
      form: {
        _csrf: await adminCsrf(page, app),
        slug,
      },
      maxRedirects: 0,
    });
    expect(deleteTheme.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM themes WHERE slug = '${slug}';`)).toBe('0');
    expect(sqliteQuery(app, "SELECT value FROM site_settings WHERE key = 'default_theme';")).toBe('forest');
    await page.goto(app.baseURL);
    await expectSafePage(page);
  });
});

async function uploadFavicon(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
  route: string,
  filePath: string,
  fields: Record<string, string> = {},
) {
  return page.request.post(`${app.baseURL}${route}`, {
    multipart: {
      _csrf: await adminCsrf(page, app),
      ...fields,
      favicon: {
        name: path.basename(filePath),
        mimeType: 'image/png',
        buffer: fs.readFileSync(filePath),
      },
    },
    headers: sameOriginHeaders(app),
    maxRedirects: 0,
  });
}

async function uploadBanner(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
  route: string,
  filePath: string,
  fields: Record<string, string> = {},
) {
  return page.request.post(`${app.baseURL}${route}`, {
    multipart: {
      _csrf: await adminCsrf(page, app),
      target_type: 'none',
      show_on_index: '1',
      show_on_catalog: '1',
      enabled: '1',
      ...fields,
      banner: {
        name: path.basename(filePath),
        mimeType: 'image/png',
        buffer: fs.readFileSync(filePath),
      },
    },
    headers: sameOriginHeaders(app),
    maxRedirects: 0,
  });
}

function sameOriginHeaders(app: Parameters<typeof adminCsrf>[1]): Record<string, string> {
  return {
    Origin: app.baseURL,
    Referer: `${app.baseURL}/admin/panel`,
  };
}

function bannerRow(app: Parameters<typeof sqliteQuery>[0], whereClause: string): { id: number; storageKey: string; createdAt: string } {
  const row = sqliteQuery(
    app,
    `SELECT id || '|' || storage_key || '|' || created_at FROM banner_assets WHERE ${whereClause} ORDER BY id DESC LIMIT 1;`,
  );
  const [id, storageKey, createdAt] = row.split('|');
  return { id: Number(id), storageKey, createdAt };
}

function pngRgba(width: number, height: number, pixelByte: (index: number) => number): Buffer {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  let sourceIndex = 0;
  for (let y = 0; y < height; y += 1) {
    const rowStart = y * (width * 4 + 1);
    raw[rowStart] = 0;
    for (let x = 0; x < width * 4; x += 1) {
      raw[rowStart + 1 + x] = pixelByte(sourceIndex);
      sourceIndex += 1;
    }
  }
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  return Buffer.concat([
    signature,
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', zlib.deflateSync(raw)),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

function pngChunk(type: string, data: Buffer): Buffer {
  const typeBytes = Buffer.from(type, 'ascii');
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  typeBytes.copy(out, 4);
  data.copy(out, 8);
  out.writeUInt32BE(crc32(Buffer.concat([typeBytes, data])), 8 + data.length);
  return out;
}

function crc32(data: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of data) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}
