import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import * as zlib from 'node:zlib';
import type { Page } from '@playwright/test';
import {
  boardId,
  expect,
  expectSafePage,
  expectSafeResponse,
  publicCsrf,
  setBoardFixtureSettings,
  sqliteQuery,
  test,
  type RustChanServer,
} from './helpers';

const IMAGE_LIMIT = 256 * 1024;
const VIDEO_LIMIT = 700 * 1024;
const IMPOSSIBLE_LIMIT = 1024;

const toolchainSkipReason = requiredMediaToolchainSkipReason();
test.skip(!!toolchainSkipReason, toolchainSkipReason || '');

test.describe('auto-compress oversized uploads', () => {
  test('oversized image is compressed, accepted, served, and previewed', async ({ page, app }, testInfo) => {
    test.setTimeout(180_000);
    const board = uniqueBoard('acimg', testInfo.workerIndex);
    app.createBoardCli({ short: board, name: 'Auto Image' });
    setBoardFixtureSettings(app, board, {
      allowImages: true,
      allowVideo: false,
      maxImageSizeBytes: IMAGE_LIMIT,
      maxVideoSizeBytes: IMAGE_LIMIT,
      maxAudioSizeBytes: IMAGE_LIMIT,
    });
    const fixtures = await createAutoCompressFixtures(app, testInfo.workerIndex);
    expect(fs.statSync(fixtures.compressibleImage).size).toBeGreaterThan(IMAGE_LIMIT);

    const form = await openThreadForm(page, app, board, 'image auto-compress');
    const fileInput = form.locator('input[type="file"]').first();
    await fileInput.setInputFiles(fixtures.compressibleImage);
    await expect(page.locator('#compress-modal')).toBeVisible();
    await page.getByRole('button', { name: /auto-compress/i }).click();
    await expectCompressedSelection(fileInput, IMAGE_LIMIT);
    await expect(form.locator('.auto-compress-status')).toContainText(/Auto-compressed/);
    await expect(page.locator('#compress-modal')).toBeHidden({ timeout: 90_000 });

    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/\\d+`)),
      form.getByRole('button', { name: /post thread/i }).click(),
    ]);
    await expectSafePage(page);

    const media = await expectServedMediaUnderLimit(page, app, IMAGE_LIMIT, /image\//);
    await expectSafeResponse(media.response);
    const thumb = page.locator('[data-media-thumb="1"]').first();
    await expect(thumb).toBeVisible();
    await expect.poll(async () => thumb.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth > 0)).toBe(true);

    await page.goto(`${app.baseURL}/${board}/catalog`);
    await expectSafePage(page);
    await expect(page.locator('.catalog-item')).toHaveCount(1);
    await expect(page.locator('[data-media-thumb="1"]').first()).toBeVisible();
  });

  test('oversized video is compressed, accepted, served, playable, and thumbnailed', async ({ page, app }, testInfo) => {
    test.setTimeout(240_000);
    const board = uniqueBoard('acvid', testInfo.workerIndex);
    app.createBoardCli({ short: board, name: 'Auto Video' });
    setBoardFixtureSettings(app, board, {
      allowImages: false,
      allowVideo: true,
      maxImageSizeBytes: VIDEO_LIMIT,
      maxVideoSizeBytes: VIDEO_LIMIT,
      maxAudioSizeBytes: VIDEO_LIMIT,
    });
    const fixtures = await createAutoCompressFixtures(app, testInfo.workerIndex);
    expect(fs.statSync(fixtures.video).size).toBeGreaterThan(VIDEO_LIMIT);

    const form = await openThreadForm(page, app, board, 'video auto-compress');
    const fileInput = form.locator('input[type="file"]').first();
    await fileInput.setInputFiles(fixtures.video);
    await expect(page.locator('#compress-modal')).toBeVisible();
    await page.getByRole('button', { name: /auto-compress/i }).click();
    await expectCompressedSelection(fileInput, VIDEO_LIMIT);
    await expect(form.locator('.auto-compress-status')).toContainText(/Auto-compressed/);
    await expect(page.locator('#compress-modal')).toBeHidden({ timeout: 140_000 });

    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/\\d+`)),
      form.getByRole('button', { name: /post thread/i }).click(),
    ]);
    await expectSafePage(page);

    const media = await expectServedMediaUnderLimit(page, app, VIDEO_LIMIT, /video\/webm/);
    await expectSafeResponse(media.response);
    const videoThumb = page.getByRole('link', { name: /video thumbnail/i }).first();
    await expect(videoThumb).toBeVisible();
    await videoThumb.click();
    const video = page.locator('video.media-expanded-video').first();
    await expect(video).toBeVisible();
    await expectBrowserCanDecodeVideo(page, `${app.baseURL}${media.href}`);

    const thumbSrc = await page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    expect(thumbSrc).toBeTruthy();
    const thumbResponse = await page.request.get(`${app.baseURL}${thumbSrc}`);
    expect(thumbResponse.status()).toBe(200);
    expect(thumbResponse.headers()['content-type']).toContain('image/webp');
  });

  test('direct oversized upload without client compression rejects cleanly', async ({ page, app }, testInfo) => {
    const board = uniqueBoard('acnojs', testInfo.workerIndex);
    app.createBoardCli({ short: board, name: 'No Auto' });
    setBoardFixtureSettings(app, board, {
      allowImages: true,
      allowVideo: true,
      maxImageSizeBytes: 64 * 1024,
      maxVideoSizeBytes: 64 * 1024,
      maxAudioSizeBytes: 64 * 1024,
    });
    const fixtures = await createAutoCompressFixtures(app, testInfo.workerIndex);
    const csrf = await publicCsrf(page, app, `/${board}`);
    const before = postCount(app, board);

    const response = await page.request.post(`${app.baseURL}/${board}`, {
      multipart: {
        _csrf: csrf,
        submission_token: `direct-${Date.now()}`,
        body: 'direct oversized upload',
        file: {
          name: 'oversized.png',
          mimeType: 'image/png',
          buffer: await fsp.readFile(fixtures.stubbornImage),
        },
      },
      maxRedirects: 0,
    });

    expect(response.status()).toBe(413);
    const text = await expectSafeResponse(response);
    expect(text).toMatch(/File too large|Maximum upload size/i);
    expect(postCount(app, board)).toBe(before);
    await expectNoUploadStaging(app);
  });

  test('still-too-large browser compression fails without creating posts or staged files', async ({ page, app }, testInfo) => {
    test.setTimeout(120_000);
    const board = uniqueBoard('acfail', testInfo.workerIndex);
    app.createBoardCli({ short: board, name: 'Auto Fail' });
    setBoardFixtureSettings(app, board, {
      allowImages: true,
      allowVideo: false,
      maxImageSizeBytes: IMPOSSIBLE_LIMIT,
      maxVideoSizeBytes: IMPOSSIBLE_LIMIT,
      maxAudioSizeBytes: IMPOSSIBLE_LIMIT,
    });
    const fixtures = await createAutoCompressFixtures(app, testInfo.workerIndex);
    const before = postCount(app, board);

    const form = await openThreadForm(page, app, board, 'image impossible auto-compress');
    await form.locator('input[type="file"]').first().setInputFiles(fixtures.stubbornImage);
    await expect(page.locator('#compress-modal')).toBeVisible();
    await page.getByRole('button', { name: /auto-compress/i }).click();
    await expect(page.locator('#compress-progress-text')).toContainText(/Could not compress|Error:/, { timeout: 90_000 });

    expect(postCount(app, board)).toBe(before);
    await expectNoUploadStaging(app);
    await expectSafePage(page);
  });
});

