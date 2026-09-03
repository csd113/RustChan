import { test as base, expect, type APIResponse, type Page, type TestInfo, type WorkerInfo } from '@playwright/test';
import crypto from 'node:crypto';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import * as zlib from 'node:zlib';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  RustChanServer,
  extractCsrf,
  setBoardFixtureSettings,
} from './helpers';

const MIB = 1024 * 1024;
const SUPPORTED_PROJECTS = new Set(['chromium', 'firefox-nojs', 'mobile-webkit']);
const SUMMARY_DIR = path.resolve(__dirname, '../../test-results/e2e/upload-validation');

type RuntimeMode = 'local' | 'external';
type Outcome = 'pass' | 'fail' | 'skip';
type MediaKind = 'image' | 'video' | 'audio' | 'pdf' | 'other';
type ExpectedOutcome = 'accept' | 'reject';

type UploadRuntime = {
  mode: RuntimeMode;
  baseURL: string;
  fixtureDir: string;
  app?: RustChanServer;
  adminUsername: string;
  adminPassword: string;
};

type FixtureFile = {
  key: string;
  path: string;
  name: string;
  mimeType: string;
  size: number;
  sha256: string;
  mediaKind: MediaKind;
};

type BoardSet = {
  media: string;
  text: string;
  noImages: string;
  noVideo: string;
  noAudio: string;
  noPdf: string;
  any: string;
};

type UploadResult = {
  status?: number;
  responseUrl?: string;
  location?: string;
  visibleErrorText?: string;
  classification: string;
  requestHeaders?: Record<string, string>;
  responseHeaders?: Record<string, string>;
  mediaHref?: string;
  downloadedBytes?: number;
  downloadedSha256?: string;
  expectedSha256?: string;
};

type ScenarioResult = {
  id: string;
  title: string;
  browserProject: string;
  baseURLMode: RuntimeMode;
  outcome: Outcome;
  reason: string;
  expected: ExpectedOutcome | 'diagnostic';
  flow: string;
  file?: {
    name: string;
    type: string;
    size: number;
    sha256: string;
  };
  board?: {
    short: string;
    settings: Record<string, unknown>;
  };
  upload?: UploadResult;
  diagnostics: ReturnType<DiagnosticsCollector['collect']>;
  screenshot?: string;
  durationMs: number;
};

type Scenario = {
  id: string;
  title: string;
  expected: ExpectedOutcome | 'diagnostic';
  flow: string;
  fixture?: FixtureFile;
  board?: string;
  boardSettings?: Record<string, unknown>;
  keyScreenshot?: boolean;
  skip?: (ctx: ScenarioContext) => string | undefined;
  run: (ctx: ScenarioContext) => Promise<UploadResult | undefined>;
};

type ScenarioContext = {
  page: Page;
  runtime: UploadRuntime;
  testInfo: TestInfo;
  diagnostics: DiagnosticsCollector;
  fixtures: Record<string, FixtureFile>;
  boards: BoardSet;
  jsEnabled: boolean;
};

class DiagnosticsCollector {
  private readonly consoleMessages: string[] = [];
  private readonly pageErrors: string[] = [];
  private readonly failedRequests: Array<Record<string, string | undefined>> = [];
  private readonly uploadRequests: Array<Record<string, string | number | undefined>> = [];

  constructor(page: Page) {
    page.on('console', (msg) => {
      if (['error', 'warning'].includes(msg.type())) {
        this.consoleMessages.push(`${msg.type()}: ${msg.text()}`);
      }
    });
    page.on('pageerror', (error) => {
      this.pageErrors.push(error.message);
    });
    page.on('requestfailed', (request) => {
      this.failedRequests.push({
        method: request.method(),
        url: request.url(),
        failure: request.failure()?.errorText,
      });
    });
    page.on('request', (request) => {
      if (request.method() !== 'POST') return;
      const headers = request.headers();
      this.uploadRequests.push({
        method: request.method(),
        url: request.url(),
        origin: headers.origin,
        referer: headers.referer,
        host: headers.host,
        forwardedProto: headers['x-forwarded-proto'],
        forwardedHost: headers['x-forwarded-host'],
        contentType: headers['content-type'],
        contentLength: headers['content-length'],
        postDataBytes: request.postDataBuffer()?.length,
      });
    });
  }

  mark() {
    return {
      console: this.consoleMessages.length,
      pageErrors: this.pageErrors.length,
      failedRequests: this.failedRequests.length,
      uploadRequests: this.uploadRequests.length,
    };
  }

  collect(mark: ReturnType<DiagnosticsCollector['mark']>) {
    return {
      console: this.consoleMessages.slice(mark.console),
      pageErrors: this.pageErrors.slice(mark.pageErrors),
      failedRequests: this.failedRequests.slice(mark.failedRequests),
      uploadRequests: this.uploadRequests.slice(mark.uploadRequests),
    };
  }
}

