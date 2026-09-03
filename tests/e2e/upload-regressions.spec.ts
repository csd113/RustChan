import { test as base, expect, type Locator, type Page, type TestInfo, type WorkerInfo } from '@playwright/test';
import crypto from 'node:crypto';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  RustChanServer,
  extractCsrf,
} from './helpers';

type RuntimeMode = 'local' | 'external';

type Runtime = {
  mode: RuntimeMode;
  baseURL: string;
  fixtureDir: string;
  app?: RustChanServer;
  adminUsername: string;
  adminPassword: string;
};

type UploadFixture = {
  key: string;
  name: string;
  path: string;
  mimeType: string;
  kind: 'image' | 'audio' | 'video';
  buffer: Buffer;
};

type FilePayload = UploadFixture | {
  name: string;
  mimeType: string;
  buffer: Buffer;
  kind: 'audio' | 'video' | 'image';
};

const RUN_ENV = 'RUSTCHAN_UPLOAD_REGRESSION_E2E';
const EXTERNAL_BASE_ENV = 'RUSTCHAN_UPLOAD_BASE_URL';
const ADMIN_USER_ENV = 'RUSTCHAN_UPLOAD_ADMIN_USERNAME';
const ADMIN_PASSWORD_ENV = 'RUSTCHAN_UPLOAD_ADMIN_PASSWORD';
const MIB = 1024 * 1024;

const test = base.extend<{ runtime: Runtime }>({
  runtime: async ({}, use, workerInfo) => {
    const externalBase = process.env[EXTERNAL_BASE_ENV]?.replace(/\/+$/, '');
    if (externalBase) {
      const fixtureDir = await fsp.mkdtemp(path.join(os.tmpdir(), 'rustchan-upload-regressions-external-'));
      try {
        await use({
          mode: 'external',
          baseURL: externalBase,
          fixtureDir,
          adminUsername: process.env[ADMIN_USER_ENV] ?? ADMIN_USERNAME,
          adminPassword: process.env[ADMIN_PASSWORD_ENV] ?? ADMIN_PASSWORD,
        });
      } finally {
        await fsp.rm(fixtureDir, { recursive: true, force: true });
      }
      return;
    }

    const app = await RustChanServer.create(workerInfo as WorkerInfo, {
      env: {
        CHAN_ENABLE_ANY_FILE_UPLOADS_FEATURE: '1',
        CHAN_FFMPEG_PATH: process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg',
        CHAN_FFPROBE_PATH: process.env.RUSTCHAN_E2E_FFPROBE_PATH ?? 'ffprobe',
        CHAN_PUBLIC_HOSTS: 'localhost,127.0.0.1,::1',
      },
    });
    try {
      app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
      await app.start();
      await use({
        mode: 'local',
        baseURL: app.baseURL,
        fixtureDir: app.fixtureDir,
        app,
        adminUsername: ADMIN_USERNAME,
        adminPassword: ADMIN_PASSWORD,
      });
    } finally {
      await app.dispose();
    }
  },
});

