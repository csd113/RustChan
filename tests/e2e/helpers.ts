import { test as base, expect, type APIResponse, type Page, type TestInfo, type WorkerInfo } from '@playwright/test';
import { spawn, spawnSync, type ChildProcess, type ChildProcessByStdio } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import type { Readable } from 'node:stream';
import * as zlib from 'node:zlib';

export const ADMIN_USERNAME = 'admin';
export const ADMIN_PASSWORD = 'AdminPass123!';

const repoRoot = path.resolve(__dirname, '../..');
const sourceBinary = path.join(repoRoot, 'target/debug/rustchan-cli');

type SeedBoard = {
  short: string;
  name: string;
  description?: string;
  nsfw?: boolean;
  noImages?: boolean;
  noVideos?: boolean;
  audio?: boolean;
};

type BoardSettings = {
  name?: string;
  description?: string;
  nsfw?: boolean;
  allowImages?: boolean;
  allowVideo?: boolean;
  allowAudio?: boolean;
  allowPdf?: boolean;
  allowAnyFiles?: boolean;
  allowArchive?: boolean;
  allowEditing?: boolean;
  allowSelfDelete?: boolean;
  allowCaptcha?: boolean;
  accessMode?: 'public' | 'view_password' | 'post_password';
  accessPassword?: string;
  clearAccessPassword?: boolean;
  defaultTheme?: string;
  maxImageSizeMb?: number;
  maxVideoSizeMb?: number;
  maxAudioSizeMb?: number;
  maxPdfSizeMb?: number;
  bumpLimit?: number;
  maxThreads?: number;
  maxArchivedThreads?: number;
  postCooldownSecs?: number;
};

type BoardFixtureSettings = BoardSettings & {
  maxImageSizeBytes?: number;
  maxVideoSizeBytes?: number;
  maxAudioSizeBytes?: number;
  maxPdfSizeBytes?: number;
  accessPasswordHash?: string;
  defaultTheme?: string;
};

type SiteFixtureSettings = {
  siteName?: string;
  siteSubtitle?: string;
  defaultTheme?: string;
  homepageNewThreadBadgesEnabled?: boolean;
  homepageNewReplyBadgesEnabled?: boolean;
  threadNewReplyBadgesEnabled?: boolean;
};

type ServerOptions = {
  env?: Record<string, string>;
  preserveRoot?: boolean;
};

type StartOptions = {
  cwd?: string;
};

export type FixtureFiles = {
  tinyPng: string;
  spoofedPng: string;
  fakeMp4: string;
  fakeOgg: string;
  tinyPdf: string;
  invalid: string;
  oversized: string;
  oddNamePng: string;
};

export class RustChanServer {
  readonly rootDir: string;
  readonly binDir: string;
  readonly binaryPath: string;
  readonly dataDir: string;
  readonly logPath: string;
  readonly fixtureDir: string;
  readonly port: number;
  readonly baseURL: string;
  readonly mediaToolchain: boolean;
  readonly envOverrides: Record<string, string>;
  readonly preserveRoot: boolean;
  private portReservation?: net.Server;
  private logStream?: fs.WriteStream;
  process?: ChildProcessByStdio<null, Readable, Readable>;

  private constructor(
    rootDir: string,
    port: number,
    mediaToolchain: boolean,
    envOverrides: Record<string, string>,
    preserveRoot: boolean,
    portReservation: net.Server,
  ) {
    this.rootDir = rootDir;
    this.binDir = path.join(rootDir, 'bin');
    this.binaryPath = path.join(this.binDir, 'rustchan-cli');
    this.dataDir = path.join(this.binDir, 'rustchan-data');
    this.logPath = path.join(rootDir, 'server.log');
    this.fixtureDir = path.join(rootDir, 'fixtures');
    this.port = port;
    this.baseURL = `http://127.0.0.1:${port}`;
    this.mediaToolchain = mediaToolchain;
    this.envOverrides = envOverrides;
    this.preserveRoot = preserveRoot;
    this.portReservation = portReservation;
  }

  static async create(workerInfo?: WorkerInfo, options: ServerOptions = {}): Promise<RustChanServer> {
    const rootDir = await fsp.mkdtemp(path.join(os.tmpdir(), `rustchan-e2e-${workerInfo?.workerIndex ?? 'manual'}-`));
    const preserveRoot = options.preserveRoot ?? process.env.RUSTCHAN_E2E_PRESERVE_ROOTS === '1';
    let app: RustChanServer | undefined;
    try {
      const { port, server: portReservation } = await reserveFreePort();
      app = new RustChanServer(
        rootDir,
        port,
        process.env.RUSTCHAN_E2E_MEDIA_TOOLCHAIN === '1',
        options.env ?? {},
        preserveRoot,
        portReservation,
      );
      await fsp.mkdir(app.binDir, { recursive: true });
      await fsp.copyFile(sourceBinary, app.binaryPath);
      await fsp.chmod(app.binaryPath, 0o755);
      await app.createFixtureFiles();
      return app;
    } catch (error) {
      if (app) {
        await app.dispose().catch((cleanupError: unknown) => {
          console.error(`failed to dispose incomplete RustChan fixture ${rootDir}: ${String(cleanupError)}`);
        });
      } else {
        await removeFixtureRoot(rootDir, preserveRoot).catch((cleanupError: unknown) => {
          console.error(`failed to remove incomplete RustChan fixture ${rootDir}: ${String(cleanupError)}`);
        });
      }
      throw error;
    }
  }