const test = base.extend<{ runtime: UploadRuntime }>({
  runtime: async ({}, use, workerInfo) => {
    const externalBase = process.env.RUSTCHAN_UPLOAD_BASE_URL?.replace(/\/+$/, '');
    if (externalBase) {
      const fixtureDir = await fsp.mkdtemp(path.join(os.tmpdir(), 'rustchan-upload-validation-external-'));
      const runtime: UploadRuntime = {
        mode: 'external',
        baseURL: externalBase,
        fixtureDir,
        adminUsername: process.env.RUSTCHAN_UPLOAD_ADMIN_USERNAME ?? ADMIN_USERNAME,
        adminPassword: process.env.RUSTCHAN_UPLOAD_ADMIN_PASSWORD ?? ADMIN_PASSWORD,
      };
      try {
        await use(runtime);
      } finally {
        await fsp.rm(fixtureDir, { recursive: true, force: true });
      }
      return;
    }

    const app = await RustChanServer.create(workerInfo as WorkerInfo, {
      env: {
        CHAN_ENABLE_ANY_FILE_UPLOADS_FEATURE: '1',
        CHAN_PUBLIC_HOSTS: 'localhost,127.0.0.1,::1',
      },
    });
    try {
      await app.initializeDefaultData();
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

test.beforeEach(async ({}, testInfo) => {
  test.skip(!SUPPORTED_PROJECTS.has(testInfo.project.name), 'upload validation runs on chromium, firefox-nojs, and mobile-webkit');
});

test('RustChan media upload validation matrix', async ({ page, runtime }, testInfo) => {
  test.setTimeout(240_000);

  const diagnostics = new DiagnosticsCollector(page);
  const fixtures = await createUploadFixtures(runtime.fixtureDir);
  const summary: {
    generatedAt: string;
    browserProject: string;
    baseURLMode: RuntimeMode;
    baseURL: string;
    scenarios: ScenarioResult[];
    counts?: Record<Outcome, number>;
  } = {
    generatedAt: new Date().toISOString(),
    browserProject: testInfo.project.name,
    baseURLMode: runtime.mode,
    baseURL: runtime.baseURL,
    scenarios: [],
  };

  let boards: BoardSet | undefined;
  try {
    boards = await setupValidationBoards(page, runtime, testInfo);
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    for (const scenario of placeholderScenarios(testInfo.project.name)) {
      summary.scenarios.push({
        id: scenario.id,
        title: scenario.title,
        browserProject: testInfo.project.name,
        baseURLMode: runtime.mode,
        outcome: 'skip',
        reason: `board setup failed: ${reason}`,
        expected: scenario.expected,
        flow: scenario.flow,
        diagnostics: { console: [], pageErrors: [], failedRequests: [], uploadRequests: [] },
        durationMs: 0,
      });
    }
    await writeSummary(summary, testInfo);
    throw new Error(`upload validation board setup failed; summary written to ${summaryPath(testInfo)}`);
  }

  const ctx: ScenarioContext = {
    page,
    runtime,
    testInfo,
    diagnostics,
    fixtures,
    boards,
    jsEnabled: testInfo.project.name !== 'firefox-nojs',
  };

  for (const scenario of buildScenarios(ctx)) {
    const started = Date.now();
    const mark = diagnostics.mark();
    let result: UploadResult | undefined;
    let outcome: Outcome = 'pass';
    let reason = 'ok';
    let screenshot: string | undefined;

    const skipReason = scenario.skip?.(ctx);
    if (skipReason) {
      outcome = 'skip';
      reason = skipReason;
    } else {
      try {
        result = await scenario.run(ctx);
      } catch (error) {
        outcome = 'fail';
        reason = error instanceof Error ? error.message : String(error);
      }
    }

    if (outcome === 'fail' || scenario.keyScreenshot) {
      screenshot = await captureScenarioScreenshot(page, testInfo, scenario.id).catch(() => undefined);
    }

    summary.scenarios.push({
      id: scenario.id,
      title: scenario.title,
      browserProject: testInfo.project.name,
      baseURLMode: runtime.mode,
      outcome,
      reason,
      expected: scenario.expected,
      flow: scenario.flow,
      file: scenario.fixture
        ? {
            name: scenario.fixture.name,
            type: scenario.fixture.mimeType,
            size: scenario.fixture.size,
            sha256: scenario.fixture.sha256,
          }
        : undefined,
      board: scenario.board
        ? {
            short: scenario.board,
            settings: scenario.boardSettings ?? {},
          }
        : undefined,
      upload: result,
      diagnostics: diagnostics.collect(mark),
      screenshot,
      durationMs: Date.now() - started,
    });
  }

  await writeSummary(summary, testInfo);
  const failed = summary.scenarios.filter((scenario) => scenario.outcome === 'fail');
  if (failed.length > 0) {
    throw new Error(
      `${failed.length} upload validation scenario(s) failed. JSON summary: ${summaryPath(testInfo)}`,
    );
  }
});

function buildScenarios(ctx: ScenarioContext): Scenario[] {
  const f = ctx.fixtures;
  const b = ctx.boards;
  const mediaBoardSettings = {
    allowImages: true,
    allowVideo: true,
    allowAudio: true,
    allowPdf: true,
    allowAnyFiles: false,
    maxImageSizeMb: 1,
    maxVideoSizeMb: 2,
    maxAudioSizeMb: 1,
    maxPdfSizeMb: 2,
  };
  const scenarios: Scenario[] = [
    acceptScenario('ui-new-thread-png', 'new thread upload through the visible form', 'new-thread-ui', b.media, f.png, mediaBoardSettings, async (inner) => {
      return submitUiThread(inner, b.media, f.png, true);
    }),
    acceptScenario('ui-reply-png', 'reply upload through the visible form', 'reply-ui', b.media, f.png, mediaBoardSettings, async (inner) => {
      const threadId = await createTextThread(inner.page, inner.runtime, b.media, 'reply upload parent');
      return submitUiReply(inner, b.media, threadId, f.png, true);
    }),
    rejectScenario('default-any-disabled-rejects-generic', 'default board policy rejects generic files', 'request-new-thread', b.media, f.genericText, mediaBoardSettings),
    rejectScenario('image-disabled-rejects-png', 'image-disabled board rejects PNG', 'request-new-thread', b.noImages, f.png, { allowImages: false, allowVideo: true, allowAudio: true, allowPdf: true }),
    rejectScenario('video-disabled-rejects-mp4', 'video-disabled board rejects MP4', 'request-new-thread', b.noVideo, f.mp4, { allowImages: true, allowVideo: false, allowAudio: true, allowPdf: true }),
    rejectScenario('audio-disabled-rejects-ogg', 'audio-disabled board rejects OGG', 'request-new-thread', b.noAudio, f.ogg, { allowImages: true, allowVideo: true, allowAudio: false, allowPdf: true }),
    rejectScenario('pdf-disabled-rejects-pdf', 'PDF-disabled board rejects PDF', 'request-new-thread', b.noPdf, f.pdf, { allowImages: true, allowVideo: true, allowAudio: true, allowPdf: false }),
    acceptScenario('any-enabled-accepts-generic', 'global and per-board any-file upload accepts generic download', 'request-new-thread', b.any, f.genericText, { allowAnyFiles: true }, async (inner) => {
      return expectAcceptedRequest(inner, b.any, f.genericText, { compareBytes: true });
    }),
    diagnosticScenario('proxy-like-forwarded-headers', 'upload succeeds with tunnel-like forwarded headers', 'request-new-thread', b.media, f.png, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.png, {
        headers: tunnelLikeHeaders(inner.runtime.baseURL),
        compareBytes: inner.runtime.mode === 'local',
      });
    }),
    diagnosticScenario('csrf-mismatch-classification', 'bad CSRF is classified separately from upload validation', 'request-new-thread', b.media, f.png, mediaBoardSettings, async (inner) => {
      const result = await submitRequestUpload(inner.page, inner.runtime, {
        board: b.media,
        fixture: f.png,
        csrfOverride: 'definitely-not-the-cookie-token',
      });
      assertRejected(result, /csrf|forbidden/i);
      return result;
    }),
    diagnosticScenario('global-any-feature-disabled', 'global any-file feature off overrides per-board allow_any_files', 'request-new-thread', b.any, f.genericText, { allowAnyFiles: true }, async (inner) => {
      return validateGlobalAnyFileFeatureDisabled(inner, f.genericText);
    }, (inner) => {
      if (inner.runtime.mode !== 'local') return 'requires local isolated server with feature flag disabled';
      if (inner.testInfo.project.name !== 'chromium') return 'covered once in chromium to avoid extra server startup per browser';
      return undefined;
    }),
  ];

  for (const key of ['png', 'jpg', 'gif', 'webp', 'bmp', 'tiff', 'heic', 'heif', 'mp4', 'webm', 'mp3', 'ogg', 'flac', 'wav', 'm4a', 'aac', 'pdf'] as const) {
    scenarios.push(
      acceptScenario(`allowed-${key}`, `allowed ${key.toUpperCase()} upload is accepted`, 'request-new-thread', b.media, f[key], mediaBoardSettings, async (inner) => {
        return expectAcceptedRequest(inner, b.media, f[key], {
          compareBytes: inner.runtime.mode === 'local' && ['png', 'mp4', 'webm', 'mp3', 'ogg', 'flac', 'wav', 'm4a', 'aac', 'pdf'].includes(key),
        });
      }),
    );
  }

  scenarios.push(
    rejectScenario('misleading-extension-rejected', 'plain text renamed as PNG is rejected', 'request-new-thread', b.media, f.textRenamedPng, mediaBoardSettings),
    acceptScenario('mime-extension-mismatch-accepted-by-sniffing', 'PNG bytes named .jpg are accepted by sniffing', 'request-new-thread', b.media, f.pngNamedJpg, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.pngNamedJpg, { compareBytes: inner.runtime.mode === 'local' });
    }),
    rejectScenario('zero-byte-named-file-rejected', 'zero-byte named file is rejected clearly', 'request-new-thread', b.media, f.zero, mediaBoardSettings),
    rejectScenario('truncated-png-rejected', 'truncated PNG is rejected', 'request-new-thread', b.media, f.truncatedPng, mediaBoardSettings),
    rejectScenario('svg-rejected', 'SVG uploads are rejected as active image content', 'request-new-thread', b.media, f.svg, mediaBoardSettings),
    rejectScenario('mkv-rejected', 'MKV containers are rejected when only supported browser video types are enabled', 'request-new-thread', b.media, f.mkv, mediaBoardSettings),
    rejectScenario('malformed-pdf-rejected', 'PDF missing trailer is rejected', 'request-new-thread', b.media, f.malformedPdf, mediaBoardSettings),
    rejectScenario('malformed-aac-rejected', 'AAC with only a plausible prefix is rejected before waveform jobs', 'request-new-thread', b.media, f.malformedAac, mediaBoardSettings),
    acceptScenario('duplicate-upload-first', 'first duplicate PNG upload succeeds', 'request-new-thread', b.media, f.duplicatePng, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.duplicatePng, { compareBytes: inner.runtime.mode === 'local' });
    }),
    acceptScenario('duplicate-upload-second', 'second duplicate PNG upload succeeds via dedup/cache path', 'request-new-thread', b.media, f.duplicatePng, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.duplicatePng, { compareBytes: inner.runtime.mode === 'local' });
    }),
  );

  for (const key of ['trickySpaces', 'trickyShell', 'trickyBackslash', 'trickyTraversal', 'trickyLong'] as const) {
    scenarios.push(
      acceptScenario(`filename-${key}`, `tricky filename ${key} is accepted without unsafe paths`, 'request-new-thread', b.media, f[key], mediaBoardSettings, async (inner) => {
        const result = await expectAcceptedRequest(inner, b.media, f[key], { compareBytes: inner.runtime.mode === 'local' });
        expect(result.mediaHref).toMatch(new RegExp(`^/boards/${b.media}/[^?]+`));
        expect(result.mediaHref).not.toContain('..');
        expect(result.mediaHref).not.toContain('\\');
        return result;
      }),
    );
  }

  scenarios.push(
    acceptScenario('image-size-under-cap', 'image just under cap is accepted', 'request-new-thread', b.media, f.pngUnderImageCap, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.pngUnderImageCap, { compareBytes: inner.runtime.mode === 'local' });
    }, true),
    acceptScenario('image-size-exact-cap', 'image exactly at cap is accepted', 'request-new-thread', b.media, f.pngExactImageCap, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.pngExactImageCap, { compareBytes: inner.runtime.mode === 'local' });
    }, true),
    rejectScenario('image-size-over-cap', 'image just over cap is rejected', 'request-new-thread', b.media, f.pngOverImageCap, mediaBoardSettings, true),
    rejectScenario('image-size-clear-over-cap', 'image clearly over cap is rejected', 'request-new-thread', b.media, f.pngClearOverImageCap, mediaBoardSettings),
    acceptScenario('video-size-exact-cap', 'video exactly at cap is accepted', 'request-new-thread', b.media, f.mp4ExactVideoCap, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.mp4ExactVideoCap, { compareBytes: inner.runtime.mode === 'local' });
    }, true),
    rejectScenario('video-size-over-cap', 'video just over cap is rejected', 'request-new-thread', b.media, f.mp4OverVideoCap, mediaBoardSettings, true),
    acceptScenario('audio-size-exact-cap', 'audio exactly at cap is accepted', 'request-new-thread', b.media, f.oggExactAudioCap, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.oggExactAudioCap, { compareBytes: inner.runtime.mode === 'local' });
    }, true),
    rejectScenario('audio-size-over-cap', 'audio just over cap is rejected', 'request-new-thread', b.media, f.oggOverAudioCap, mediaBoardSettings, true),
    acceptScenario('pdf-size-exact-generic-cap', 'PDF exactly at generic cap is accepted', 'request-new-thread', b.media, f.pdfExactGenericCap, mediaBoardSettings, async (inner) => {
      return expectAcceptedRequest(inner, b.media, f.pdfExactGenericCap, { compareBytes: inner.runtime.mode === 'local' });
    }, true),
    rejectScenario('pdf-size-over-generic-cap', 'PDF just over generic cap is rejected', 'request-new-thread', b.media, f.pdfOverGenericCap, mediaBoardSettings, true),
    acceptScenario('any-size-exact-generic-cap', 'any-file exactly at generic cap is accepted', 'request-new-thread', b.any, f.genericExactCap, { allowAnyFiles: true }, async (inner) => {
      return expectAcceptedRequest(inner, b.any, f.genericExactCap, { compareBytes: true });
    }, true),
    rejectScenario('any-size-over-generic-cap', 'any-file just over generic cap is rejected', 'request-new-thread', b.any, f.genericOverCap, { allowAnyFiles: true }, true),
  );

  return scenarios;
}