async function openThreadForm(page, app: RustChanServer, board: string, body: string) {
  await page.goto(`${app.baseURL}/${board}`);
  await expectSafePage(page);
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  const form = page.locator(`form[action="/${board}"]`).first();
  await form.locator('textarea[name="body"]').fill(body);
  return form;
}

async function expectCompressedSelection(fileInput, limit: number): Promise<void> {
  await expect.poll(async () => fileInput.evaluate((input: HTMLInputElement) => ({
    size: input.files?.[0]?.size ?? 0,
    name: input.files?.[0]?.name ?? '',
    compressed: input.dataset.autoCompressed,
  })), { timeout: 120_000, intervals: [500, 1_000, 2_000] }).toMatchObject({
    compressed: '1',
  });
  const selected = await fileInput.evaluate((input: HTMLInputElement) => ({
    size: input.files?.[0]?.size ?? 0,
    name: input.files?.[0]?.name ?? '',
  }));
  expect(selected.name).toContain('_compressed.');
  expect(selected.size).toBeGreaterThan(0);
  expect(selected.size).toBeLessThanOrEqual(limit);
}

async function expectServedMediaUnderLimit(page, app: RustChanServer, limit: number, contentType: RegExp) {
  const href = await page.locator('.file-info a').first().getAttribute('href');
  expect(href).toBeTruthy();
  const response = await page.request.get(`${app.baseURL}${href}`);
  expect(response.status()).toBe(200);
  expect(response.headers()['content-type']).toMatch(contentType);
  const body = await response.body();
  expect(body.length).toBeGreaterThan(0);
  expect(body.length).toBeLessThanOrEqual(limit);
  return { href: href ?? '', response, body };
}