  get env(): NodeJS.ProcessEnv {
    return {
      ...process.env,
      RUSTCHAN_SPAWNED: '1',
      RUST_BACKTRACE: '1',
      CHAN_HOST: '127.0.0.1',
      CHAN_PORT: String(this.port),
      CHAN_BIND: `127.0.0.1:${this.port}`,
      CHAN_TOR_SUPPORT: '0',
      CHAN_REQUIRE_FFMPEG: this.mediaToolchain ? '1' : '0',
      CHAN_FFMPEG_PATH: this.mediaToolchain
        ? (process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg')
        : '__rustchan_e2e_no_ffmpeg__',
      CHAN_FFPROBE_PATH: this.mediaToolchain
        ? (process.env.RUSTCHAN_E2E_FFPROBE_PATH ?? 'ffprobe')
        : '__rustchan_e2e_no_ffprobe__',
      CHAN_HTTPS_COOKIES: '0',
      CHAN_ENABLE_ANY_FILE_UPLOADS_FEATURE: '1',
      CHAN_RATE_GETS: '1000',
      CHAN_RATE_WINDOW: '1',
      CHAN_AUTO_FULL_BACKUP_HOURS: '0',
      CHAN_AUTO_FULL_BACKUP_STORAGE_MODE: 'directory',
      CHAN_AUTO_VACUUM_HOURS: '0',
      CHAN_WAL_CHECKPOINT_SECS: '0',
      CHAN_POLL_CLEANUP_HOURS: '0',
      CHAN_WAVEFORM_CACHE_MAX_MB: '1',
      CHAN_JOB_QUEUE_CAPACITY: '50',
      CHAN_PUBLIC_HOSTS: 'localhost,127.0.0.1,::1',
      ...this.envOverrides,
    };
  }

  dbPath(): string {
    return path.join(this.dataDir, 'chan.db');
  }

  async initializeDefaultData(): Promise<void> {
    this.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
    const boards: SeedBoard[] = [
      { short: 'pub', name: 'Public Board', description: 'General public discussion' },
      { short: 'img', name: 'Images', description: 'Image upload board' },
      { short: 'vid', name: 'Video', description: 'Video upload board' },
      { short: 'aud', name: 'Audio', description: 'Audio upload board', audio: true },
      { short: 'nsfw', name: 'NSFW Board', description: 'Adult test board', nsfw: true },
      { short: 'txt', name: 'Text Only', description: 'Uploads disabled', noImages: true, noVideos: true },
    ];
    for (const board of boards) {
      this.createBoardCli(board);
    }
  }

  createBoardCli(board: SeedBoard): void {
    const args = ['admin', 'create-board', board.short, board.name, board.description ?? ''];
    if (board.nsfw) args.push('--nsfw');
    if (board.noImages) args.push('--no-images');
    if (board.noVideos) args.push('--no-videos');
    if (board.audio) args.push('--audio');
    this.runCli(args);
  }

  runCli(args: string[]): void {
    const result = spawnSync(this.binaryPath, args, {
      cwd: this.binDir,
      env: this.env,
      encoding: 'utf8',
      killSignal: 'SIGKILL',
      timeout: 30_000,
    });
    fs.appendFileSync(this.logPath, [
      `$ ${this.binaryPath} ${args.join(' ')}`,
      result.stdout,
      result.stderr,
    ].join('\n'));
    if (result.error || result.status !== 0) {
      throw new Error(`rustchan-cli ${args.join(' ')} failed: ${result.error?.message || result.stderr || result.stdout}`);
    }
  }

  async start(options: StartOptions = {}): Promise<void> {
    if (this.process) {
      return;
    }
    await this.releasePortReservation();
    const out = fs.createWriteStream(this.logPath, { flags: 'a' });
    this.logStream = out;
    // Browser fixtures must stay headless even when Playwright itself runs in
    // a terminal. Ratatui and the first-run terminal wizard require both stdin
    // and stdout to be TTYs; never inherit those streams or send TUI shortcuts.
    const child = spawn(this.binaryPath, ['serve'], {
      cwd: options.cwd ?? this.binDir,
      env: this.env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    this.process = child;
    child.stdout.pipe(out, { end: false });
    child.stderr.pipe(out, { end: false });
    child.once('exit', (code, signal) => {
      fs.appendFileSync(this.logPath, `\n[server exited code=${code} signal=${signal}]\n`);
    });
    // Keep a bounded diagnostic tail so a terminal-initialization regression
    // reports its actual error rather than only a readiness timeout/exit code.
    let startupOutput = '';
    const captureStartup = (chunk: Buffer) => {
      startupOutput = (startupOutput + chunk.toString('utf8')).slice(-8_192);
    };
    child.stdout.on('data', captureStartup);
    child.stderr.on('data', captureStartup);
    try {
      await waitForReady(this.baseURL, child);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(
        `${message}\nRecent server output:\n${startupOutput.trim() || '(no output captured)'}`,
        { cause: error },
      );
    } finally {
      child.stdout.removeListener('data', captureStartup);
      child.stderr.removeListener('data', captureStartup);
    }
  }

  async stop(): Promise<void> {
    const child = this.process;
    if (child && !childHasExited(child)) {
      child.kill('SIGTERM');
      if (!(await waitForChildExit(child, 5_000))) {
        child.kill('SIGKILL');
        if (!(await waitForChildExit(child, 2_000))) {
          throw new Error(
            `rustchan fixture process ${child.pid ?? 'unknown'} did not exit after SIGTERM and SIGKILL`,
          );
        }
      }
    }
    if (child) {
      this.process = undefined;
    }
    await this.closeLogStream();
  }

  async dispose(): Promise<void> {
    // If the child cannot be stopped, retain both its handle and fixture root so
    // callers can retry cleanup without leaving an untracked live process.
    await this.stop();
    let firstError: unknown;
    try {
      await this.releasePortReservation();
    } catch (error) {
      firstError = error;
    }
    try {
      await removeFixtureRoot(this.rootDir, this.preserveRoot);
    } catch (error) {
      firstError ??= error;
    }
    if (firstError) {
      throw firstError;
    }
  }

  async restart(): Promise<void> {
    await this.stop();
    await this.start();
  }

  private async releasePortReservation(): Promise<void> {
    const reservation = this.portReservation;
    if (!reservation) {
      return;
    }
    this.portReservation = undefined;
    await new Promise<void>((resolve, reject) => {
      reservation.close((error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve();
      });
    });
  }

  private async closeLogStream(): Promise<void> {
    const stream = this.logStream;
    this.logStream = undefined;
    if (!stream || stream.closed) {
      return;
    }
    await new Promise<void>((resolve, reject) => {
      const onError = (error: Error) => {
        stream.removeListener('error', onError);
        reject(error);
      };
      stream.once('error', onError);
      stream.end(() => {
        stream.removeListener('error', onError);
        resolve();
      });
    });
  }

  async logs(): Promise<string> {
    return fsp.readFile(this.logPath, 'utf8').catch(() => '');
  }

  fixtures(): FixtureFiles {
    return {
      tinyPng: path.join(this.fixtureDir, 'tiny.png'),
      spoofedPng: path.join(this.fixtureDir, 'spoofed.png'),
      fakeMp4: path.join(this.fixtureDir, 'tiny.mp4'),
      fakeOgg: path.join(this.fixtureDir, 'tiny.ogg'),
      tinyPdf: path.join(this.fixtureDir, 'tiny.pdf'),
      invalid: path.join(this.fixtureDir, 'invalid.txt'),
      oversized: path.join(this.fixtureDir, 'oversized.bin'),
      oddNamePng: path.join(this.fixtureDir, '../fixtures/name with spaces unicode-é quotes \' \" .. slash.png'),
    };
  }

  private async createFixtureFiles(): Promise<void> {
    await fsp.mkdir(this.fixtureDir, { recursive: true });
    const tinyPng = pngRgba(1, 1, (index) => [0, 200, 64, 255][index % 4]);
    await fsp.writeFile(path.join(this.fixtureDir, 'tiny.png'), tinyPng);
    await fsp.writeFile(path.join(this.fixtureDir, 'spoofed.png'), Buffer.from('not actually a png'));
    if (this.mediaToolchain) {
      await this.createRealMediaFixtures();
    } else {
      await fsp.writeFile(path.join(this.fixtureDir, 'tiny.mp4'), Buffer.concat([
        Buffer.from([0x00, 0x00, 0x00, 0x18]),
        Buffer.from('ftypisom'),
        Buffer.from([0x00, 0x00, 0x02, 0x00]),
        Buffer.from('isomiso2mp41'),
        Buffer.from([0x00, 0x00, 0x00, 0x08]),
        Buffer.from('free'),
      ]));
      await fsp.writeFile(path.join(this.fixtureDir, 'tiny.ogg'), Buffer.concat([
        Buffer.from('OggS'),
        Buffer.alloc(64, 0),
      ]));
    }
    await fsp.writeFile(path.join(this.fixtureDir, 'tiny.pdf'), Buffer.from(
      '%PDF-1.1\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] >>\nendobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \ntrailer\n<< /Root 1 0 R /Size 4 >>\nstartxref\n186\n%%EOF\n',
    ));
    await fsp.writeFile(path.join(this.fixtureDir, 'invalid.txt'), 'plain text is not an accepted media file');
    await fsp.writeFile(path.join(this.fixtureDir, 'oversized.bin'), pngRgba(900, 900, (index) => (index * 37 + 19) % 256));
    await fsp.writeFile(this.fixtures().oddNamePng, tinyPng);
  }

  private async createRealMediaFixtures(): Promise<void> {
    const ffmpeg = process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg';
    runFixtureTool(ffmpeg, [
      '-hide_banner',
      '-loglevel',
      'error',
      '-y',
      '-f',
      'lavfi',
      '-i',
      'testsrc2=size=160x120:rate=12:duration=3',
      '-f',
      'lavfi',
      '-i',
      'sine=frequency=440:duration=3',
      '-c:v',
      'mpeg4',
      '-b:v',
      '900k',
      '-pix_fmt',
      'yuv420p',
      '-c:a',
      'aac',
      '-b:a',
      '128k',
      '-movflags',
      '+faststart',
      path.join(this.fixtureDir, 'tiny.mp4'),
    ]);
    runFixtureTool(ffmpeg, [
      '-hide_banner',
      '-loglevel',
      'error',
      '-y',
      '-f',
      'lavfi',
      '-i',
      'sine=frequency=880:duration=2',
      '-c:a',
      'libopus',
      path.join(this.fixtureDir, 'tiny.ogg'),
    ]);
  }
}

export const test = base.extend<{ app: RustChanServer; serverLogOnFailure: void }>({
  app: [async ({}, use, workerInfo) => {
    const app = await RustChanServer.create(workerInfo);
    try {
      await app.initializeDefaultData();
      await app.start();
      await use(app);
    } finally {
      await app.dispose();
    }
  }, { scope: 'worker', timeout: 120_000 }],

  serverLogOnFailure: [async ({ app }, use, testInfo: TestInfo) => {
    await use(undefined);
    if (testInfo.status !== testInfo.expectedStatus) {
      await testInfo.attach('rustchan-server.log', {
        body: await app.logs(),
        contentType: 'text/plain',
      });
    }
  }, { auto: true }],
});

export { expect };

export async function gotoAppPage(page: Page, url: string): Promise<void> {
  let lastError: unknown;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      await page.goto(url, {
        waitUntil: 'commit',
        timeout: attempt === 0 ? 10_000 : 20_000,
      });
      return;
    } catch (error) {
      lastError = error;
      if (hasReachedAppUrl(page.url(), url)) {
        return;
      }
      await page.waitForTimeout(250).catch(() => undefined);
    }
  }
  throw lastError instanceof Error ? lastError : new Error(`navigation failed for ${url}`);
}

function hasReachedAppUrl(currentUrl: string, targetUrl: string): boolean {
  try {
    const current = new URL(currentUrl);
    const target = new URL(targetUrl);
    return current.origin === target.origin
      && (current.pathname === target.pathname || current.pathname.startsWith(`${target.pathname}/`));
  } catch {
    return currentUrl === targetUrl;
  }
}

export async function adminLogin(page: Page, app: RustChanServer): Promise<void> {
  await gotoAppPage(page, `${app.baseURL}/admin`);
  if (page.url().includes('/admin/panel')) {
    await expectSafePage(page, { allowAdminInternals: true });
    return;
  }
  await page.getByLabel('Username').fill(ADMIN_USERNAME);
  await page.getByLabel('Password').fill(ADMIN_PASSWORD);
  await Promise.all([
    page.waitForURL(/\/admin\/panel/),
    page.getByRole('button', { name: 'authenticate' }).click(),
  ]);
  await expectSafePage(page, { allowAdminInternals: true });
}

export async function adminLogout(page: Page): Promise<void> {
  const form = page.locator('form.admin-panel-logout, form[action="/admin/logout"]').first();
  await Promise.all([
    page.waitForURL(/\/admin/),
    form.getByRole('button', { name: /logout/i }).click(),
  ]);
}

export async function createBoard(page: Page, app: RustChanServer, board: SeedBoard): Promise<void> {
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  const form = new URLSearchParams({
    _csrf: csrf,
    short_name: board.short,
    name: board.name,
    description: board.description ?? '',
  });
  if (board.nsfw) form.set('nsfw', '1');
  if (board.audio) form.set('allow_audio', '1');
  const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
    form: Object.fromEntries(form),
    maxRedirects: 0,
  });
  expect([303, 409]).toContain(response.status());
}