function acceptScenario(
  id: string,
  title: string,
  flow: string,
  board: string,
  fixture: FixtureFile,
  boardSettings: Record<string, unknown>,
  run: (ctx: ScenarioContext) => Promise<UploadResult>,
  keyScreenshot = false,
): Scenario {
  return {
    id,
    title,
    expected: 'accept',
    flow,
    board,
    fixture,
    boardSettings,
    keyScreenshot,
    run,
  };
}

function rejectScenario(
  id: string,
  title: string,
  flow: string,
  board: string,
  fixture: FixtureFile,
  boardSettings: Record<string, unknown>,
  keyScreenshot = false,
): Scenario {
  return {
    id,
    title,
    expected: 'reject',
    flow,
    board,
    fixture,
    boardSettings,
    keyScreenshot,
    run: async (ctx) => {
      const result = await submitRequestUpload(ctx.page, ctx.runtime, { board, fixture });
      assertRejected(result, /disabled|not allowed|accepted|too large|maximum|empty|malformed|incomplete|trailer|only accepts/i);
      await assertNoLocalUploadTempLeaks(ctx.runtime, board);
      return result;
    },
  };
}

function diagnosticScenario(
  id: string,
  title: string,
  flow: string,
  board: string,
  fixture: FixtureFile,
  boardSettings: Record<string, unknown>,
  run: (ctx: ScenarioContext) => Promise<UploadResult>,
  skip?: (ctx: ScenarioContext) => string | undefined,
): Scenario {
  return {
    id,
    title,
    expected: 'diagnostic',
    flow,
    board,
    fixture,
    boardSettings,
    run,
    skip,
  };
}

function placeholderScenarios(projectName: string): Pick<Scenario, 'id' | 'title' | 'expected' | 'flow'>[] {
  return [
    { id: 'setup', title: `board setup for ${projectName}`, expected: 'diagnostic', flow: 'admin-setup' },
  ];
}

async function setupValidationBoards(page: Page, runtime: UploadRuntime, testInfo: TestInfo): Promise<BoardSet> {
  await adminLogin(page, runtime);
  const seed = crypto
    .createHash('sha256')
    .update(`${testInfo.project.name}-${testInfo.workerIndex}-${Date.now()}-${Math.random()}`)
    .digest('hex')
    .slice(0, 4);
  const boards: BoardSet = {
    media: `m${seed}`,
    text: `t${seed}`,
    noImages: `i${seed}`,
    noVideo: `v${seed}`,
    noAudio: `a${seed}`,
    noPdf: `p${seed}`,
    any: `n${seed}`,
  };

  for (const [short, name] of [
    [boards.media, 'Upload Validation Media'],
    [boards.text, 'Upload Validation Text'],
    [boards.noImages, 'Upload Validation No Images'],
    [boards.noVideo, 'Upload Validation No Video'],
    [boards.noAudio, 'Upload Validation No Audio'],
    [boards.noPdf, 'Upload Validation No PDF'],
    [boards.any, 'Upload Validation Any'],
  ] as const) {
    await createBoard(page, runtime, short, name);
  }

  await updateBoard(page, runtime, boards.media, {
    allowImages: true,
    allowVideo: true,
    allowAudio: true,
    allowPdf: true,
    allowAnyFiles: false,
    maxImageSizeMb: 1,
    maxVideoSizeMb: 2,
    maxAudioSizeMb: 1,
    maxPdfSizeMb: 2,
  });
  await updateBoard(page, runtime, boards.text, {
    allowImages: false,
    allowVideo: false,
    allowAudio: false,
    allowPdf: false,
    allowAnyFiles: false,
  });
  await updateBoard(page, runtime, boards.noImages, {
    allowImages: false,
    allowVideo: true,
    allowAudio: true,
    allowPdf: true,
  });
  await updateBoard(page, runtime, boards.noVideo, {
    allowImages: true,
    allowVideo: false,
    allowAudio: true,
    allowPdf: true,
  });
  await updateBoard(page, runtime, boards.noAudio, {
    allowImages: true,
    allowVideo: true,
    allowAudio: false,
    allowPdf: true,
  });
  await updateBoard(page, runtime, boards.noPdf, {
    allowImages: true,
    allowVideo: true,
    allowAudio: true,
    allowPdf: false,
  });
  await updateBoard(page, runtime, boards.any, {
    allowImages: false,
    allowVideo: false,
    allowAudio: false,
    allowPdf: false,
    allowAnyFiles: true,
    maxImageSizeMb: 1,
    maxVideoSizeMb: 2,
    maxAudioSizeMb: 1,
  });

  return boards;
}

