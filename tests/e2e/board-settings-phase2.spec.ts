import fs from 'node:fs';
import path from 'node:path';
import {
  adminCsrf,
  adminLogin,
  boardId,
  createBoard,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  publicCsrf,
  sqliteQuery,
  test,
  uniqueShort,
  unlockBoard,
  updateBoardSettings,
} from './helpers';

test.describe('phase 2 board settings depth', () => {
  test('browser admin flow creates boards and persists deep settings across access modes', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'browser-generic board settings coverage runs on Chromium first');

    await adminLogin(page, app);
    const publicBoard = uniqueShort('bpub', testInfo);
    await page.goto(`${app.baseURL}/admin/panel?open=boards#boards`);
    const createForm = page.locator('form[action="/admin/board/create"]');
    await createForm.locator('input[name="short_name"]').fill(publicBoard);
    await createForm.locator('input[name="name"]').fill('Phase 2 Public');
    await createForm.locator('input[name="description"]').fill('created from the browser admin form');
    await Promise.all([
      page.waitForURL(/\/admin\/panel/),
      createForm.getByRole('button', { name: /^create$/i }).click(),
    ]);
    await page.goto(`${app.baseURL}/admin/panel?open=board-${publicBoard}#board-${publicBoard}`);
    await expect(page.locator(`#board-${publicBoard}`)).toContainText(`/${publicBoard}/`);

    const boardCard = page.locator(`#board-${publicBoard}`);
    await boardCard.evaluate((node) => {
      if (node instanceof HTMLDetailsElement) node.open = true;
    });
    const settingsForm = boardCard.locator('form.board-settings-form');
    await settingsForm.locator('input[name="name"]').fill('Phase 2 Public Edited');
    await settingsForm.locator('input[name="description"]').fill(`long description ${'D'.repeat(280)}`);
    await settingsForm.locator('input[name="bump_limit"]').fill('2');
    await settingsForm.locator('input[name="max_threads"]').fill('4');
    await settingsForm.locator('input[name="max_archived_threads"]').fill('6');
    await settingsForm.locator('input[name="post_cooldown_secs"]').fill('1');
    await settingsForm.locator('select[name="access_mode"]').selectOption('public');
    await settingsForm.locator('input[name="nsfw"]').check();
    await settingsForm.locator('input[name="allow_archive"]').check();
    await settingsForm.locator('input[name="allow_images"]').uncheck();
    await settingsForm.locator('input[name="allow_video"]').uncheck();
    await settingsForm.locator('input[name="allow_audio"]').check();
    await settingsForm.locator('input[name="allow_pdf"]').check();
    await settingsForm.locator('input[name="allow_any_files"]').check();
    await settingsForm.locator('input[name="allow_tripcodes"]').uncheck();
    await settingsForm.locator('input[name="allow_editing"]').check();
    await settingsForm.locator('input[name="allow_self_delete"]').check();
    await settingsForm.locator('input[name="allow_video_embeds"]').uncheck();
    await settingsForm.locator('input[name="show_poster_ids"]').check();
    await settingsForm.locator('input[name="collapse_greentext"]').check();
    await Promise.all([
      page.waitForURL(/\/admin\/panel/),
      settingsForm.getByRole('button', { name: /^save settings$/i }).click(),
    ]);

    await postBoardSettings(page, app, publicBoard, {
      name: 'Phase 2 Public Edited',
      description: `long description ${'D'.repeat(280)}`,
      nsfw: true,
      allowImages: false,
      allowVideo: false,
      allowAudio: true,
      allowPdf: true,
      allowAnyFiles: true,
      allowArchive: true,
      allowTripcodes: false,
      allowEditing: true,
      allowSelfDelete: true,
      allowVideoEmbeds: false,
      showPosterIds: true,
      collapseGreentext: true,
      bumpLimit: 2,
      maxThreads: 4,
      maxArchivedThreads: 6,
      postCooldownSecs: 1,
      defaultTheme: 'terminal',
      bannerMode: 'none',
      accessMode: 'public',
    });
    expect(boardSettingsRow(app, publicBoard)).toEqual([
      'Phase 2 Public Edited',
      `long description ${'D'.repeat(280)}`.slice(0, 256),
      '1',
      '0',
      '0',
      '1',
      '1',
      '1',
      '0',
      '1',
      '1',
      '0',
      '1',
      '1',
      '2',
      '4',
      '6',
      '1',
      'terminal',
      'none',
      'public',
    ]);

    await page.goto(`${app.baseURL}/${publicBoard}`);
    await expectSafePage(page);
    const uploadDeniedCsrf = await postFormCsrf(page, publicBoard);
    const uploadDenied = await page.request.post(`${app.baseURL}/${publicBoard}`, {
      multipart: {
        _csrf: uploadDeniedCsrf,
        submission_token: `image-denied-${Date.now()}`,
        subject: 'image denied',
        body: 'this should not accept images',
        file: {
          name: 'tiny.png',
          mimeType: 'image/png',
          buffer: fs.readFileSync(app.fixtures().tinyPng),
        },
      },
      headers: publicSameOriginHeaders(app, `/${publicBoard}`),
      maxRedirects: 0,
    });
    expect([400, 415, 422]).toContain(uploadDenied.status());
    await expectSafeResponse(uploadDenied);

    const viewBoard = uniqueShort('bview', testInfo);
    const postBoard = uniqueShort('bpost', testInfo);
    await createBoard(page, app, { short: viewBoard, name: 'View Protected' });
    await createBoard(page, app, { short: postBoard, name: 'Post Protected' });
    await updateBoardSettings(page, app, viewBoard, {
      accessMode: 'view_password',
      accessPassword: 'view-pass',
      allowArchive: true,
    });
    await updateBoardSettings(page, app, postBoard, {
      accessMode: 'post_password',
      accessPassword: 'post-pass',
      allowArchive: true,
    });

    await page.context().clearCookies();
    await page.goto(`${app.baseURL}/${viewBoard}`);
    await expect(page.locator('body')).toContainText(/password protected/i);
    await unlockBoard(page, app, viewBoard, 'view-pass');
    await expectSafePage(page);

    await page.context().clearCookies();
    await page.goto(`${app.baseURL}/${postBoard}`);
    await expectSafePage(page);
    await expect(page.locator('#board-access-gate')).toContainText(/posting is password protected|unlock posting/i);
    const deniedPostCsrf = await postFormCsrf(page, postBoard);
    const deniedPost = await page.request.post(`${app.baseURL}/${postBoard}`, {
      multipart: {
        _csrf: deniedPostCsrf,
        submission_token: `post-password-denied-${Date.now()}`,
        subject: 'denied',
        body: 'missing post password',
      },
      headers: publicSameOriginHeaders(app, `/${postBoard}`),
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(deniedPost.status());
    expect(deniedPost.headers().location).toContain(`/${postBoard}/unlock`);
    await unlockBoard(page, app, postBoard, 'post-pass');
    const postPasswordThread = await createThread(page, app, postBoard, {
      subject: 'post password accepted',
      body: 'posting works after unlock',
    });
    await expect(page).toHaveURL(new RegExp(`/${postBoard}/thread/${postPasswordThread}`));
  });

  test('invalid board inputs, upload caps, delete cleanup, and safe redirects fail closed', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'board validation and cleanup coverage runs on Chromium first');

    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);
    const duplicate = uniqueShort('bdup', testInfo);
    await createBoard(page, app, { short: duplicate, name: 'Duplicate Source' });

    for (const shortName of ['', '../etc', 'bad/name', 'waytoolong', duplicate]) {
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: {
          _csrf: csrf,
          short_name: shortName,
          name: 'Invalid Board',
          description: 'must fail safely',
        },
        maxRedirects: 0,
      });
      expect([400, 409, 422]).toContain(response.status());
      await expectSafeResponse(response);
    }

    const capBoard = uniqueShort('bcap', testInfo);
    await createBoard(page, app, { short: capBoard, name: 'Cap Board' });
    const beforeCaps = sqliteQuery(
      app,
      `SELECT max_image_size || '|' || max_video_size || '|' || max_audio_size || '|' || max_pdf_size FROM boards WHERE short_name = '${capBoard}';`,
    );
    const badCaps = await postRawBoardSettings(page, app, capBoard, {
      max_image_size_mb: '0',
      max_video_size_mb: '50',
      max_audio_size_mb: '150',
      max_pdf_size_mb: '8',
    });
    expect(badCaps.status()).toBe(400);
    await expectSafeResponse(badCaps);
    expect(sqliteQuery(
      app,
      `SELECT max_image_size || '|' || max_video_size || '|' || max_audio_size || '|' || max_pdf_size FROM boards WHERE short_name = '${capBoard}';`,
    )).toBe(beforeCaps);

    const redirectBoard = uniqueShort('bred', testInfo);
    await createBoard(page, app, { short: redirectBoard, name: 'Redirect Board' });
    const safeRedirect = await page.request.post(`${app.baseURL}/admin/board/reorder`, {
      form: {
        _csrf: await adminCsrf(page, app),
        board_id: String(boardId(app, redirectBoard)),
        direction: 'up',
        return_to: '//evil.example/phish',
      },
      maxRedirects: 0,
    });
    expect(safeRedirect.status()).toBe(303);
    expect(safeRedirect.headers().location).toBe('/admin/panel');

    const cleanupBoard = uniqueShort('bdel', testInfo);
    await createBoard(page, app, { short: cleanupBoard, name: 'Delete Cleanup' });
    await createThread(page, app, cleanupBoard, {
      subject: 'delete cleanup',
      body: 'board upload directory should disappear',
      filePath: app.fixtures().tinyPng,
    });
    const boardDir = path.join(app.dataDir, 'boards', cleanupBoard);
    expect(fs.existsSync(boardDir)).toBe(true);
    const deleteResponse = await page.request.post(`${app.baseURL}/admin/board/delete`, {
      form: {
        _csrf: await adminCsrf(page, app),
        board_id: String(boardId(app, cleanupBoard)),
      },
      maxRedirects: 0,
    });
    expect(deleteResponse.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM boards WHERE short_name = '${cleanupBoard}';`)).toBe('0');
    expect(fs.existsSync(boardDir)).toBe(false);
    await page.goto(`${app.baseURL}/${cleanupBoard}`);
    await expect(page.locator('body')).not.toContainText('delete cleanup');
  });
});

type BoardSettingsPost = {
  name?: string;
  description?: string;
  nsfw?: boolean;
  allowImages?: boolean;
  allowVideo?: boolean;
  allowAudio?: boolean;
  allowPdf?: boolean;
  allowAnyFiles?: boolean;
  allowArchive?: boolean;
  allowTripcodes?: boolean;
  allowEditing?: boolean;
  allowSelfDelete?: boolean;
  allowVideoEmbeds?: boolean;
  showPosterIds?: boolean;
  collapseGreentext?: boolean;
  bumpLimit?: number;
  maxThreads?: number;
  maxArchivedThreads?: number;
  postCooldownSecs?: number;
  defaultTheme?: string;
  bannerMode?: 'inherit' | 'none' | 'override';
  accessMode?: 'public' | 'view_password' | 'post_password';
  accessPassword?: string;
};

async function postBoardSettings(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
  short: string,
  settings: BoardSettingsPost,
): Promise<void> {
  const response = await postRawBoardSettings(page, app, short, {
    name: settings.name ?? `${short} board`,
    description: settings.description ?? '',
    bump_limit: String(settings.bumpLimit ?? 300),
    max_threads: String(settings.maxThreads ?? 150),
    max_archived_threads: String(settings.maxArchivedThreads ?? 150),
    post_cooldown_secs: String(settings.postCooldownSecs ?? 0),
    max_image_size_mb: '8',
    max_video_size_mb: '50',
    max_audio_size_mb: '150',
    max_pdf_size_mb: '8',
    default_theme: settings.defaultTheme ?? '',
    banner_mode: settings.bannerMode ?? 'inherit',
    access_mode: settings.accessMode ?? 'public',
    access_password: settings.accessPassword ?? '',
    nsfw: settings.nsfw,
    allow_images: settings.allowImages,
    allow_video: settings.allowVideo,
    allow_audio: settings.allowAudio,
    allow_pdf: settings.allowPdf,
    allow_any_files: settings.allowAnyFiles,
    allow_archive: settings.allowArchive,
    allow_tripcodes: settings.allowTripcodes,
    allow_editing: settings.allowEditing,
    allow_self_delete: settings.allowSelfDelete,
    allow_video_embeds: settings.allowVideoEmbeds,
    show_poster_ids: settings.showPosterIds,
    collapse_greentext: settings.collapseGreentext,
  });
  expect(response.status()).toBe(303);
}

async function postRawBoardSettings(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
  short: string,
  fields: Record<string, string | boolean | undefined>,
) {
  const form: Record<string, string> = {
    _csrf: await adminCsrf(page, app),
    board_id: String(boardId(app, short)),
    name: typeof fields.name === 'string' ? fields.name : `${short} board`,
    description: typeof fields.description === 'string' ? fields.description : '',
    bump_limit: typeof fields.bump_limit === 'string' ? fields.bump_limit : '300',
    max_threads: typeof fields.max_threads === 'string' ? fields.max_threads : '150',
    max_archived_threads: typeof fields.max_archived_threads === 'string' ? fields.max_archived_threads : '150',
    post_cooldown_secs: typeof fields.post_cooldown_secs === 'string' ? fields.post_cooldown_secs : '0',
    max_image_size_mb: typeof fields.max_image_size_mb === 'string' ? fields.max_image_size_mb : '8',
    max_video_size_mb: typeof fields.max_video_size_mb === 'string' ? fields.max_video_size_mb : '50',
    max_audio_size_mb: typeof fields.max_audio_size_mb === 'string' ? fields.max_audio_size_mb : '150',
    max_pdf_size_mb: typeof fields.max_pdf_size_mb === 'string' ? fields.max_pdf_size_mb : '8',
    default_theme: typeof fields.default_theme === 'string' ? fields.default_theme : '',
    banner_mode: typeof fields.banner_mode === 'string' ? fields.banner_mode : 'inherit',
    access_mode: typeof fields.access_mode === 'string' ? fields.access_mode : 'public',
    access_password: typeof fields.access_password === 'string' ? fields.access_password : '',
  };
  for (const name of [
    'nsfw',
    'allow_images',
    'allow_video',
    'allow_audio',
    'allow_pdf',
    'allow_any_files',
    'allow_archive',
    'allow_tripcodes',
    'allow_editing',
    'allow_self_delete',
    'allow_video_embeds',
    'show_poster_ids',
    'collapse_greentext',
  ]) {
    if (fields[name] === true) form[name] = '1';
  }
  return page.request.post(`${app.baseURL}/admin/board/settings`, { form, maxRedirects: 0 });
}

function boardSettingsRow(app: Parameters<typeof sqliteQuery>[0], short: string): string[] {
  return sqliteQuery(
    app,
    [
      'SELECT name, description, nsfw, allow_images, allow_video, allow_audio, allow_pdf, allow_any_files,',
      'allow_tripcodes, allow_editing, allow_self_delete, allow_video_embeds, show_poster_ids, collapse_greentext,',
      'bump_limit, max_threads, max_archived_threads, post_cooldown_secs, default_theme, banner_mode, access_mode',
      `FROM boards WHERE short_name = '${short}'`,
    ].join(' '),
  ).split('|');
}

function publicSameOriginHeaders(app: Parameters<typeof adminCsrf>[1], pathPart: string): Record<string, string> {
  return {
    Origin: app.baseURL,
    Referer: `${app.baseURL}${pathPart}`,
  };
}

async function postFormCsrf(page: Parameters<typeof publicCsrf>[0], board: string): Promise<string> {
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"], [data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  const postInput = page.locator(`form[action="/${board}"] input[name="_csrf"]`).first();
  if (await postInput.count() > 0) {
    const value = await postInput.inputValue();
    if (value.length > 0) return value;
  }
  const values = await page.locator('input[name="_csrf"]').evaluateAll((inputs) =>
    inputs
      .map((input) => input instanceof HTMLInputElement ? input.value : '')
      .filter((value) => value.length > 0),
  );
  expect(values.length, `csrf token for /${board}/`).toBeGreaterThan(0);
  return values[0];
}