export async function updateBoardSettings(
  page: Page,
  app: RustChanServer,
  short: string,
  settings: BoardSettings,
): Promise<void> {
  await adminLogin(page, app);
  const html = await adminPanelHtml(page, app);
  const csrf = extractCsrf(html);
  const boardId = extractBoardId(html, short);
  const form = new URLSearchParams({
    _csrf: csrf,
    board_id: String(boardId),
    name: settings.name ?? displayNameFor(short),
    description: settings.description ?? `${short} e2e board`,
    bump_limit: String(settings.bumpLimit ?? 300),
    max_threads: String(settings.maxThreads ?? 150),
    max_archived_threads: String(settings.maxArchivedThreads ?? 150),
    post_cooldown_secs: String(settings.postCooldownSecs ?? 0),
    max_image_size_mb: String(settings.maxImageSizeMb ?? 8),
    max_video_size_mb: String(settings.maxVideoSizeMb ?? 50),
    max_audio_size_mb: String(settings.maxAudioSizeMb ?? 150),
    max_pdf_size_mb: String(settings.maxPdfSizeMb ?? 8),
    default_theme: settings.defaultTheme ?? '',
    banner_mode: 'inherit',
    access_mode: settings.accessMode ?? 'public',
    access_password: settings.accessPassword ?? '',
  });
  setCheck(form, 'nsfw', settings.nsfw ?? false);
  setCheck(form, 'allow_images', settings.allowImages ?? true);
  setCheck(form, 'allow_video', settings.allowVideo ?? true);
  setCheck(form, 'allow_audio', settings.allowAudio ?? false);
  setCheck(form, 'allow_pdf', settings.allowPdf ?? false);
  setCheck(form, 'allow_any_files', settings.allowAnyFiles ?? false);
  setCheck(form, 'allow_archive', settings.allowArchive ?? false);
  setCheck(form, 'allow_tripcodes', true);
  setCheck(form, 'allow_editing', settings.allowEditing ?? false);
  setCheck(form, 'allow_self_delete', settings.allowSelfDelete ?? false);
  setCheck(form, 'allow_video_embeds', true);
  setCheck(form, 'show_poster_ids', false);
  setCheck(form, 'collapse_greentext', false);
  if (settings.clearAccessPassword) form.set('clear_access_password', '1');

  const response = await page.request.post(`${app.baseURL}/admin/board/settings`, {
    form: Object.fromEntries(form),
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
}

export async function createThread(
  page: Page,
  app: RustChanServer,
  board: string,
  options: { subject?: string; body?: string; filePath?: string } = {},
): Promise<number> {
  await gotoAppPage(page, `${app.baseURL}/${board}`);
  await expectSafePage(page);
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  const form = page.locator(`form[action="/${board}"]`).first();
  await expect(form.locator('input[name="subject"]')).toBeVisible();
  await form.locator('input[name="subject"]').fill(options.subject ?? `subject ${Date.now()}`);
  await form.locator('textarea[name="body"]').fill(options.body ?? `thread body ${Date.now()}`);
  if (options.filePath) {
    await form.locator('input[type="file"]').first().setInputFiles(options.filePath);
  }
  const threadUrl = new RegExp(`/${board}/thread/\\d+`);
  const submit = form.getByRole('button', { name: /post thread/i });
  let lastError: unknown;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    await submit.scrollIntoViewIfNeeded();
    try {
      await Promise.all([
        page.waitForURL(threadUrl, { waitUntil: 'domcontentloaded', timeout: attempt === 0 ? 10_000 : 20_000 }),
        submit.click(),
      ]);
      await expectSafePage(page);
      return threadIdFromUrl(page.url());
    } catch (error) {
      lastError = error;
      if (threadUrl.test(page.url())) {
        await expectSafePage(page);
        return threadIdFromUrl(page.url());
      }
      const banner = page.locator('.post-error-banner').first();
      if (await banner.isVisible().catch(() => false)) {
        throw new Error(`thread creation failed: ${await banner.innerText()}`);
      }
    }
  }
  throw lastError instanceof Error ? lastError : new Error(`thread creation failed for /${board}`);
}

export async function createReply(
  page: Page,
  app: RustChanServer,
  board: string,
  threadId: number,
  body = `reply body ${Date.now()}`,
): Promise<void> {
  await gotoAppPage(page, `${app.baseURL}/${board}/thread/${threadId}`);
  await expectSafePage(page);
  const toggle = page.locator('[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  const form = page.locator(`form[action="/${board}/thread/${threadId}"]`).first();
  await form.locator('textarea[name="body"]').fill(body);
  const replyPath = `/${board}/thread/${threadId}`;
  const postCountBefore = Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`,
  ));
  const [response] = await Promise.all([
    page.waitForResponse((candidate) => (
      candidate.request().method() === 'POST'
      && new URL(candidate.url()).pathname === replyPath
    )),
    form.getByRole('button', { name: /post reply/i }).click(),
  ]);
  expect([302, 303]).toContain(response.status());
  await expect.poll(
    () => Number(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`)),
  ).toBe(postCountBefore + 1);
  await expect(page.locator('.post')).toHaveCount(postCountBefore + 1);
  await expectSafePage(page);
}