async function adminLogin(page: Page, runtime: UploadRuntime): Promise<void> {
  await page.goto(`${runtime.baseURL}/admin`);
  if (page.url().includes('/admin/panel')) return;
  await page.getByLabel('Username').fill(runtime.adminUsername);
  await page.getByLabel('Password').fill(runtime.adminPassword);
  await Promise.all([
    page.waitForURL(/\/admin\/panel/),
    page.getByRole('button', { name: 'authenticate' }).click(),
  ]);
}

async function createBoard(page: Page, runtime: UploadRuntime, short: string, name: string): Promise<void> {
  const csrf = await adminCsrf(page, runtime);
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
  runtime: UploadRuntime,
  short: string,
  settings: {
    allowImages?: boolean;
    allowVideo?: boolean;
    allowAudio?: boolean;
    allowPdf?: boolean;
    allowAnyFiles?: boolean;
    maxImageSizeMb?: number;
    maxVideoSizeMb?: number;
    maxAudioSizeMb?: number;
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
    description: `${short} upload validation`,
    bump_limit: '300',
    max_threads: '150',
    max_archived_threads: '150',
    post_cooldown_secs: '0',
    max_image_size_mb: String(settings.maxImageSizeMb ?? 1),
    max_video_size_mb: String(settings.maxVideoSizeMb ?? 2),
    max_audio_size_mb: String(settings.maxAudioSizeMb ?? 1),
    max_pdf_size_mb: String(settings.maxPdfSizeMb ?? 1),
    default_theme: '',
    banner_mode: 'inherit',
    access_mode: 'public',
    access_password: '',
    allow_tripcodes: '1',
    allow_video_embeds: '1',
  };
  setCheckbox(form, 'allow_images', settings.allowImages ?? true);
  setCheckbox(form, 'allow_video', settings.allowVideo ?? true);
  setCheckbox(form, 'allow_audio', settings.allowAudio ?? false);
  setCheckbox(form, 'allow_pdf', settings.allowPdf ?? false);
  setCheckbox(form, 'allow_any_files', settings.allowAnyFiles ?? false);

  const response = await page.request.post(`${runtime.baseURL}/admin/board/settings`, {
    form,
    maxRedirects: 0,
  });
  if (response.status() !== 303) {
    throw new Error(`update board /${short}/ failed with ${response.status()}: ${await response.text()}`);
  }
}

async function adminCsrf(page: Page, runtime: UploadRuntime): Promise<string> {
  return extractCsrf(await adminPanelHtml(page, runtime));
}

async function adminPanelHtml(page: Page, runtime: UploadRuntime): Promise<string> {
  const response = await page.request.get(`${runtime.baseURL}/admin/panel`);
  if (response.status() !== 200) {
    throw new Error(`admin panel returned ${response.status()}`);
  }
  return response.text();
}

function extractBoardId(html: string, short: string): number {
  const escaped = short.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const details = new RegExp(`<details[^>]+id="board-${escaped}"[\\s\\S]*?<input type="hidden" name="board_id" value="(\\d+)"`);
  const match = html.match(details);
  if (!match) throw new Error(`board id for /${short}/ not found`);
  return Number(match[1]);
}

function setCheckbox(form: Record<string, string>, name: string, checked: boolean): void {
  if (checked) form[name] = '1';
}

async function createTextThread(page: Page, runtime: UploadRuntime, board: string, body: string): Promise<number> {
  const csrf = await publicCsrf(page, runtime, `/${board}`);
  const response = await page.request.post(`${runtime.baseURL}/${board}`, {
    multipart: {
      _csrf: csrf,
      submission_token: `text-${Date.now()}-${Math.random()}`,
      subject: 'upload validation parent',
      body,
    },
    maxRedirects: 0,
  });
  if (![302, 303].includes(response.status())) {
    throw new Error(`create text thread failed with ${response.status()}: ${await response.text()}`);
  }
  const location = response.headers().location ?? '';
  const id = Number(location.match(/\/thread\/(\d+)/)?.[1]);
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error(`thread id missing from redirect location ${location}`);
  }
  return id;
}

async function publicCsrf(page: Page, runtime: UploadRuntime, pathPart: string): Promise<string> {
  const response = await page.request.get(`${runtime.baseURL}${pathPart}`);
  if (response.status() !== 200) {
    throw new Error(`GET ${pathPart} for CSRF returned ${response.status()}`);
  }
  return extractPostFormCsrf(await response.text(), pathPart);
}

function extractPostFormCsrf(html: string, actionPath: string): string {
  const escapedAction = actionPath.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const form = html.match(new RegExp(`<form[^>]+action="${escapedAction}"[\\s\\S]*?</form>`));
  if (!form) {
    throw new Error(`post form with action ${actionPath} not found`);
  }
  return extractCsrf(form[0]);
}