async function expectBrowserCanDecodeVideo(page: Page, src: string): Promise<void> {
  const decoded = await page.evaluate(async (url) => {
    const video = document.createElement('video');
    video.muted = true;
    video.playsInline = true;
    video.preload = 'metadata';
    video.src = url;
    document.body.appendChild(video);

    try {
      return await new Promise((resolve) => {
        let settled = false;
        const finish = (value: boolean) => {
          if (settled) return;
          settled = true;
          window.clearTimeout(timer);
          resolve(value);
        };
        const timer = window.setTimeout(() => finish(false), 15_000);
        const hasVideoFrame = () => video.readyState >= 1 && video.videoWidth > 0 && video.videoHeight > 0;
        video.onloadedmetadata = () => finish(hasVideoFrame());
        video.onloadeddata = () => finish(hasVideoFrame());
        video.onerror = () => finish(false);
        video.load();
      });
    } finally {
      video.removeAttribute('src');
      video.load();
      video.remove();
    }
  }, src);

  expect(decoded).toBe(true);
}

async function expectNoUploadStaging(app: RustChanServer): Promise<void> {
  const pending = path.join(app.dataDir, 'boards', '.pending');
  const entries = await fsp.readdir(pending).catch((error: NodeJS.ErrnoException) => {
    if (error.code === 'ENOENT') return [];
    throw error;
  });
  expect(entries).toEqual([]);
}