export async function createThreadViaRequest(
  page: Page,
  app: RustChanServer,
  board: string,
  options: { subject?: string; body?: string } = {},
): Promise<number> {
  const csrf = await publicCsrf(page, app, `/${board}`);
  const response = await page.request.post(`${app.baseURL}/${board}`, {
    multipart: {
      _csrf: csrf,
      submission_token: uniqueSubmissionToken(board),
      subject: options.subject ?? `subject ${Date.now()}`,
      body: options.body ?? `thread body ${Date.now()}`,
    },
    maxRedirects: 0,
  });
  expect([302, 303]).toContain(response.status());
  const location = response.headers().location ?? '';
  return threadIdFromUrl(location);
}

export async function createReplyViaRequest(
  page: Page,
  app: RustChanServer,
  board: string,
  threadId: number,
  body = `reply body ${Date.now()}`,
): Promise<void> {
  const csrf = await publicCsrf(page, app, `/${board}/thread/${threadId}`);
  const response = await page.request.post(`${app.baseURL}/${board}/thread/${threadId}`, {
    multipart: {
      _csrf: csrf,
      submission_token: uniqueSubmissionToken(`${board}-${threadId}`),
      body,
    },
    maxRedirects: 0,
  });
  expect([302, 303]).toContain(response.status());
}