async function submitUiThread(ctx: ScenarioContext, board: string, fixture: FixtureFile, expand: boolean): Promise<UploadResult> {
  await ctx.page.goto(`${ctx.runtime.baseURL}/${board}`);
  await revealPostForm(ctx.page);
  const form = ctx.page.locator(`form[action="/${board}"]`).first();
  await form.locator('input[name="subject"]').fill(`ui upload ${Date.now()}`);
  await form.locator('textarea[name="body"]').fill('ui upload validation');
  await form.locator('input[type="file"]').first().setInputFiles(fixture.path);
  const [response] = await Promise.all([
    ctx.page.waitForResponse((candidate) => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${board}`),
    form.getByRole('button', { name: /post thread/i }).click(),
  ]);
  await ctx.page.waitForURL(new RegExp(`/${board}/thread/\\d+`));
  return verifyRenderedUpload(ctx, responseToUploadResult(response), fixture, { expand, compareBytes: ctx.runtime.mode === 'local' });
}

async function submitUiReply(
  ctx: ScenarioContext,
  board: string,
  threadId: number,
  fixture: FixtureFile,
  expand: boolean,
): Promise<UploadResult> {
  await ctx.page.goto(`${ctx.runtime.baseURL}/${board}/thread/${threadId}`);
  await revealPostForm(ctx.page);
  const form = ctx.page.locator(`form[action="/${board}/thread/${threadId}"]`).first();
  await form.locator('textarea[name="body"]').fill('ui reply upload validation');
  await form.locator('input[type="file"]').first().setInputFiles(fixture.path);
  const [response] = await Promise.all([
    ctx.page.waitForResponse((candidate) => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === `/${board}/thread/${threadId}`),
    form.getByRole('button', { name: /post reply/i }).click(),
  ]);
  await ctx.page.waitForURL(new RegExp(`/${board}/thread/${threadId}`));
  return verifyRenderedUpload(ctx, responseToUploadResult(response), fixture, { expand, compareBytes: ctx.runtime.mode === 'local' });
}

async function revealPostForm(page: Page): Promise<void> {
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"], [data-action="toggle-post-form"]').first();
  if (await toggle.isVisible().catch(() => false)) {
    await toggle.click();
  }
}

async function expectAcceptedRequest(
  ctx: ScenarioContext,
  board: string,
  fixture: FixtureFile,
  options: { headers?: Record<string, string>; compareBytes?: boolean } = {},
): Promise<UploadResult> {
  const result = await submitRequestUpload(ctx.page, ctx.runtime, {
    board,
    fixture,
    headers: options.headers,
  });
  assertAccepted(result);
  await ctx.page.goto(`${ctx.runtime.baseURL}${result.location}`);
  return verifyRenderedUpload(ctx, result, fixture, {
    compareBytes: options.compareBytes ?? false,
    expand: fixture.mediaKind !== 'other',
  });
}

async function submitRequestUpload(
  page: Page,
  runtime: UploadRuntime,
  options: {
    board: string;
    fixture: FixtureFile;
    threadId?: number;
    csrfOverride?: string;
    headers?: Record<string, string>;
  },
): Promise<UploadResult> {
  const pathPart = options.threadId ? `/${options.board}/thread/${options.threadId}` : `/${options.board}`;
  const csrf = options.csrfOverride ?? await publicCsrf(page, runtime, pathPart);
  try {
    const response = await page.request.post(`${runtime.baseURL}${pathPart}`, {
      multipart: {
        _csrf: csrf,
        submission_token: `${options.fixture.key}-${Date.now()}-${Math.random()}`,
        subject: `upload ${options.fixture.key}`,
        body: `upload validation ${options.fixture.key}`,
        file: {
          name: options.fixture.name,
          mimeType: options.fixture.mimeType,
          buffer: await fsp.readFile(options.fixture.path),
        },
      },
      headers: options.headers,
      maxRedirects: 0,
      timeout: 60_000,
    });
    const result = responseToUploadResult(response, options.headers);
    if (![302, 303].includes(response.status())) {
      const text = await response.text();
      const visibleError = extractVisibleErrorText(text);
      result.visibleErrorText = visibleError;
      result.classification = classifyResponse(response.status(), visibleError, response.headers());
    }
    return result;
  } catch (error) {
    return {
      classification: classifyTransportError(error),
      visibleErrorText: error instanceof Error ? error.message : String(error),
      requestHeaders: options.headers,
    };
  }
}

function extractVisibleErrorText(html: string): string {
  const banner = html.match(/class="[^"]*post-error-banner[^"]*"[^>]*>([\s\S]*?)<\/div>/i);
  const source = banner?.[1] ?? html;
  return decodeHtml(source.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim()).slice(0, 1000);
}

function decodeHtml(value: string): string {
  return value
    .replace(/&quot;/g, '"')
    .replace(/&#x27;/g, "'")
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>');
}

function responseToUploadResult(response: APIResponse, requestHeaders?: Record<string, string>): UploadResult {
  const status = response.status();
  const headers = response.headers();
  const location = headers.location;
  return {
    status,
    responseUrl: response.url(),
    location,
    classification: classifyResponse(status, '', headers),
    requestHeaders,
    responseHeaders: pickHeaders(headers),
  };
}

async function verifyRenderedUpload(
  ctx: ScenarioContext,
  result: UploadResult,
  fixture: FixtureFile,
  options: { compareBytes?: boolean; expand?: boolean } = {},
): Promise<UploadResult> {
  await expect(ctx.page.locator('body')).toBeVisible();
  const body = await ctx.page.locator('body').innerText();
  expect(body).not.toMatch(/thread panicked|stack backtrace|SQLITE_|database is locked/i);

  const fileLink = ctx.page.locator('.file-info a[href^="/boards/"]').last();
  await expect(fileLink).toBeVisible();
  const mediaHref = await fileLink.getAttribute('href');
  if (!mediaHref) throw new Error('uploaded media link was not rendered');

  const mediaResponse = await ctx.page.request.get(`${ctx.runtime.baseURL}${mediaHref}`);
  expect(mediaResponse.status()).toBe(200);
  const mediaBytes = await mediaResponse.body();
  result.mediaHref = mediaHref;
  result.downloadedBytes = mediaBytes.length;
  result.downloadedSha256 = sha256(mediaBytes);
  result.expectedSha256 = fixture.sha256;
  result.responseHeaders = { ...result.responseHeaders, downloadContentType: mediaResponse.headers()['content-type'] };
  if (options.compareBytes) {
    expect(result.downloadedSha256).toBe(fixture.sha256);
  }

  if (fixture.mediaKind === 'audio') {
    await expect(ctx.page.locator('.audio-container').last()).toBeVisible();
  } else if (fixture.mediaKind !== 'other') {
    await expect(ctx.page.locator('.media-preview, [data-media-thumb="1"], .audio-player').first()).toBeVisible();
  }

  if (options.expand && ctx.jsEnabled && ['image', 'video', 'pdf'].includes(fixture.mediaKind)) {
    const preview = ctx.page.locator('.media-preview').last();
    if (await preview.isVisible().catch(() => false)) {
      await preview.click();
      await expect(ctx.page.locator('.media-expanded').last()).toBeVisible();
    }
  }

  result.classification = 'success';
  return result;
}

function assertAccepted(result: UploadResult): void {
  if (![302, 303].includes(result.status ?? 0) || !result.location) {
    throw new Error(`expected upload success redirect, got ${result.status ?? result.classification}: ${result.visibleErrorText ?? ''}`);
  }
}

function assertRejected(result: UploadResult, clearError: RegExp): void {
  if ([302, 303].includes(result.status ?? 0)) {
    throw new Error(`expected upload rejection, got success redirect to ${result.location}`);
  }
  if (result.status && result.status >= 200 && result.status < 300) {
    throw new Error(`expected non-2xx upload rejection, got status ${result.status}: ${result.visibleErrorText ?? ''}`);
  }
  const visible = result.visibleErrorText ?? '';
  if (![400, 403, 413, 415, 422].includes(result.status ?? 0) && result.classification === 'success') {
    throw new Error(`expected upload rejection, got status ${result.status}`);
  }
  if (result.status && !clearError.test(visible) && !clearError.test(result.classification)) {
    throw new Error(`rejection did not include a clear upload error: status=${result.status} text=${visible.slice(0, 200)}`);
  }
}

async function assertNoLocalUploadTempLeaks(runtime: UploadRuntime, board: string): Promise<void> {
  if (!runtime.app) return;
  const boardDir = path.join(runtime.app.dataDir, 'boards', board);
  if (!fs.existsSync(boardDir)) return;
  const leaked = await listFiles(boardDir, (name) => name.startsWith('.tmp_') || name.startsWith('rustchan-upload-'));
  expect(leaked).toEqual([]);
}

async function validateGlobalAnyFileFeatureDisabled(ctx: ScenarioContext, fixture: FixtureFile): Promise<UploadResult> {
  const app = await RustChanServer.create(undefined, {
    env: {
      CHAN_ENABLE_ANY_FILE_UPLOADS_FEATURE: '0',
      CHAN_PUBLIC_HOSTS: 'localhost,127.0.0.1,::1',
    },
  });
  try {
    app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
    app.createBoardCli({ short: 'gany', name: 'Global Any Disabled' });
    setBoardFixtureSettings(app, 'gany', {
      allowImages: false,
      allowVideo: false,
      allowAudio: false,
      allowPdf: false,
      allowAnyFiles: true,
      maxImageSizeMb: 1,
      maxVideoSizeMb: 2,
      maxAudioSizeMb: 1,
    });
    await app.start();
    const runtime: UploadRuntime = {
      mode: 'local',
      baseURL: app.baseURL,
      fixtureDir: app.fixtureDir,
      app,
      adminUsername: ADMIN_USERNAME,
      adminPassword: ADMIN_PASSWORD,
    };
    const result = await submitRequestUpload(ctx.page, runtime, { board: 'gany', fixture });
    assertRejected(result, /only accepts|disabled|not allowed/i);
    return result;
  } finally {
    await app.dispose();
  }
}

function classifyResponse(status: number, text: string, headers: Record<string, string>): string {
  const joined = `${text} ${headers.location ?? ''}`.toLowerCase();
  if ([302, 303].includes(status)) return 'success';
  if (status === 403 || joined.includes('csrf token mismatch') || joined.includes('origin')) return 'csrf-or-origin-rejection';
  if (status === 413 || joined.includes('too large') || joined.includes('body') && joined.includes('limit')) return 'request-size-or-body-limit';
  if (status === 415 || joined.includes('not allowed') || joined.includes('accepted:')) return 'app-validation-media-type';
  if (status === 422 || joined.includes('disabled') || joined.includes('malformed') || joined.includes('empty')) return 'app-validation';
  if (status >= 500) return 'server-error';
  return `http-${status}`;
}

function classifyTransportError(error: unknown): string {
  const text = error instanceof Error ? error.message.toLowerCase() : String(error).toLowerCase();
  if (text.includes('timeout')) return 'timeout';
  if (text.includes('econnreset') || text.includes('socket hang up') || text.includes('connection reset')) return 'connection-reset-or-proxy-truncation';
  if (text.includes('failed to fetch') || text.includes('net::')) return 'network-or-proxy-failure';
  return 'transport-error';
}

async function createUploadFixtures(dir: string): Promise<Record<string, FixtureFile>> {
  await fsp.mkdir(dir, { recursive: true });
  const files: Record<string, FixtureFile> = {};
  const add = async (key: string, name: string, mimeType: string, mediaKind: MediaKind, bytes: Buffer) => {
    const filePath = path.join(dir, `${key}-${sanitizeLocalName(name)}`);
    await fsp.writeFile(filePath, bytes);
    files[key] = {
      key,
      path: filePath,
      name,
      mimeType,
      size: bytes.length,
      sha256: sha256(bytes),
      mediaKind,
    };
  };

  const tinyPng = pngRgba(2, 2, (x, y) => [x * 80, y * 80, 180, 255]);
  await add('png', 'tiny.png', 'image/png', 'image', tinyPng);
  await add('duplicatePng', 'duplicate.png', 'image/png', 'image', tinyPng);
  await add('pngNamedJpg', 'misleading.jpg', 'image/jpeg', 'image', tinyPng);
  await add('jpg', 'tiny.jpg', 'image/jpeg', 'image', Buffer.from(JPEG_1X1_BASE64, 'base64'));
  await add('gif', 'tiny.gif', 'image/gif', 'image', Buffer.from('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==', 'base64'));
  await add('webp', 'tiny.webp', 'image/webp', 'image', Buffer.from(WEBP_1X1_BASE64, 'base64'));
  await add('bmp', 'tiny.bmp', 'image/bmp', 'image', bmp1x1());
  await add('tiff', 'tiny.tiff', 'image/tiff', 'image', tiff1x1());
  await add('heic', 'tiny.heic', 'image/heic', 'image', ftypFile('heic'));
  await add('heif', 'tiny.heif', 'image/heif', 'image', ftypFile('mif1'));
  await add('mp4', 'tiny.mp4', 'video/mp4', 'video', mp4Fixture(4096));
  // This header-only fixture is accepted and preserved as a neutral download
  // when the isolated validation runtime intentionally has no ffprobe.
  await add('webm', 'tiny.webm', 'video/webm', 'other', webmFixture(4096));
  await add('mp3', 'tiny.mp3', 'audio/mpeg', 'audio', prefixedFixture(Buffer.from([0xff, 0xfb, 0x90, 0x64]), 2048));
  await add('ogg', 'tiny.ogg', 'audio/ogg', 'audio', prefixedFixture(Buffer.from('OggS'), 2048));
  await add('flac', 'tiny.flac', 'audio/flac', 'audio', prefixedFixture(Buffer.from('fLaC'), 2048));
  await add('wav', 'tiny.wav', 'audio/wav', 'audio', wavFixture(2048));
  await add('m4a', 'tiny.m4a', 'audio/mp4', 'audio', m4aFixture(2048));
  await add('aac', 'tiny.aac', 'audio/aac', 'audio', tinyAacFixture());
  await add('pdf', 'tiny.pdf', 'application/pdf', 'pdf', pdfFixture(2048));
  await add('genericText', 'notes.txt', 'text/plain', 'other', Buffer.from('plain text generic download fixture\n'));
  await add('textRenamedPng', 'fake.png', 'image/png', 'other', Buffer.from('plain text renamed as png\n'));
  await add('zero', 'empty.png', 'image/png', 'image', Buffer.alloc(0));
  await add('truncatedPng', 'truncated.png', 'image/png', 'image', Buffer.from('\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR', 'binary'));
  await add('malformedAac', 'malformed.aac', 'audio/aac', 'audio', prefixedFixture(Buffer.from([0xff, 0xf1, 0x50, 0x80]), 2048));
  await add('svg', 'active.svg', 'image/svg+xml', 'image', Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>\n'));
  await add('mkv', 'unsupported.mkv', 'video/x-matroska', 'video', prefixedFixture(Buffer.from([0x1a, 0x45, 0xdf, 0xa3]), 4096));
  await add('malformedPdf', 'missing-eof.pdf', 'application/pdf', 'pdf', Buffer.from('%PDF-1.7\n1 0 obj\n<<>>\nendobj\n'));
  await add('trickySpaces', 'spaces unicode-é quotes \' ".png', 'image/png', 'image', tinyPng);
  await add('trickyShell', 'semi;dollar$backtick`pipe|amp&.png', 'image/png', 'image', tinyPng);
  await add('trickyBackslash', 'dir\\windows\\name.png', 'image/png', 'image', tinyPng);
  await add('trickyTraversal', '../..//traversal-like.png', 'image/png', 'image', tinyPng);
  await add('trickyLong', `${'long-name-'.repeat(18)}.png`, 'image/png', 'image', tinyPng);
  await add('pngUnderImageCap', 'image-under-cap.png', 'image/png', 'image', pngWithTargetSize(MIB - 1));
  await add('pngExactImageCap', 'image-exact-cap.png', 'image/png', 'image', pngWithTargetSize(MIB));
  await add('pngOverImageCap', 'image-over-cap.png', 'image/png', 'image', pngWithTargetSize(MIB + 1));
  await add('pngClearOverImageCap', 'image-clear-over-cap.png', 'image/png', 'image', pngWithTargetSize(2 * MIB + 256));
  await add('mp4ExactVideoCap', 'video-exact-cap.mp4', 'video/mp4', 'video', mp4Fixture(2 * MIB));
  await add('mp4OverVideoCap', 'video-over-cap.mp4', 'video/mp4', 'video', mp4Fixture(2 * MIB + 1));
  await add('oggExactAudioCap', 'audio-exact-cap.ogg', 'audio/ogg', 'audio', prefixedFixture(Buffer.from('OggS'), MIB));
  await add('oggOverAudioCap', 'audio-over-cap.ogg', 'audio/ogg', 'audio', prefixedFixture(Buffer.from('OggS'), MIB + 1));
  await add('pdfExactGenericCap', 'pdf-exact-generic-cap.pdf', 'application/pdf', 'pdf', pdfFixture(2 * MIB));
  await add('pdfOverGenericCap', 'pdf-over-generic-cap.pdf', 'application/pdf', 'pdf', pdfFixture(2 * MIB + 1));
  await add('genericExactCap', 'generic-exact-cap.bin', 'application/octet-stream', 'other', deterministicBytes(2 * MIB, 91));
  await add('genericOverCap', 'generic-over-cap.bin', 'application/octet-stream', 'other', deterministicBytes(2 * MIB + 1, 92));

  return files;
}

function pngRgba(width: number, height: number, pixel: (x: number, y: number) => [number, number, number, number]): Buffer {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y += 1) {
    const rowStart = y * (width * 4 + 1);
    raw[rowStart] = 0;
    for (let x = 0; x < width; x += 1) {
      const [r, g, b, a] = pixel(x, y);
      const offset = rowStart + 1 + x * 4;
      raw[offset] = r;
      raw[offset + 1] = g;
      raw[offset + 2] = b;
      raw[offset + 3] = a;
    }
  }
  return buildPng(width, height, [pngChunk('IDAT', zlib.deflateSync(raw))]);
}

function pngWithTargetSize(size: number): Buffer {
  const base = pngRgba(2, 2, (x, y) => [20 + x, 40 + y, 120, 255]);
  const extraLength = size - base.length - 12;
  if (extraLength < 4) throw new Error(`target PNG size ${size} is too small`);
  const textData = Buffer.concat([Buffer.from('pad\0'), deterministicBytes(extraLength - 4, 41)]);
  return buildPng(2, 2, [
    pngChunk('IDAT', zlib.deflateSync(Buffer.from([0, 20, 40, 120, 255, 21, 40, 120, 255, 0, 20, 41, 120, 255, 21, 41, 120, 255]))),
    pngChunk('tEXt', textData),
  ]);
}

function buildPng(width: number, height: number, chunks: Buffer[]): Buffer {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  return Buffer.concat([
    Buffer.from('\x89PNG\r\n\x1a\n', 'binary'),
    pngChunk('IHDR', ihdr),
    ...chunks,
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

function pngChunk(type: string, data: Buffer): Buffer {
  const typeBuffer = Buffer.from(type, 'ascii');
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuffer, data])), 0);
  return Buffer.concat([length, typeBuffer, data, crc]);
}

function crc32(buffer: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc = (crc >>> 8) ^ CRC_TABLE[(crc ^ byte) & 0xff];
  }
  return (crc ^ 0xffffffff) >>> 0;
}

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k += 1) {
    c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  }
  return c >>> 0;
});

