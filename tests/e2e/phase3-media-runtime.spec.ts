import fsp from 'node:fs/promises';
import path from 'node:path';
import type { Page } from '@playwright/test';
import {
  adminPasswordHash,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  setBoardFixtureSettings,
  sqliteExec,
  sqliteQuery,
  test,
  uniqueShort,
  updateBoardSettings,
  type RustChanServer,
} from './helpers';

const SCRIPTED_MEDIA_PROJECTS = new Set(['chromium', 'webkit', 'mobile-webkit']);
const MEDIA_CONTROL_PROJECTS = new Set(['chromium']);
const DOWNLOAD_HEADER_PROJECTS = new Set(['chromium', 'firefox']);
const NO_JS_PROJECTS = new Set(['firefox-nojs']);

test.describe('phase 3 media viewer runtime behavior', () => {
  test('image viewer expands, collapses, falls back for missing thumbs, and keeps long filename links usable', async ({ page, app }, testInfo) => {
    test.skip(
      !SCRIPTED_MEDIA_PROJECTS.has(testInfo.project.name),
      'scripted media viewer coverage is sampled on Chromium, WebKit, and mobile WebKit',
    );

    const longImage = path.join(
      app.fixtureDir,
      `phase3-${'long-filename-'.repeat(7)}image.png`,
    );
    await fsp.copyFile(app.fixtures().tinyPng, longImage);

    const threadId = await createThread(page, app, 'img', {
      subject: 'phase 3 long image filename',
      body: `long image filename ${'bodyword'.repeat(24)}`,
      filePath: longImage,
    });
    const postId = opPostId(app, threadId);
    const post = page.locator(`#p${postId}`);
    const fileLink = post.locator('.file-info a').first();

    const title = await fileLink.getAttribute('title');
    expect(title).toContain('phase3-long-filename');
    expect(title).not.toMatch(/[\\/"']/);
    await expect(fileLink).toContainText('(...)');
    await expect(post.locator('.image-preview')).toBeVisible();
    await expect(post.locator('.media-expanded-image')).not.toBeVisible();

    await post.locator('.image-preview').click();
    await expect(post.locator('.media-expanded-image')).toBeVisible();
    await expect(post.locator('.media-expanded-image')).toHaveAttribute('src', /\/boards\/img\//);
    await expect(post.locator('.media-close-btn')).toBeVisible();

    await post.locator('.media-expanded-image').click();
    await expect(post.locator('.media-expanded-image')).not.toBeVisible();
    await expect(post.locator('.image-preview')).toBeVisible();

    sqliteExec(
      app,
      `UPDATE posts SET thumb_path = 'img/thumbs/phase3-missing-${postId}.webp' WHERE id = ${postId};`,
    );
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.locator(`#p${postId} .media-thumb-fallback`)).toBeVisible();

    const href = await page.locator(`#p${postId} .file-info a`).first().getAttribute('href');
    expect(href).toBeTruthy();
    await fsp.rm(path.join(app.dataDir, href!.replace(/^\/boards\//, 'boards/')), { force: true });
    const missingOriginal = await page.request.get(`${app.baseURL}${href}`);
    expect(missingOriginal.status()).toBe(404);
    await expectSafeResponse(missingOriginal);
  });

  test('video, audio, PDF, and pending media refresh render controls without pixel assertions', async ({ page, app }, testInfo) => {
    test.skip(
      !MEDIA_CONTROL_PROJECTS.has(testInfo.project.name),
      'DOM media-control and refresh assertions run once in Chromium',
    );

    const videoThread = await createThread(page, app, 'vid', {
      subject: 'phase 3 pending video',
      body: 'video starts with pending preview state',
      filePath: app.fixtures().fakeMp4,
    });
    const videoPostId = opPostId(app, videoThread);
    sqliteExec(
      app,
      [
        'UPDATE posts',
        "SET thumb_path = NULL, media_type = 'video', mime_type = 'video/mp4',",
        "    media_processing_state = 'pending', media_processing_error = NULL",
        `WHERE id = ${videoPostId};`,
      ].join(' '),
    );
    await page.goto(`${app.baseURL}/vid/thread/${videoThread}`);
    await expect(page.locator(`#p${videoPostId}`)).toHaveAttribute('data-media-processing-state', 'pending');
    await expect(page.locator(`#p${videoPostId}`)).toContainText(/processing media|Preview still processing/i);

    const videoThumb = await writeBoardFile(app, 'vid', `thumbs/phase3-video-${videoPostId}.png`, await fsp.readFile(app.fixtures().tinyPng));
    sqliteExec(
      app,
      [
        'UPDATE posts',
        `SET thumb_path = ${sqlString(videoThumb)}, media_processing_state = '', media_processing_error = NULL`,
        `WHERE id = ${videoPostId};`,
      ].join(' '),
    );
    await page.locator('[data-action="fetch-updates"]').first().click();
    await expect(page.locator(`#p${videoPostId} .video-container video[controls]`)).toHaveCount(1);
    await expect(page.locator(`#p${videoPostId}`)).not.toHaveAttribute('data-media-processing-state', 'pending');

    await page.locator(`#p${videoPostId} .video-preview`).click();
    await expect(page.locator(`#p${videoPostId} video.media-expanded-video`)).toBeVisible();

    const audioThread = await createThread(page, app, 'aud', {
      subject: 'phase 3 audio controls',
      body: 'audio controls should render even with deterministic fixture media',
      filePath: app.fixtures().fakeOgg,
    });
    const audioPostId = opPostId(app, audioThread);
    const audioThumb = await writeBoardFile(app, 'aud', `thumbs/phase3-audio-${audioPostId}.png`, await fsp.readFile(app.fixtures().tinyPng));
    sqliteExec(
      app,
      `UPDATE posts SET thumb_path = ${sqlString(audioThumb)}, media_type = 'audio', mime_type = 'audio/ogg' WHERE id = ${audioPostId};`,
    );
    await page.goto(`${app.baseURL}/aud/thread/${audioThread}`);
    await expect(page.locator(`#p${audioPostId} audio.audio-player[controls]`)).toHaveCount(1);
    await expect(page.locator(`#p${audioPostId} source[type="audio/ogg"]`)).toHaveCount(1);

    const pdfBoard = uniqueShort('p3pdf', testInfo);
    app.createBoardCli({ short: pdfBoard, name: 'Phase 3 PDF' });
    await updateBoardSettings(page, app, pdfBoard, { allowPdf: true });
    const pdfThread = await createThread(page, app, pdfBoard, {
      subject: 'phase 3 pdf viewer',
      body: 'pdf inline viewer should be same-origin framed',
      filePath: app.fixtures().tinyPdf,
    });
    const pdfPostId = opPostId(app, pdfThread);
    const pdfThumb = await writeBoardFile(app, pdfBoard, `thumbs/phase3-pdf-${pdfPostId}.svg`, Buffer.from(pdfThumbSvg()));
    sqliteExec(
      app,
      `UPDATE posts SET thumb_path = ${sqlString(pdfThumb)}, media_type = 'pdf', mime_type = 'application/pdf' WHERE id = ${pdfPostId};`,
    );
    await page.goto(`${app.baseURL}/${pdfBoard}/thread/${pdfThread}`);
    await expect(page.locator(`#p${pdfPostId} .pdf-container iframe.media-expanded-pdf`)).toHaveAttribute('data-src', /\/boards\//);
    await page.locator(`#p${pdfPostId} .pdf-preview`).click();
    await expect(page.locator(`#p${pdfPostId} iframe.media-expanded-pdf`)).toBeVisible();
    await expect(page.locator(`#p${pdfPostId} iframe.media-expanded-pdf`)).toHaveAttribute('src', /\/boards\//);
  });

  test('Firefox no-JS users keep direct media links without the JS viewer', async ({ page, app }, testInfo) => {
    test.skip(!NO_JS_PROJECTS.has(testInfo.project.name), 'no-JS fallback is Firefox-specific signal');

    const threadId = await createThread(page, app, 'img', {
      subject: 'phase 3 no-js image',
      body: 'direct media links stay usable with JavaScript disabled',
      filePath: app.fixtures().tinyPng,
    });
    await page.goto(`${app.baseURL}/img/thread/${threadId}`);
    await expectSafePage(page);

    const previewHref = await page.locator('.image-preview').first().getAttribute('href');
    const originalHref = await page.locator('.file-info a').first().getAttribute('href');
    expect(previewHref).toBe(originalHref);
    expect(originalHref).toMatch(/^\/boards\/img\//);
    await expect(page.locator('.file-info a').first()).toHaveAttribute('target', '_blank');
    if (!originalHref) {
      throw new Error('original media href not found');
    }

    const original = await page.request.get(`${app.baseURL}${originalHref}`);
    expect(original.status()).toBe(200);
    expect(original.headers()['content-type']).toContain('image/png');
  });
});

test.describe('phase 3 downloads, range requests, and media headers', () => {
  test('uploaded media headers, browser downloads, stale transcode redirects, and protected denials are safe', async ({ page, app }, testInfo) => {
    test.skip(
      !DOWNLOAD_HEADER_PROJECTS.has(testInfo.project.name),
      'download and header flow coverage runs in Chromium and Firefox',
    );

    const imageThread = await createThread(page, app, 'img', {
      subject: 'phase 3 image headers',
      body: 'image header checks',
      filePath: app.fixtures().tinyPng,
    });
    const imageHref = await firstFileHref(page);

    const videoThread = await createThread(page, app, 'vid', {
      subject: 'phase 3 video headers',
      body: 'video range checks',
      filePath: app.fixtures().fakeMp4,
    });
    const videoHref = await firstFileHref(page);

    const audioThread = await createThread(page, app, 'aud', {
      subject: 'phase 3 audio headers',
      body: 'audio header checks',
      filePath: app.fixtures().fakeOgg,
    });
    const audioHref = await firstFileHref(page);

    const pdfBoard = uniqueShort('hdrpdf', testInfo);
    app.createBoardCli({ short: pdfBoard, name: 'Header PDF' });
    await updateBoardSettings(page, app, pdfBoard, { allowPdf: true });
    await createThread(page, app, pdfBoard, {
      subject: 'phase 3 pdf headers',
      body: 'pdf header checks',
      filePath: app.fixtures().tinyPdf,
    });
    const pdfHref = await firstFileHref(page);

    const anyBoard = uniqueShort('any', testInfo);
    app.createBoardCli({ short: anyBoard, name: 'Generic Attachments' });
    await updateBoardSettings(page, app, anyBoard, { allowAnyFiles: true });
    const genericFile = path.join(app.fixtureDir, 'phase3-generic-download.bin');
    await fsp.writeFile(genericFile, Buffer.from('phase 3 generic attachment\n'));
    const genericThread = await createThread(page, app, anyBoard, {
      subject: 'phase 3 generic attachment',
      body: 'generic downloads should be attachments',
      filePath: genericFile,
    });
    const genericHref = await firstFileHref(page);

    await expectContentHeaders(page, app, imageHref, {
      type: 'image/png',
      disposition: false,
    });
    await expectContentHeaders(page, app, videoHref, {
      type: 'video/mp4',
      disposition: false,
    });
    await expectContentHeaders(page, app, audioHref, {
      type: 'audio/ogg',
      disposition: false,
    });
    const pdf = await expectContentHeaders(page, app, pdfHref, {
      type: 'application/pdf',
      disposition: false,
    });
    expect(pdf.headers()['x-frame-options']).toBe('SAMEORIGIN');
    expect(pdf.headers()['content-security-policy']).toContain("frame-ancestors 'self'");

    const generic = await expectContentHeaders(page, app, genericHref, {
      type: 'application/octet-stream',
      disposition: true,
    });
    expect(generic.headers()['content-disposition']).toMatch(/^attachment; filename="[^"\\/]+\.bin"$/);

    const range = await page.request.get(`${app.baseURL}${videoHref}`, {
      headers: { Range: 'bytes=0-3' },
    });
    expect([200, 206]).toContain(range.status());
    if (range.status() === 206) {
      expect(range.headers()['content-range']).toMatch(/^bytes 0-3\//);
    }
    expect(range.headers()['x-content-type-options']).toBe('nosniff');

    await page.goto(`${app.baseURL}/${anyBoard}/thread/${genericThread}`);
    const [download] = await Promise.all([
      page.waitForEvent('download'),
      page.locator('.file-download .file-info a').first().click(),
    ]);
    expect(download.suggestedFilename()).toMatch(/^[^"\\/]+\.bin$/);
    expect(await download.path()).toBeTruthy();

    const staleWebmRel = await writeBoardFile(app, 'vid', 'phase3-stale-transcode.webm', Buffer.from('webm'));
    expect(staleWebmRel).toBe('vid/phase3-stale-transcode.webm');
    const stale = await page.request.get(`${app.baseURL}/boards/vid/phase3-stale-transcode.mp4`, {
      maxRedirects: 0,
    });
    expect([301, 308]).toContain(stale.status());
    expect(stale.headers().location).toBe('/boards/vid/phase3-stale-transcode.webm');

    const protectedBoard = uniqueShort('pmedia', testInfo);
    await restartWithProtectedBoard(app, protectedBoard);
    await createThread(page, app, protectedBoard, {
      subject: 'protected media',
      body: 'protected media download denial',
      filePath: app.fixtures().tinyPng,
    });
    const protectedHref = await firstFileHref(page);
    await page.context().clearCookies();
    const denied = await page.request.get(`${app.baseURL}${protectedHref}`, { maxRedirects: 0 });
    expect(denied.status()).toBe(403);
    const deniedBody = await expectSafeResponse(denied);
    expect(deniedBody).not.toContain('protected media download denial');

    for (const threadId of [imageThread, videoThread, audioThread]) {
      expect(Number.isInteger(threadId)).toBe(true);
    }
  });
});

function opPostId(app: RustChanServer, threadId: number): number {
  const raw = sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 1 LIMIT 1;`);
  const id = Number(raw);
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error(`OP post for thread ${threadId} not found`);
  }
  return id;
}

async function firstFileHref(page: Page): Promise<string> {
  const href = await page.locator('.file-info a[href^="/boards/"]').first().getAttribute('href');
  if (!href) {
    throw new Error('media href not found');
  }
  return href;
}

async function expectContentHeaders(
  page: Page,
  app: RustChanServer,
  href: string,
  options: { type: string; disposition: boolean },
) {
  const response = await page.request.get(`${app.baseURL}${href}`);
  expect(response.status(), href).toBe(200);
  expect(response.headers()['content-type']).toContain(options.type);
  expect(response.headers()['x-content-type-options']).toBe('nosniff');
  expect(response.headers()['cache-control']).toBeTruthy();
  if (options.disposition) {
    expect(response.headers()['content-disposition']).toContain('attachment');
  } else {
    expect(response.headers()['content-disposition'] ?? '').toBe('');
  }
  return response;
}

async function writeBoardFile(
  app: RustChanServer,
  board: string,
  rel: string,
  bytes: Buffer,
): Promise<string> {
  const logical = `${board}/${rel}`;
  const target = path.join(app.dataDir, 'boards', logical);
  await fsp.mkdir(path.dirname(target), { recursive: true });
  await fsp.writeFile(target, bytes);
  return logical;
}

async function restartWithProtectedBoard(app: RustChanServer, board: string): Promise<void> {
  await app.stop();
  app.createBoardCli({ short: board, name: 'Protected Phase 3 Media' });
  setBoardFixtureSettings(app, board, {
    accessMode: 'view_password',
    accessPasswordHash: adminPasswordHash(app),
    postCooldownSecs: 0,
  });
  await app.start();
}

function sqlString(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}

function pdfThumbSvg(): string {
  return [
    '<svg xmlns="http://www.w3.org/2000/svg" width="96" height="128" viewBox="0 0 96 128">',
    '<rect width="96" height="128" rx="6" fill="#111"/>',
    '<rect x="14" y="18" width="68" height="92" rx="3" fill="#f7f7f7"/>',
    '<text x="48" y="72" text-anchor="middle" font-family="monospace" font-size="18" fill="#b00020">PDF</text>',
    '</svg>',
  ].join('');
}