export async function unlockBoard(page: Page, app: RustChanServer, board: string, password: string): Promise<void> {
  await gotoAppPage(page, `${app.baseURL}/${board}/unlock`);
  await page.locator('input[name="password"]').fill(password);
  await Promise.all([
    page.waitForURL(new RegExp(`/${board}`)),
    page.getByRole('button', { name: /unlock board|unlock posting/i }).click(),
  ]);
  await expectSafePage(page);
}

export async function expectSafePage(page: Page, options: { allowAdminInternals?: boolean } = {}): Promise<void> {
  await expect(page.locator('body')).toBeVisible();
  const body = await page.locator('body').innerText();
  expect(body).not.toMatch(/thread panicked|stack backtrace|SQLITE_|database is locked/i);
  if (!options.allowAdminInternals) {
    expect(body).not.toMatch(/\/Users\/|rustchan-data|target\/debug/i);
  }
}

export async function expectSafeResponse(response: APIResponse): Promise<string> {
  const text = await response.text();
  expect(text).not.toMatch(/thread panicked|stack backtrace|SQLITE_|database is locked/i);
  return text;
}

export async function expectNoDialog(page: Page, action: () => Promise<void>): Promise<void> {
  let dialogSeen = false;
  page.once('dialog', async (dialog) => {
    dialogSeen = true;
    await dialog.dismiss();
  });
  await action();
  expect(dialogSeen).toBe(false);
}