function bmp1x1(): Buffer {
  const file = Buffer.alloc(58, 0);
  file.write('BM', 0, 'ascii');
  file.writeUInt32LE(58, 2);
  file.writeUInt32LE(54, 10);
  file.writeUInt32LE(40, 14);
  file.writeInt32LE(1, 18);
  file.writeInt32LE(1, 22);
  file.writeUInt16LE(1, 26);
  file.writeUInt16LE(24, 28);
  file.writeUInt32LE(4, 34);
  file[54] = 255;
  file[55] = 0;
  file[56] = 0;
  file[57] = 0;
  return file;
}

function tiff1x1(): Buffer {
  const entryCount = 10;
  const ifdSize = 2 + entryCount * 12 + 4;
  const bitsOffset = 8 + ifdSize;
  const pixelOffset = bitsOffset + 6;
  const out = Buffer.alloc(pixelOffset + 3, 0);
  out.write('II', 0, 'ascii');
  out.writeUInt16LE(42, 2);
  out.writeUInt32LE(8, 4);
  out.writeUInt16LE(entryCount, 8);
  let offset = 10;
  const entry = (tag: number, type: number, count: number, value: number) => {
    out.writeUInt16LE(tag, offset);
    out.writeUInt16LE(type, offset + 2);
    out.writeUInt32LE(count, offset + 4);
    if (type === 3 && count === 1) {
      out.writeUInt16LE(value, offset + 8);
    } else {
      out.writeUInt32LE(value, offset + 8);
    }
    offset += 12;
  };
  entry(256, 4, 1, 1);
  entry(257, 4, 1, 1);
  entry(258, 3, 3, bitsOffset);
  entry(259, 3, 1, 1);
  entry(262, 3, 1, 2);
  entry(273, 4, 1, pixelOffset);
  entry(277, 3, 1, 3);
  entry(278, 4, 1, 1);
  entry(279, 4, 1, 3);
  entry(284, 3, 1, 1);
  out.writeUInt16LE(8, bitsOffset);
  out.writeUInt16LE(8, bitsOffset + 2);
  out.writeUInt16LE(8, bitsOffset + 4);
  out[pixelOffset] = 255;
  out[pixelOffset + 1] = 0;
  out[pixelOffset + 2] = 0;
  return out;
}