test.describe('upload regressions (ignored by default)', () => {
  test.skip(process.env[RUN_ENV] !== '1', `set ${RUN_ENV}=1 to run this opt-in upload suite`);

  test('posting UI accepts audio/MKV variants and keeps textarea resize disabled', async ({ page, runtime }, testInfo) => {
    test.skip(!toolAvailable(process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg'), 'ffmpeg is required');
    test.skip(!toolAvailable(process.env.RUSTCHAN_E2E_FFPROBE_PATH ?? 'ffprobe'), 'ffprobe is required');
    test.setTimeout(180_000);

    const fixtures = await createFixtures(runtime.fixtureDir);
    const mediaBoard = uniqueShort('ureg', testInfo);
    const noAudioBoard = uniqueShort('uano', testInfo);
    const noVideoBoard = uniqueShort('uvno', testInfo);

    await adminLogin(page, runtime);
    await createBoard(page, runtime, mediaBoard, 'Upload Regressions');
    await createBoard(page, runtime, noAudioBoard, 'Upload Regressions No Audio');
    await createBoard(page, runtime, noVideoBoard, 'Upload Regressions No Video');
    await updateBoard(page, runtime, mediaBoard, {
      allowImages: true,
      allowVideo: true,
      allowAudio: true,
      maxImageSizeMb: 1,
      maxVideoSizeMb: 1,
      maxAudioSizeMb: 1,
    });
    await updateBoard(page, runtime, noAudioBoard, {
      allowImages: true,
      allowVideo: true,
      allowAudio: false,
      maxImageSizeMb: 1,
      maxVideoSizeMb: 1,
      maxAudioSizeMb: 1,
    });
    await updateBoard(page, runtime, noVideoBoard, {
      allowImages: false,
      allowVideo: false,
      allowAudio: true,
      maxImageSizeMb: 1,
      maxVideoSizeMb: 1,
      maxAudioSizeMb: 1,
    });

    await assertTextareaNotResizable(page, runtime, mediaBoard);
    await postUiThread(page, runtime, mediaBoard, fixtures.png);

    for (const fixture of [
      fixtures.flac,
      fixtures.mp3,
      fixtures.wav,
      fixtures.ogg,
      fixtures.oga,
      fixtures.m4a,
      fixtures.aac,
      fixtures.opus,
      fixtures.webmAudio,
    ]) {
      await postUiThread(page, runtime, mediaBoard, fixture);
    }

    const mkvMimeVariants = [
      { name: 'tiny-video-x-matroska.mkv', mimeType: 'video/x-matroska' },
      { name: 'tiny-video-matroska.mkv', mimeType: 'video/matroska' },
      { name: 'tiny-octet-stream.mkv', mimeType: 'application/octet-stream' },
      { name: 'tiny-blank-mime.mkv', mimeType: '' },
    ];
    for (const variant of mkvMimeVariants) {
      await postUiThread(page, runtime, mediaBoard, {
        ...variant,
        buffer: fixtures.mkv.buffer,
        kind: 'video',
      });
    }

    await expectUiUploadError(page, runtime, mediaBoard, fixtures.fakeMkv, /matroska|ffprobe|validate|streams/i);
    await expectUiUploadError(page, runtime, noAudioBoard, fixtures.flac, /audio uploads are disabled/i);
    await expectUiUploadError(page, runtime, noVideoBoard, fixtures.mkv, /video uploads are disabled/i);
    await expectRequestUploadError(page, runtime, mediaBoard, fixtures.overLimitOgg, /too large|maximum audio upload size/i);
    await expectRequestUploadError(page, runtime, mediaBoard, fixtures.overLimitMkv, /too large|maximum video upload size/i);
  });
});

async function postUiThread(page: Page, runtime: Runtime, board: string, file: FilePayload): Promise<void> {
  await page.goto(`${runtime.baseURL}/${board}`);
  await revealPostForm(page);
  const form = page.locator(`form[action="/${board}"]`).first();
  await form.locator('input[name="subject"]').fill(`upload ${Date.now()}`);
  await form.locator('textarea[name="body"]').fill(`upload ${file.name}`);
  await setFile(form.locator('input[type="file"]').first(), file);
  const [response] = await Promise.all([
    page.waitForResponse((candidate) => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${board}`),
    form.getByRole('button', { name: /post thread/i }).click(),
  ]);
  expect(response.status(), await response.text().catch(() => '')).toBeLessThan(400);
  await page.waitForURL(new RegExp(`/${board}/thread/\\d+`));
  await verifyRenderedUpload(page, runtime, file.kind);
}

async function expectUiUploadError(page: Page, runtime: Runtime, board: string, file: FilePayload, pattern: RegExp): Promise<void> {
  await page.goto(`${runtime.baseURL}/${board}`);
  await revealPostForm(page);
  const form = page.locator(`form[action="/${board}"]`).first();
  await form.locator('input[name="subject"]').fill(`reject ${Date.now()}`);
  await form.locator('textarea[name="body"]').fill(`reject ${file.name}`);
  await setFile(form.locator('input[type="file"]').first(), file);
  const [response] = await Promise.all([
    page.waitForResponse((candidate) => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${board}`),
    form.getByRole('button', { name: /post thread/i }).click(),
  ]);
  expect(response.status()).not.toBeGreaterThanOrEqual(500);
  await expect(page.locator('.post-error-banner').first()).toContainText(pattern);
}

async function expectRequestUploadError(page: Page, runtime: Runtime, board: string, file: FilePayload, pattern: RegExp): Promise<void> {
  const csrf = await postFormCsrf(page, runtime, board);
  const response = await page.request.post(`${runtime.baseURL}/${board}`, {
    multipart: {
      _csrf: csrf,
      submission_token: `reject-${file.name}-${Date.now()}-${Math.random()}`,
      subject: `reject ${Date.now()}`,
      body: `reject ${file.name}`,
      file: {
        name: file.name,
        mimeType: file.mimeType,
        buffer: file.buffer,
      },
    },
    maxRedirects: 0,
    timeout: 60_000,
  });
  const text = await response.text();
  expect(response.status(), text).not.toBeGreaterThanOrEqual(500);
  expect(response.status(), text).toBeGreaterThanOrEqual(400);
  expect(response.status(), text).not.toBe(303);
  expect(stripTags(text)).toMatch(pattern);
}

async function assertTextareaNotResizable(page: Page, runtime: Runtime, board: string): Promise<void> {
  await page.goto(`${runtime.baseURL}/${board}`);
  await revealPostForm(page);
  const textarea = page.locator(`form[action="/${board}"] textarea[name="body"]`).first();
  await expect(textarea).toHaveCSS('resize', 'none');
  const before = await textarea.boundingBox();
  if (!before) throw new Error('textarea box not available');
  await page.mouse.move(before.x + before.width - 2, before.y + before.height - 2);
  await page.mouse.down();
  await page.mouse.move(before.x + before.width + 80, before.y + before.height + 80);
  await page.mouse.up();
  const after = await textarea.boundingBox();
  if (!after) throw new Error('textarea box not available after drag');
  expect(Math.round(after.height)).toBe(Math.round(before.height));
}

async function setFile(locator: Locator, file: FilePayload): Promise<void> {
  await locator.setInputFiles({
    name: file.name,
    mimeType: file.mimeType,
    buffer: file.buffer,
  });
}

async function verifyRenderedUpload(page: Page, runtime: Runtime, kind: FilePayload['kind']): Promise<void> {
  const link = page.locator('.file-info a[href^="/boards/"]').last();
  await expect(link).toBeVisible();
  const href = await link.getAttribute('href');
  if (!href) throw new Error('media link missing after upload');
  const response = await page.request.get(`${runtime.baseURL}${href}`);
  expect(response.status()).toBe(200);
  if (kind === 'audio') {
    await expect(page.locator('.audio-container').last()).toBeVisible();
  } else {
    await expect(page.locator('.media-preview, [data-media-thumb="1"]').last()).toBeVisible();
  }
}

async function revealPostForm(page: Page): Promise<void> {
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"], [data-action="toggle-post-form"]').first();
  if (await toggle.isVisible().catch(() => false)) {
    await toggle.click();
  }
}

async function adminLogin(page: Page, runtime: Runtime): Promise<void> {
  await page.goto(`${runtime.baseURL}/admin`);
  if (page.url().includes('/admin/panel')) return;
  await page.getByLabel('Username').fill(runtime.adminUsername);
  await page.getByLabel('Password').fill(runtime.adminPassword);
  await Promise.all([
    page.waitForURL(/\/admin\/panel/),
    page.getByRole('button', { name: 'authenticate' }).click(),
  ]);
}

async function createBoard(page: Page, runtime: Runtime, short: string, name: string): Promise<void> {
  const csrf = extractCsrf(await adminPanelHtml(page, runtime));
  const response = await page.request.post(`${runtime.baseURL}/admin/board/create`, {
    form: {
      _csrf: csrf,
      short_name: short,
      name,
      description: `${name} board`,
    },
    maxRedirects: 0,
  });
  if (![303, 409].includes(response.status())) {
    throw new Error(`create board /${short}/ failed with ${response.status()}: ${await response.text()}`);
  }
}

async function updateBoard(
  page: Page,
  runtime: Runtime,
  short: string,
  settings: {
    allowImages: boolean;
    allowVideo: boolean;
    allowAudio: boolean;
    maxImageSizeMb: number;
    maxVideoSizeMb: number;
    maxAudioSizeMb: number;
    maxPdfSizeMb?: number;
  },
): Promise<void> {
  const html = await adminPanelHtml(page, runtime);
  const csrf = extractCsrf(html);
  const boardId = extractBoardId(html, short);
  const form: Record<string, string> = {
    _csrf: csrf,
    board_id: String(boardId),
    name: `${short.toUpperCase()} Board`,
    description: `${short} upload regression`,
    bump_limit: '300',
    max_threads: '150',
    max_archived_threads: '150',
    post_cooldown_secs: '0',
    max_image_size_mb: String(settings.maxImageSizeMb),
    max_video_size_mb: String(settings.maxVideoSizeMb),
    max_audio_size_mb: String(settings.maxAudioSizeMb),
    max_pdf_size_mb: String(settings.maxPdfSizeMb ?? settings.maxImageSizeMb),
    default_theme: '',
    banner_mode: 'inherit',
    access_mode: 'public',
    access_password: '',
    allow_tripcodes: '1',
    allow_video_embeds: '1',
  };
  if (settings.allowImages) form.allow_images = '1';
  if (settings.allowVideo) form.allow_video = '1';
  if (settings.allowAudio) form.allow_audio = '1';
  const response = await page.request.post(`${runtime.baseURL}/admin/board/settings`, {
    form,
    maxRedirects: 0,
  });
  if (response.status() !== 303) {
    throw new Error(`update board /${short}/ failed with ${response.status()}: ${await response.text()}`);
  }
}

async function adminPanelHtml(page: Page, runtime: Runtime): Promise<string> {
  const response = await page.request.get(`${runtime.baseURL}/admin/panel`);
  if (response.status() !== 200) throw new Error(`admin panel returned ${response.status()}`);
  return response.text();
}

async function postFormCsrf(page: Page, runtime: Runtime, board: string): Promise<string> {
  const actionPath = `/${board}`;
  const response = await page.request.get(`${runtime.baseURL}${actionPath}`);
  if (response.status() !== 200) {
    throw new Error(`GET ${actionPath} for CSRF returned ${response.status()}`);
  }
  return extractPostFormCsrf(await response.text(), actionPath);
}

function extractPostFormCsrf(html: string, actionPath: string): string {
  const escapedAction = actionPath.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const form = html.match(new RegExp(`<form[^>]+action="${escapedAction}"[\\s\\S]*?</form>`));
  if (!form) {
    throw new Error(`post form with action ${actionPath} not found`);
  }
  return extractCsrf(form[0]);
}

function stripTags(html: string): string {
  return html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ');
}

function extractBoardId(html: string, short: string): number {
  const escaped = short.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const details = new RegExp(`<details[^>]+id="board-${escaped}"[\\s\\S]*?<input type="hidden" name="board_id" value="(\\d+)"`);
  const match = html.match(details);
  if (!match) throw new Error(`board id for /${short}/ not found`);
  return Number(match[1]);
}

function uniqueShort(prefix: string, testInfo: TestInfo): string {
  return `${prefix}${crypto.createHash('sha1').update(`${testInfo.workerIndex}-${Date.now()}-${Math.random()}`).digest('hex').slice(0, 4)}`.slice(0, 8);
}

async function createFixtures(dir: string): Promise<Record<string, UploadFixture>> {
  await fsp.mkdir(dir, { recursive: true });
  const files: Record<string, UploadFixture> = {};
  const add = async (key: string, name: string, mimeType: string, kind: UploadFixture['kind'], buffer: Buffer) => {
    const filePath = path.join(dir, `${key}-${name}`);
    await fsp.writeFile(filePath, buffer);
    files[key] = { key, name, path: filePath, mimeType, kind, buffer };
  };

  await add('png', 'control.png', 'image/png', 'image', ffmpegBytes(dir, 'control.png', ['-f', 'lavfi', '-i', 'color=c=red:s=2x2:d=0.01', '-frames:v', '1']));
  await add('flac', 'tiny.flac', 'audio/x-flac', 'audio', ffmpegBytes(dir, 'tiny.flac', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'flac']));
  await add('mp3', 'tiny.mp3', 'audio/mp3', 'audio', ffmpegBytes(dir, 'tiny.mp3', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'libmp3lame', '-b:a', '64k']));
  await add('wav', 'tiny.wav', 'audio/x-wav', 'audio', ffmpegBytes(dir, 'tiny.wav', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'pcm_s16le']));
  await add('ogg', 'tiny.ogg', 'application/ogg', 'audio', ffmpegBytes(dir, 'tiny.ogg', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'libvorbis']));
  await add('oga', 'tiny.oga', 'audio/oga', 'audio', ffmpegBytes(dir, 'tiny.oga', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'libvorbis']));
  await add('m4a', 'tiny.m4a', 'audio/x-m4a', 'audio', ffmpegBytes(dir, 'tiny.m4a', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'aac', '-b:a', '64k']));
  await add('aac', 'tiny.aac', 'audio/x-aac', 'audio', ffmpegBytes(dir, 'tiny.aac', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'aac', '-b:a', '64k', '-f', 'adts']));
  await add('opus', 'tiny.opus', 'audio/opus', 'audio', ffmpegBytes(dir, 'tiny.opus', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'libopus', '-b:a', '32k']));
  await add('webmAudio', 'tiny-audio.webm', 'audio/webm', 'audio', ffmpegBytes(dir, 'tiny-audio.webm', ['-f', 'lavfi', '-i', 'sine=frequency=440:duration=0.05', '-c:a', 'libopus', '-b:a', '32k', '-f', 'webm']));
  await add('mkv', 'tiny.mkv', 'video/x-matroska', 'video', ffmpegBytes(dir, 'tiny.mkv', ['-f', 'lavfi', '-i', 'color=c=black:s=16x16:d=0.1', '-an', '-c:v', 'mpeg4', '-f', 'matroska']));
  await add('fakeMkv', 'fake.mkv', 'video/x-matroska', 'video', Buffer.from('\x1a\x45\xdf\xa3\xa3\x42\x86\x81\x01\x42\xf7\x81\x01\x42\xf2\x81\x04\x42\xf3\x81\x08\x42\x82\x88matroska\x42\x87\x81\x04not real media', 'binary'));
  await add('overLimitOgg', 'over-limit.ogg', 'audio/ogg', 'audio', Buffer.concat([Buffer.from('OggS'), Buffer.alloc(MIB + 1)]));
  await add('overLimitMkv', 'over-limit.mkv', 'video/x-matroska', 'video', Buffer.concat([files.mkv.buffer, Buffer.alloc(MIB + 1)]));
  return files;
}

function ffmpegBytes(dir: string, name: string, args: string[]): Buffer {
  const output = path.join(dir, `generated-${name}`);
  const ffmpeg = process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg';
  const result = spawnSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', ...args, output], {
    encoding: 'utf8',
  });
  if (result.status !== 0) {
    throw new Error(`ffmpeg fixture ${name} failed: ${result.stderr || result.stdout}`);
  }
  return fs.readFileSync(output);
}

function toolAvailable(program: string): boolean {
  return spawnSync(program, ['-version'], { stdio: 'ignore' }).status === 0;
}