export async function adminCsrf(page: Page, app: RustChanServer): Promise<string> {
  return extractCsrf(await adminPanelHtml(page, app));
}

export async function publicCsrf(page: Page, app: RustChanServer, pathPart = '/'): Promise<string> {
  const response = await page.request.get(`${app.baseURL}${pathPart}`);
  const html = await response.text();
  return extractCsrf(html);
}

export function threadIdFromUrl(url: string): number {
  const match = url.match(/\/thread\/(\d+)/);
  if (!match) throw new Error(`no thread id in ${url}`);
  return Number(match[1]);
}

export function uniqueShort(prefix: string, testInfo: TestInfo): string {
  const stem = prefix.toLowerCase().replace(/[^a-z0-9]/g, '').slice(0, 3) || 'b';
  const hash = createHash('sha1')
    .update([
      testInfo.project.name,
      testInfo.file,
      testInfo.title,
      testInfo.workerIndex,
      testInfo.repeatEachIndex,
      testInfo.retry,
      prefix,
    ].join('\0'))
    .digest('hex');
  return `${stem}${hash.slice(0, 8 - stem.length)}`;
}

function uniqueSubmissionToken(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export function extractCsrf(html: string): string {
  const match = html.match(/name="_csrf"\s+value="([^"]+)"/);
  if (!match) throw new Error('CSRF token not found');
  return decodeHtml(match[1]);
}

export async function createStandaloneApp(options: { admin?: boolean; boards?: SeedBoard[] } = {}): Promise<RustChanServer> {
  const app = await RustChanServer.create();
  try {
    if (options.admin) {
      app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
    }
    for (const board of options.boards ?? []) {
      app.createBoardCli(board);
    }
    await app.start();
    return app;
  } catch (error) {
    await app.dispose().catch(() => undefined);
    throw error;
  }
}