function ftypFile(brand: string): Buffer {
  const out = Buffer.alloc(96, 0);
  out.writeUInt32BE(24, 0);
  out.write('ftyp', 4, 'ascii');
  out.write(brand, 8, 'ascii');
  out.writeUInt32BE(0, 12);
  out.write(brand, 16, 'ascii');
  out.write('isom', 20, 'ascii');
  return out;
}

function mp4Fixture(size: number): Buffer {
  const prefix = Buffer.concat([
    Buffer.from([0x00, 0x00, 0x00, 0x18]),
    Buffer.from('ftypisom'),
    Buffer.from([0x00, 0x00, 0x02, 0x00]),
    Buffer.from('isomiso2mp41'),
  ]);
  return prefixedFixture(prefix, size);
}

function m4aFixture(size: number): Buffer {
  const prefix = Buffer.concat([
    Buffer.from([0x00, 0x00, 0x00, 0x18]),
    Buffer.from('ftypM4A '),
    Buffer.from([0x00, 0x00, 0x00, 0x00]),
    Buffer.from('M4A isom'),
  ]);
  return prefixedFixture(prefix, size);
}

function webmFixture(size: number): Buffer {
  const prefix = Buffer.from([
    0x1a, 0x45, 0xdf, 0xa3, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x42, 0x82, 0x84, 0x77, 0x65, 0x62, 0x6d,
  ]);
  return prefixedFixture(prefix, size);
}

function wavFixture(size: number): Buffer {
  const out = prefixedFixture(Buffer.from('RIFF\x00\x00\x00\x00WAVEfmt ', 'binary'), size);
  out.writeUInt32LE(Math.max(0, size - 8), 4);
  return out;
}

function pdfFixture(size: number): Buffer {
  const prefix = Buffer.from('%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n');
  const suffix = Buffer.from('\n%%EOF\n');
  const pad = size - prefix.length - suffix.length;
  if (pad < 0) throw new Error(`target PDF size ${size} is too small`);
  return Buffer.concat([prefix, Buffer.from(`%${'p'.repeat(Math.max(0, pad - 1))}`), suffix]);
}

function prefixedFixture(prefix: Buffer, size: number): Buffer {
  if (prefix.length > size) throw new Error(`prefix length ${prefix.length} exceeds target ${size}`);
  return Buffer.concat([prefix, deterministicBytes(size - prefix.length, prefix.length)]);
}