function postCount(app: RustChanServer, board: string): number {
  const id = boardId(app, board);
  return Number(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE board_id = ${id};`));
}

async function createAutoCompressFixtures(
  app: RustChanServer,
  workerIndex: number,
): Promise<{ compressibleImage: string; stubbornImage: string; video: string }> {
  const dir = path.join(app.fixtureDir, `auto-compress-${workerIndex}`);
  await fsp.mkdir(dir, { recursive: true });
  const compressibleImage = path.join(dir, 'oversized-gradient.bmp');
  const stubbornImage = path.join(dir, 'oversized-noise.png');
  const video = path.join(dir, 'oversized-video.webm');
  await fsp.writeFile(compressibleImage, bmpRgb(1200, 900, (x, y, channel) => {
    if (channel === 0) return (x * 255) / 1199;
    if (channel === 1) return (y * 255) / 899;
    return ((x + y) * 255) / (1199 + 899);
  }));
  await fsp.writeFile(stubbornImage, pngRgba(900, 900, (index) => {
    if (index % 4 === 3) return 255;
    return (index * 1103515245 + 12345) >>> 16;
  }));
  runTool(process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg', [
    '-hide_banner',
    '-loglevel',
    'error',
    '-y',
    '-f',
    'lavfi',
    '-i',
    'testsrc2=size=640x360:rate=24:duration=6',
    '-c:v',
    'libvpx-vp9',
    '-b:v',
    '3500k',
    '-crf',
    '10',
    '-pix_fmt',
    'yuv420p',
    video,
  ]);
  return { compressibleImage, stubbornImage, video };
}

function requiredMediaToolchainSkipReason(): string | undefined {
  if (process.env.RUSTCHAN_E2E_MEDIA_TOOLCHAIN !== '1') {
    return 'opt-in real media auto-compress pass only; set RUSTCHAN_E2E_MEDIA_TOOLCHAIN=1';
  }
  const ffmpeg = process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg';
  const ffprobe = process.env.RUSTCHAN_E2E_FFPROBE_PATH ?? 'ffprobe';
  if (spawnSync(ffmpeg, ['-version'], { stdio: 'ignore' }).status !== 0) {
    return `ffmpeg is required for auto-compress live fixtures: ${ffmpeg}`;
  }
  if (spawnSync(ffprobe, ['-version'], { stdio: 'ignore' }).status !== 0) {
    return `ffprobe is required for auto-compress live fixtures: ${ffprobe}`;
  }
  const encoders = spawnSync(ffmpeg, ['-hide_banner', '-encoders'], { encoding: 'utf8' });
  const output = `${encoders.stdout}\n${encoders.stderr}`;
  for (const encoder of ['libwebp', 'libvpx-vp9', 'libopus']) {
    if (!output.includes(encoder)) {
      return `ffmpeg encoder ${encoder} is required for auto-compress live verification`;
    }
  }
  return undefined;
}

function runTool(command: string, args: string[]): void {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error([`failed to run ${command}`, result.stdout, result.stderr].filter(Boolean).join('\n'));
  }
}

function uniqueBoard(prefix: string, workerIndex: number): string {
  return `${prefix}${workerIndex}${Date.now().toString(36).slice(-2)}`.toLowerCase().replace(/[^a-z0-9]/g, '').slice(0, 8);
}

function bmpRgb(width: number, height: number, pixelByte: (x: number, y: number, channel: number) => number): Buffer {
  const rowStride = Math.ceil((width * 3) / 4) * 4;
  const pixelBytes = rowStride * height;
  const out = Buffer.alloc(54 + pixelBytes);
  out.write('BM', 0, 'ascii');
  out.writeUInt32LE(out.length, 2);
  out.writeUInt32LE(54, 10);
  out.writeUInt32LE(40, 14);
  out.writeInt32LE(width, 18);
  out.writeInt32LE(height, 22);
  out.writeUInt16LE(1, 26);
  out.writeUInt16LE(24, 28);
  out.writeUInt32LE(pixelBytes, 34);
  for (let y = 0; y < height; y += 1) {
    const sourceY = height - 1 - y;
    const row = 54 + y * rowStride;
    for (let x = 0; x < width; x += 1) {
      const offset = row + x * 3;
      out[offset] = pixelByte(x, sourceY, 2) & 0xff;
      out[offset + 1] = pixelByte(x, sourceY, 1) & 0xff;
      out[offset + 2] = pixelByte(x, sourceY, 0) & 0xff;
    }
  }
  return out;
}

function pngRgba(width: number, height: number, pixelByte: (index: number) => number): Buffer {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  let sourceIndex = 0;
  for (let y = 0; y < height; y += 1) {
    const rowStart = y * (width * 4 + 1);
    raw[rowStart] = 0;
    for (let x = 0; x < width * 4; x += 1) {
      raw[rowStart + 1 + x] = pixelByte(sourceIndex) & 0xff;
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