export function setBoardFixtureSettings(app: RustChanServer, short: string, settings: BoardFixtureSettings): void {
  const assignments: string[] = [];
  addSqlBool(assignments, 'nsfw', settings.nsfw);
  addSqlBool(assignments, 'allow_images', settings.allowImages);
  addSqlBool(assignments, 'allow_video', settings.allowVideo);
  addSqlBool(assignments, 'allow_audio', settings.allowAudio);
  addSqlBool(assignments, 'allow_pdf', settings.allowPdf);
  addSqlBool(assignments, 'allow_any_files', settings.allowAnyFiles);
  addSqlBool(assignments, 'allow_archive', settings.allowArchive);
  addSqlBool(assignments, 'allow_editing', settings.allowEditing);
  addSqlBool(assignments, 'allow_self_delete', settings.allowSelfDelete);
  addSqlBool(assignments, 'allow_captcha', settings.allowCaptcha);
  addSqlInt(assignments, 'bump_limit', settings.bumpLimit);
  addSqlInt(assignments, 'max_threads', settings.maxThreads);
  addSqlInt(assignments, 'max_archived_threads', settings.maxArchivedThreads);
  addSqlInt(assignments, 'post_cooldown_secs', settings.postCooldownSecs);
  addSqlInt(assignments, 'max_image_size', settings.maxImageSizeBytes ?? mibToBytes(settings.maxImageSizeMb));
  addSqlInt(assignments, 'max_video_size', settings.maxVideoSizeBytes ?? mibToBytes(settings.maxVideoSizeMb));
  addSqlInt(assignments, 'max_audio_size', settings.maxAudioSizeBytes ?? mibToBytes(settings.maxAudioSizeMb));
  addSqlInt(assignments, 'max_pdf_size', settings.maxPdfSizeBytes ?? mibToBytes(settings.maxPdfSizeMb));
  addSqlText(assignments, 'name', settings.name);
  addSqlText(assignments, 'description', settings.description);
  addSqlText(assignments, 'default_theme', settings.defaultTheme);
  if (settings.accessMode) {
    assignments.push(`access_mode = ${sqlLiteral(settings.accessMode)}`);
  }
  if (settings.accessPasswordHash) {
    assignments.push(`access_password_hash = ${sqlLiteral(settings.accessPasswordHash)}`);
  }
  if (settings.clearAccessPassword) {
    assignments.push("access_password_hash = ''");
  }
  if (assignments.length === 0) {
    return;
  }
  sqliteExec(app, `UPDATE boards SET ${assignments.join(', ')} WHERE short_name = ${sqlLiteral(short)};`);
}

export function setThreadFixtureState(
  app: RustChanServer,
  threadId: number,
  state: { locked?: boolean; archived?: boolean; createdAt?: number; bumpedAt?: number },
): void {
  const assignments: string[] = [];
  addSqlBool(assignments, 'locked', state.locked);
  addSqlBool(assignments, 'archived', state.archived);
  addSqlInt(assignments, 'created_at', state.createdAt);
  addSqlInt(assignments, 'bumped_at', state.bumpedAt);
  if (assignments.length === 0) {
    return;
  }
  sqliteExec(app, `UPDATE threads SET ${assignments.join(', ')} WHERE id = ${threadId};`);
}

export function setPostFixtureCreatedAt(app: RustChanServer, postId: number, createdAt: number): void {
  sqliteExec(app, `UPDATE posts SET created_at = ${createdAt} WHERE id = ${postId};`);
}

export function setSiteFixtureSettings(app: RustChanServer, settings: SiteFixtureSettings): void {
  const entries: [string, string | undefined][] = [
    ['site_name', settings.siteName],
    ['site_subtitle', settings.siteSubtitle],
    ['default_theme', settings.defaultTheme],
    ['homepage_new_thread_badges_enabled', boolText(settings.homepageNewThreadBadgesEnabled)],
    ['homepage_new_reply_badges_enabled', boolText(settings.homepageNewReplyBadgesEnabled)],
    ['thread_new_reply_badges_enabled', boolText(settings.threadNewReplyBadgesEnabled)],
  ];
  for (const [key, value] of entries) {
    if (value === undefined) {
      continue;
    }
    sqliteExec(app, [
      'INSERT INTO site_settings (key, value)',
      `VALUES (${sqlLiteral(key)}, ${sqlLiteral(value)})`,
      'ON CONFLICT(key) DO UPDATE SET value = excluded.value;',
    ].join(' '));
  }
}

export function adminPasswordHash(app: RustChanServer): string {
  const hash = sqliteQuery(app, "SELECT password_hash FROM admin_users WHERE username = 'admin' LIMIT 1;");
  if (!hash) {
    throw new Error('admin password hash not found');
  }
  return hash.trim();
}