function tinyAacFixture(): Buffer {
  return Buffer.from(
    '//FQQCP//N4CAExhdmM2Mi4yOC4xMDEAAjCrXOkMQg6tTc5via6bg0VF1KZJ29kiABKEgimESckithJkIjGSSDEu2VnbKkNwMnm9hZ+GweZRce05ctx5NdRTCIVkkiIyGkmxiMkRIkPHsQkNhFgSRSfxfw32H2r7D9O9Z+K7B7i654y4juLbujtG6qxHQ3MPXXZPN2zdjbJ0lsHMWqcW4zvXHd62nesTjsbYrjZtBxVxsVhuU7ap21TtarM9Sz2g2K42Kw3Kw2qdtUa/PsdStqVs+v0bTRtWa00a/PsdCtoVVCqlK5SuUrlK7V4asdWOrRq0YaMNEk0k1U1U0k0lD0SUSTSTVTVTVTSUSUSUSUVTVTVTVTSUSURRRRRRRRf/8VBAI7/8AWaY2TbrNROLcJqStxUj/T/vri2Fxerrv19dBer1/XoTV2T+t8L41cqQYr0YePLcimsQ49gpMwRHXZMkmGT5tJ6KqxA/ZluNjnge5PthjkjvHRrVU6dqlP/G/Vrt3/k6JBga7kEhIMDO4SEgwMDO7hISsGBgZ3cJCTgMOZd3CTjQMyDXnd8gkJyXkHPkr+3ol1qd2JF0BPB7e64Pm1A4CVikjwv1OZY64pqrX1O3NLNtuFkyUppVJmul2kmkmxwxrrZ2kmkbHCmt2dpJnbGtndnZ2kZ2dscGdndnZ2dnZ0Z2dnnQrKiL6c7xAAgQdvvWTNCJdPnTPE3pu/QK6LGi10WMTOilElElE/KdEhITMlCUt1Lg01z/8VBAHR/8ATSZssxE4s1oTlEHoevP615Jer1//a8/XnjUAKN+IhFAhkagw4oswecTtaqYXuqlkQDtE1LQB1TP/GBpVf8kJBgZ3CQkGBncJCQYGBndwkJCQYGBndwkJKgwMDO7hITTBkQMid3cJCWstL2cCeZUTIswx4HIIrDGsxK+qwGOTutcIrfos11md61z1aa6dusonOftfbv1PLdidukUGkg8v/fs1214WpanXEWf9qr7m8tS3OcOReLbg4Vdc2tKcBoXr5aEb/B1PC0/Bd33cAr/0L+k/Vnd3AB8CP/6/FPjf2rg//FQQAGf/AEYgbRw',
    'base64',
  );
}

function deterministicBytes(size: number, seed: number): Buffer {
  const out = Buffer.alloc(size);
  let state = seed >>> 0;
  for (let i = 0; i < size; i += 1) {
    state = (state * 1664525 + 1013904223) >>> 0;
    out[i] = state & 0xff;
  }
  return out;
}

function sanitizeLocalName(name: string): string {
  return name.replaceAll('/', '_').replaceAll('\\', '_').replace(/[^\w .'"$;|&`()\-\u00c0-\u017f]/gu, '_').slice(0, 120);
}

function sha256(bytes: Buffer): string {
  return crypto.createHash('sha256').update(bytes).digest('hex');
}

function tunnelLikeHeaders(baseURL: string): Record<string, string> {
  const url = new URL(baseURL);
  const headers: Record<string, string> = {
    origin: baseURL,
    referer: `${baseURL}/`,
    'x-forwarded-proto': url.protocol.replace(':', ''),
    'x-forwarded-host': url.host,
  };
  if (process.env.RUSTCHAN_UPLOAD_HEADER_HOST) {
    headers.host = process.env.RUSTCHAN_UPLOAD_HEADER_HOST;
  }
  return headers;
}

function pickHeaders(headers: Record<string, string>): Record<string, string> {
  const picked: Record<string, string> = {};
  for (const key of ['location', 'content-type', 'content-length', 'x-content-type-options', 'server']) {
    if (headers[key]) picked[key] = headers[key];
  }
  return picked;
}

async function listFiles(root: string, predicate: (name: string) => boolean): Promise<string[]> {
  const found: string[] = [];
  const entries = await fsp.readdir(root, { withFileTypes: true }).catch(() => []);
  for (const entry of entries) {
    const full = path.join(root, entry.name);
    if (predicate(entry.name)) found.push(full);
    if (entry.isDirectory()) {
      found.push(...await listFiles(full, predicate));
    }
  }
  return found;
}

async function captureScenarioScreenshot(page: Page, testInfo: TestInfo, id: string): Promise<string> {
  const screenshotPath = testInfo.outputPath(`upload-validation-${id}.png`);
  await page.screenshot({ path: screenshotPath, fullPage: true });
  return screenshotPath;
}

async function writeSummary(summary: { scenarios: ScenarioResult[]; counts?: Record<Outcome, number> }, testInfo: TestInfo): Promise<void> {
  summary.counts = {
    pass: summary.scenarios.filter((scenario) => scenario.outcome === 'pass').length,
    fail: summary.scenarios.filter((scenario) => scenario.outcome === 'fail').length,
    skip: summary.scenarios.filter((scenario) => scenario.outcome === 'skip').length,
  };
  await fsp.mkdir(SUMMARY_DIR, { recursive: true });
  const body = `${JSON.stringify(summary, null, 2)}\n`;
  await fsp.writeFile(summaryPath(testInfo), body);
  await testInfo.attach('upload-validation-summary', {
    body,
    contentType: 'application/json',
  });
}

function summaryPath(testInfo: TestInfo): string {
  return path.join(SUMMARY_DIR, `${process.env.RUSTCHAN_UPLOAD_BASE_URL ? 'external' : 'local'}-${testInfo.project.name}.json`);
}

const JPEG_1X1_BASE64 =
  '/9j/4AAQSkZJRgABAQAASABIAAD/4QBMRXhpZgAATU0AKgAAAAgAAYdpAAQAAAABAAAAGgAAAAAA' +
  'A6ABAAMAAAABAAEAAKACAAQAAAABAAAAAaADAAQAAAABAAAAAQAAAAD/7QA4UGhvdG9zaG9wIDMu' +
  'MAA4QklNBAQAAAAAAAA4QklNBCUAAAAAABDUHYzZjwCyBOmACZjs+EJ+/8AAEQgAAQABAwEiAAIR' +
  'AQMRAf/EAB8AAAEFAQEBAQEBAAAAAAAAAAABAgMEBQYHCAkKC//EALUQAAIBAwMCBAMFBQQEAAAB' +
  'fQECAwAEEQUSITFBBhNRYQcicRQygZGhCCNCscEVUtHwJDNicoIJChYXGBkaJSYnKCkqNDU2Nzg5' +
  'OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6g4SFhoeIiYqSk5SVlpeYmZqio6Slpqeoq' +
  'aqys7S1tre4ubrCw8TFxsfIycrS09TV1tfY2drh4uPk5ebn6Onq8fLz9PX29/j5+v/EAB8BAAMBA' +
  'QEBAQEBAQEAAAAAAAABAgMEBQYHCAkKC//EALURAAIBAgQEAwQHBQQEAAECdwABAgMRBAUhMQYS' +
  'QVEHYXETIjKBCBRCkaGxwQkjM1LwFWJy0QoWJDThJfEXGBkaJicoKSo1Njc4OTpDREVGR0hJSlNU' +
  'VVZXWFlaY2RlZmdoaWpzdHV2d3h5eoKDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5u' +
  'sLDxMXGx8jJytLT1NXW19jZ2uLj5OXm5+jp6vLz9PX29/j5+v/bAEMAAgICAgICAwICAwUDAwMF' +
  'BgUFBQUGCAYGBgYGCAoICAgICAgKCgoKCgoKCgwMDAwMDA4ODg4ODw8PDw8PDw8PD//bAEMBAgIC' +
  'BAQEBwQEBxALCQsQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBA' +
  'QEBAQEP/dAAQAAf/aAAwDAQACEQMRAD8Ap0UUV/O5/k2f/9k=';

const WEBP_1X1_BASE64 = 'UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA';