export function boardId(app: RustChanServer, short: string): number {
  const id = Number(sqliteQuery(app, `SELECT id FROM boards WHERE short_name = ${sqlLiteral(short)} LIMIT 1;`));
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error(`board /${short}/ not found`);
  }
  return id;
}

export function sqliteExec(app: RustChanServer, sql: string): void {
  const result = spawnSync('sqlite3', [app.dbPath(), `PRAGMA busy_timeout=5000; ${sql}`], {
    encoding: 'utf8',
  });
  if (result.status !== 0) {
    throw new Error(`sqlite fixture update failed: ${result.stderr || result.stdout}`);
  }
}

export function sqliteQuery(app: RustChanServer, sql: string): string {
  const result = spawnSync('sqlite3', ['-batch', '-noheader', app.dbPath(), sql], {
    encoding: 'utf8',
  });
  if (result.status !== 0) {
    throw new Error(`sqlite fixture query failed: ${result.stderr || result.stdout}`);
  }
  return result.stdout.trim();
}

async function adminPanelHtml(page: Page, app: RustChanServer): Promise<string> {
  const response = await page.request.get(`${app.baseURL}/admin/panel`);
  expect(response.status()).toBe(200);
  return response.text();
}

function extractBoardId(html: string, short: string): number {
  const escaped = short.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const details = new RegExp(`<details[^>]+id="board-${escaped}"[\\s\\S]*?<input type="hidden" name="board_id" value="(\\d+)"`);
  const match = html.match(details);
  if (!match) throw new Error(`board id for /${short}/ not found`);
  return Number(match[1]);
}

function setCheck(form: URLSearchParams, name: string, checked: boolean): void {
  if (checked) form.set(name, '1');
}

function addSqlBool(assignments: string[], column: string, value: boolean | undefined): void {
  if (value !== undefined) {
    assignments.push(`${column} = ${value ? 1 : 0}`);
  }
}

function addSqlInt(assignments: string[], column: string, value: number | undefined): void {
  if (value !== undefined) {
    assignments.push(`${column} = ${Math.trunc(value)}`);
  }
}

function addSqlText(assignments: string[], column: string, value: string | undefined): void {
  if (value !== undefined) {
    assignments.push(`${column} = ${sqlLiteral(value)}`);
  }
}

function boolText(value: boolean | undefined): string | undefined {
  if (value === undefined) {
    return undefined;
  }
  return value ? 'true' : 'false';
}

function mibToBytes(value: number | undefined): number | undefined {
  if (value === undefined) {
    return undefined;
  }
  return Math.trunc(value * 1024 * 1024);
}

function sqlLiteral(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}

function displayNameFor(short: string): string {
  return `${short.toUpperCase()} Board`;
}

function decodeHtml(value: string): string {
  return value
    .replace(/&quot;/g, '"')
    .replace(/&#x27;/g, "'")
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>');
}

function childHasExited(child: ChildProcess): boolean {
  return child.exitCode !== null || child.signalCode !== null;
}

async function waitForChildExit(
  child: ChildProcess,
  timeoutMs: number,
): Promise<boolean> {
  if (childHasExited(child)) {
    return true;
  }
  return new Promise<boolean>((resolve) => {
    let settled = false;
    const finish = (exited: boolean) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      child.removeListener('exit', onExit);
      resolve(exited);
    };
    const onExit = () => finish(true);
    const timer = setTimeout(() => finish(childHasExited(child)), timeoutMs);
    child.once('exit', onExit);
    if (childHasExited(child)) {
      finish(true);
    }
  });
}

async function removeFixtureRoot(rootDir: string, preserveRoot: boolean): Promise<void> {
  if (preserveRoot) {
    console.warn(`preserving RustChan E2E fixture root: ${rootDir}`);
    return;
  }
  await fsp.rm(rootDir, {
    recursive: true,
    force: true,
    maxRetries: 3,
    retryDelay: 100,
  });
}

async function reserveFreePort(): Promise<{ port: number; server: net.Server }> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      if (!address || typeof address === 'string') {
        server.close(() => reject(new Error('failed to allocate port')));
        return;
      }
      resolve({ port: address.port, server });
    });
  });
}

async function waitForReady(baseURL: string, child: ChildProcess): Promise<void> {
  const deadline = Date.now() + 60_000;
  let lastError: unknown;
  while (Date.now() < deadline) {
    if (childHasExited(child)) {
      throw new Error(`rustchan exited before ready with code ${child.exitCode} signal ${child.signalCode}`);
    }
    try {
      const response = await fetch(`${baseURL}/readyz`, { signal: AbortSignal.timeout(1_000) });
      if (response.ok) return;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  throw new Error(`rustchan did not become ready: ${String(lastError)}`);
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

function runFixtureTool(command: string, args: string[]): void {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error([
      `failed to create media fixture with ${command}`,
      result.stdout,
      result.stderr,
    ].filter(Boolean).join('\n'));
  }
}
