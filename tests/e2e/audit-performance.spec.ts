/**
 * RustChan bounded performance diagnostics — NOT a load test.
 *
 * ## Measurement contract
 * These tests record evidence; they do not assert latency thresholds. Every
 * scenario takes >= 5 samples and labels sample 1 `cold` and samples 2..N
 * `warm`. In this file "cold" means "the first navigation in a browser context
 * whose page cache has not been used for that page": scenario setup that must
 * not warm the cache is done through the request API (no page navigation).
 * Where setup does touch the browser (the upload flow that precedes the media
 * samples, or an admin login performed through the UI) the attachment records
 * that caveat explicitly. Server-side caches are always warm from test setup,
 * so no sample is a cold-server measurement.
 *
 * ## Environment per run
 * - Project/browser: recorded from `testInfo.project.name` and `browser.version()`.
 *   The bounded samples run on the desktop Chromium project only; the
 *   capability probe runs on every JS-enabled project so other engines are
 *   described by observation rather than assumption.
 * - Runtime: isolated local RustChan fixture on a loopback ephemeral port
 *   (`./helpers`), one instance per test, removed at teardown.
 * - Instrumentation overhead: `test.use({ trace: 'off', video: 'off' })`
 *   because trace/video capture distorts navigation timing. Screenshots remain
 *   `only-on-failure` from the shared config. Playwright network-event
 *   observation and in-page performance entry buffers add overhead to every
 *   sample; absolute numbers include that overhead and are not release
 *   certification numbers.
 *
 * ## Engine support (detected, not assumed)
 * - Navigation Timing and Resource Timing are read only when
 *   `performance.getEntriesByType` exposes the entry type; otherwise the metric
 *   is recorded as `null` with a reason and never asserted.
 * - `PerformanceObserver.supportedEntryTypes` is probed per engine; entry types
 *   such as `largest-contentful-paint`, `layout-shift`, `longtask`, and `paint`
 *   are engine-dependent and are recorded, never asserted.
 * - `request.sizes()` is a Playwright API; engines that report -1 sizes are
 *   recorded as `null`, never as zero.
 * - The no-JS project can expose no JS performance API and is excluded from the
 *   probe with an explicit reason.
 *
 * ## What IS asserted
 * - Real workflow outcomes (pages rendered, thread count increased, image
 *   decoded: `naturalWidth > 0`), so timings describe a genuine workflow.
 * - No duplicate requests for the same resource within one navigation
 *   (intentional repeating endpoints are excluded and labelled).
 * - The RustChan process is alive before/after server observation samples.
 *
 * ## Server CPU/RSS limitation
 * `ps -o %cpu,rss` samples describe one bounded window. RSS can rise without a
 * leak and fall without a fix; a single increase is NOT evidence of a memory
 * leak and no leak assertion is made. `ps` `%cpu` is an interval average, not
 * an instantaneous value.
 */
import { spawnSync } from 'node:child_process';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  createThread,
  createThreadViaRequest,
  expect,
  extractCsrf,
  setBoardFixtureSettings,
  sqliteQuery,
  test,
  threadIdFromUrl,
  uniqueShort,
} from './helpers';
import { phase4SkipUnless } from './phase4-helpers';
import type { Page, Request, Response, TestInfo } from './diagnostics';
import * as diagnosticsApi from './diagnostics';

// Trace and video capture distort navigation timing; measurement runs without
// them. Failure screenshots remain enabled through the shared config.
test.use({ trace: 'off', video: 'off' });

type StrictDiagnosticsApi = {
  measureStep?: <T>(name: string, fn: () => Promise<T>) => Promise<T>;
  actor?: (role: 'visitor' | 'admin') => void;
};
const strictApi = diagnosticsApi as unknown as StrictDiagnosticsApi;

function actor(role: 'visitor' | 'admin'): void {
  strictApi.actor?.(role);
}

async function measured<T>(name: string, fn: () => Promise<T>): Promise<T> {
  if (strictApi.measureStep) {
    return strictApi.measureStep(name, fn);
  }
  return fn();
}

const SAMPLES = 5;

/**
 * Endpoints expected to repeat while a page is open (polling, user-triggered
 * updates). They are excluded from the duplicate-request check because
 * repetition is the documented behaviour, not an accidental fetch.
 */
const REPEATING_ENDPOINT_PATTERNS = [
  /\/updates\?/,
  /\/admin\/log\/live/,
  /\/admin\/backup\/progress/,
  /\/admin\/db\/repair\/progress/,
];

/** Logs in without navigating a page, so measured samples can start cold. */
async function loginViaRequest(page: Page, baseURL: string): Promise<void> {
  const loginPage = await page.request.get(`${baseURL}/admin`);
  const csrf = extractCsrf(await loginPage.text());
  const response = await page.request.post(`${baseURL}/admin/login`, {
    form: {
      _csrf: csrf,
      username: ADMIN_USERNAME,
      password: ADMIN_PASSWORD,
    },
    maxRedirects: 0,
  });
  expect([302, 303], 'request-API admin login should succeed').toContain(response.status());
}

type RequestRecord = { method: string; url: string };

type NavigationTimingSnapshot = {
  type: string;
  durationMs: number;
  responseStartMs: number;
  responseEndMs: number;
  domContentLoadedMs: number;
  loadEventEndMs: number;
  transferSize: number;
  encodedBodySize: number;
  decodedBodySize: number;
  nextHopProtocol: string;
};

type NavigationSample = {
  label: string;
  url: string;
  durationMs: number;
  navigationTiming: NavigationTimingSnapshot | null;
  navigationTimingNote: string | null;
  requestCount: number;
  duplicateRequests: string[];
};

type Summary = {
  sampleCount: number;
  minMs: number;
  p50Ms: number;
  p90Ms: number;
  maxMs: number;
};

function summarize(values: number[]): Summary {
  if (values.length === 0) {
    return { sampleCount: 0, minMs: 0, p50Ms: 0, p90Ms: 0, maxMs: 0 };
  }
  const sorted = [...values].sort((left, right) => left - right);
  const pick = (fraction: number): number => sorted[
    Math.min(sorted.length - 1, Math.max(0, Math.ceil(fraction * sorted.length) - 1))
  ];
  return {
    sampleCount: sorted.length,
    minMs: Math.round(sorted[0] * 10) / 10,
    p50Ms: Math.round(pick(0.5) * 10) / 10,
    p90Ms: Math.round(pick(0.9) * 10) / 10,
    maxMs: Math.round(sorted[sorted.length - 1] * 10) / 10,
  };
}

function beginRequestRecording(page: Page): () => { requests: RequestRecord[]; duplicates: string[] } {
  const requests: RequestRecord[] = [];
  const listener = (request: Request) => {
    requests.push({ method: request.method(), url: request.url() });
  };
  page.on('request', listener);
  return () => {
    page.off('request', listener);
    const counts = new Map<string, number>();
    for (const request of requests) {
      const key = `${request.method} ${request.url}`;
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    const duplicates = [...counts.entries()]
      .filter(([key, count]) => count > 1 && !REPEATING_ENDPOINT_PATTERNS.some((pattern) => pattern.test(key)))
      .map(([key, count]) => `${count}x ${key}`);
    return { requests, duplicates };
  };
}

async function readNavigationTiming(page: Page): Promise<{ timing: NavigationTimingSnapshot | null; note: string | null }> {
  return page.evaluate(() => {
    if (typeof performance.getEntriesByType !== 'function') {
      return { timing: null, note: 'performance.getEntriesByType is not exposed' };
    }
    const entries = performance.getEntriesByType('navigation') as PerformanceNavigationTiming[];
    const entry = entries[0];
    if (!entry) {
      return { timing: null, note: 'Navigation Timing entry is not exposed by this engine' };
    }
    return {
      timing: {
        type: entry.type,
        durationMs: entry.duration,
        responseStartMs: entry.responseStart,
        responseEndMs: entry.responseEnd,
        domContentLoadedMs: entry.domContentLoadedEventEnd,
        loadEventEndMs: entry.loadEventEnd,
        transferSize: entry.transferSize,
        encodedBodySize: entry.encodedBodySize,
        decodedBodySize: entry.decodedBodySize,
        nextHopProtocol: entry.nextHopProtocol,
      },
      note: null,
    };
  });
}

/** Navigates once, recording wall-clock time, per-navigation requests, and Navigation Timing. */
async function measureNavigation(page: Page, url: string, label: string): Promise<NavigationSample> {
  const finishRequests = beginRequestRecording(page);
  const started = performance.now();
  await page.goto(url, { waitUntil: 'load' });
  const durationMs = performance.now() - started;
  const { requests, duplicates } = finishRequests();
  const { timing, note } = await readNavigationTiming(page);
  return {
    label,
    url,
    durationMs: Math.round(durationMs * 10) / 10,
    navigationTiming: timing,
    navigationTimingNote: note,
    requestCount: requests.length,
    duplicateRequests: duplicates,
  };
}

async function attachEvidence(testInfo: TestInfo, name: string, payload: unknown): Promise<void> {
  await testInfo.attach(name, {
    body: JSON.stringify(payload, null, 2),
    contentType: 'application/json',
  });
}

// ---------------------------------------------------------------------------
// Engine capability probe (all JS-enabled projects; no latency assertions)
// ---------------------------------------------------------------------------
test.describe('performance instrumentation capability probe', () => {
  test.beforeEach(({}, testInfo) => {
    test.skip(
      testInfo.project.name === 'firefox-nojs',
      'the no-JS project exposes no JavaScript performance APIs to probe',
    );
  });

  test('the engine exposes the performance APIs this audit can read', async ({ page, app, browser }) => {
    await page.goto(app.baseURL, { waitUntil: 'load' });
    const capabilities = await page.evaluate(() => {
      const observer = typeof PerformanceObserver !== 'undefined' ? PerformanceObserver : null;
      const supported = observer && Array.isArray(observer.supportedEntryTypes)
        ? [...observer.supportedEntryTypes]
        : [];
      return {
        navigationTiming: typeof performance.getEntriesByType === 'function'
          && performance.getEntriesByType('navigation').length > 0,
        resourceTiming: typeof performance.getEntriesByType === 'function'
          && Array.isArray(performance.getEntriesByType('resource')),
        supportedEntryTypes: supported,
        largestContentfulPaint: supported.includes('largest-contentful-paint'),
        layoutShift: supported.includes('layout-shift'),
        longtask: supported.includes('longtask'),
        paint: supported.includes('paint'),
        userAgent: navigator.userAgent,
      };
    });
    const payload = {
      project: test.info().project.name,
      browserVersion: browser.version(),
      note: 'Metric support is recorded per engine; absent entry types are skipped, never asserted.',
      capabilities,
    };
    console.log(`[audit-performance] capability probe: ${JSON.stringify(payload)}`);
    await attachEvidence(test.info(), 'audit-performance-capabilities.json', payload);
    expect(
      capabilities.navigationTiming,
      'Navigation Timing is required for every measured sample in this file',
    ).toBe(true);
  });
});

test.describe('bounded performance diagnostics — desktop chromium', () => {
  test.beforeEach(({}, testInfo) => {
    phase4SkipUnless(
      testInfo,
      ['chromium'],
      'bounded performance samples run on desktop Chromium; the capability probe records other engines',
    );
  });

  test('home → board → thread navigation stays within the request budget', async ({ page, app, browser }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('perf', testInfo);
    app.createBoardCli({ short: board, name: 'Performance Board', description: 'performance audit' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
    // Request-API setup keeps the browser context cold for sample 1.
    const threadId = await createThreadViaRequest(page, app, board, {
      subject: 'performance navigation thread',
      body: 'performance navigation body',
    });

    const samples: NavigationSample[] = [];
    for (let sample = 1; sample <= SAMPLES; sample += 1) {
      const state = sample === 1 ? 'cold' : 'warm';
      await measured(`home → board → thread sample ${sample} (${state})`, async () => {
        samples.push(await measureNavigation(page, app.baseURL, `home sample ${sample} (${state})`));
        samples.push(await measureNavigation(page, `${app.baseURL}/${board}`, `board sample ${sample} (${state})`));
        samples.push(await measureNavigation(page, `${app.baseURL}/${board}/thread/${threadId}`, `thread sample ${sample} (${state})`));
      });
    }

    // Real workflow outcomes, asserted alongside the timings.
    await expect(page.locator('#thread-posts').first()).toBeVisible();
    await expect(page.locator('.post').first()).toBeVisible();
    for (const sample of samples) {
      expect(sample.duplicateRequests, `${sample.label} should not fetch the same resource twice`).toEqual([]);
      expect(sample.requestCount, `${sample.label} should issue at least one request`).toBeGreaterThan(0);
    }
    const byLabel = new Map<string, number[]>();
    for (const sample of samples) {
      const key = sample.label.replace(/ sample \d+ \(.*\)$/, '');
      byLabel.set(key, [...(byLabel.get(key) ?? []), sample.durationMs]);
    }
    const summaries = Object.fromEntries([...byLabel.entries()].map(([key, values]) => [key, summarize(values)]));
    await attachEvidence(testInfo, 'audit-performance-navigation.json', {
      scenario: 'home → board → thread',
      project: testInfo.project.name,
      browserVersion: browser.version(),
      samples,
      summaries,
      requestBudget: 'no duplicate resource requests within one navigation (repeating endpoints excluded)',
      coldWarm: 'sample 1 cold browser cache (request-API setup, no prior page navigation); samples 2-5 warm browser cache; server caches warm from setup',
      note: 'percentiles are indicative; no latency threshold is asserted',
    });
  });

  test('thread creation submit reaches a visible post', async ({ page, app, browser }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('perf', testInfo);
    app.createBoardCli({ short: board, name: 'Performance Posting', description: 'posting performance audit' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });

    type CreationSample = {
      sample: number;
      state: string;
      boardNavigation: NavigationSample;
      submitToUrlMs: number;
      totalMs: number;
      threadId: number;
    };
    const samples: CreationSample[] = [];
    for (let sample = 1; sample <= SAMPLES; sample += 1) {
      const state = sample === 1 ? 'cold' : 'warm';
      const record = await measured(`thread creation sample ${sample} (${state})`, async () => {
        const boardNavigation = await measureNavigation(page, `${app.baseURL}/${board}`, `board before submit ${sample}`);
        const threadsBefore = Number(sqliteQuery(app, 'SELECT COUNT(*) FROM threads;'));
        const toggle = page.locator('[data-action="toggle-post-form"]').first();
        await toggle.click();
        const form = page.locator(`form[action="/${board}"]`).first();
        await expect(form).toBeVisible();
        const subject = `perf thread ${sample} ${Date.now()}`;
        await form.locator('input[name="subject"]').fill(subject);
        await form.locator('textarea[name="body"]').fill(`perf body ${sample}`);
        const submit = form.getByRole('button', { name: /post thread/i });
        await submit.scrollIntoViewIfNeeded();
        const submitStarted = performance.now();
        await Promise.all([
          page.waitForURL(new RegExp(`/${board}/thread/\\d+`), { waitUntil: 'domcontentloaded' }),
          submit.click(),
        ]);
        const submitToUrlMs = performance.now() - submitStarted;
        // Outcome assertions inside the measurement: only a real post counts.
        await expect(page.locator('.post').first()).toBeVisible();
        await expect(page.locator('body')).toContainText(subject);
        const threadsAfter = Number(sqliteQuery(app, 'SELECT COUNT(*) FROM threads;'));
        expect(threadsAfter, 'thread count should increase after submit').toBe(threadsBefore + 1);
        const threadId = threadIdFromUrl(page.url());
        return { sample, state, boardNavigation, submitToUrlMs, totalMs: 0, threadId };
      });
      samples.push({
        ...record,
        submitToUrlMs: Math.round(record.submitToUrlMs * 10) / 10,
        totalMs: Math.round((record.boardNavigation.durationMs + record.submitToUrlMs) * 10) / 10,
      });
    }
    for (const sample of samples) {
      expect(sample.threadId).toBeGreaterThan(0);
      expect(sample.boardNavigation.duplicateRequests).toEqual([]);
    }
    await attachEvidence(testInfo, 'audit-performance-thread-creation.json', {
      scenario: 'board load → fill → submit → visible post',
      project: testInfo.project.name,
      browserVersion: browser.version(),
      samples,
      summaries: {
        submitToVisiblePostMs: summarize(samples.map((sample) => sample.submitToUrlMs)),
        boardLoadMs: summarize(samples.map((sample) => sample.boardNavigation.durationMs)),
      },
      note: 'submit includes redirect + server render + post content committed; assertions cover the outcome, not the duration',
    });
  });

  test('a thread page with a real uploaded image decodes the image', async ({ page, app, browser }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('perf', testInfo);
    app.createBoardCli({ short: board, name: 'Performance Media', description: 'media performance audit' });
    setBoardFixtureSettings(app, board, { allowImages: true, postCooldownSecs: 0 });
    // The upload happens through the real UI; it warms the context for the
    // thread page before the measured loop, which the attachment records.
    const threadId = await createThread(page, app, board, {
      subject: 'performance media thread',
      body: 'performance media body',
      filePath: app.fixtures().tinyPng,
    });
    const threadUrl = `${app.baseURL}/${board}/thread/${threadId}`;

    type BoardAssetRecord = { url: string; contentLength: number | null; responseBodyBytes: number | null };
    type MediaSample = {
      sample: number;
      state: string;
      navigation: NavigationSample;
      naturalWidth: number;
      naturalHeight: number;
      imageComplete: boolean;
      imageResourceTiming: { name: string; durationMs: number; transferSize: number } | null;
      imageResourceTimingNote: string | null;
      boardAssets: BoardAssetRecord[];
    };
    const samples: MediaSample[] = [];
    for (let sample = 1; sample <= SAMPLES; sample += 1) {
      const state = sample === 1 ? 'cold' : 'warm';
      const record = await measured(`media thread page sample ${sample} (${state})`, async () => {
        const boardAssets: BoardAssetRecord[] = [];
        const pending: Array<Promise<void>> = [];
        const listener = (response: Response) => {
          if (!response.url().includes('/boards/')) return;
          const contentLengthRaw = response.headers()['content-length'];
          const asset: BoardAssetRecord = {
            url: response.url(),
            contentLength: contentLengthRaw === undefined ? null : Number(contentLengthRaw),
            responseBodyBytes: null,
          };
          boardAssets.push(asset);
          pending.push((async () => {
            try {
              const sizes = await response.request().sizes();
              if (sizes.responseBodySize >= 0) asset.responseBodyBytes = sizes.responseBodySize;
            } catch (error) {
              // Engine may not expose sizes for this response; recorded as null.
            }
          })());
        };
        page.on('response', listener);
        const navigation = await measureNavigation(page, threadUrl, `media thread sample ${sample} (${state})`);
        await expect(page.locator('.file-container').first()).toBeVisible();
        const thumb = page.locator('.file-container img.thumb').first();
        await thumb.scrollIntoViewIfNeeded();
        await expect(thumb).toBeVisible();
        await expect
          .poll(
            () => thumb.evaluate((element) => (element as HTMLImageElement).naturalWidth),
            { message: 'uploaded image should decode (naturalWidth > 0)' },
          )
          .toBeGreaterThan(0);
        const decoded = await thumb.evaluate((element) => {
          const image = element as HTMLImageElement;
          return {
            naturalWidth: image.naturalWidth,
            naturalHeight: image.naturalHeight,
            complete: image.complete,
          };
        });
        const resourceTiming = await page.evaluate((needle) => {
          if (typeof performance.getEntriesByType !== 'function') {
            return { entry: null, note: 'Resource Timing is not exposed by this engine' };
          }
          const entries = performance.getEntriesByType('resource').filter((entry) => entry.name.includes(needle));
          const entry = entries[entries.length - 1] as PerformanceResourceTiming | undefined;
          if (!entry) {
            return { entry: null, note: 'no resource timing entry recorded for the image' };
          }
          return {
            entry: { name: entry.name, durationMs: entry.duration, transferSize: entry.transferSize },
            note: null,
          };
        }, '/boards/');
        page.off('response', listener);
        await Promise.allSettled(pending);
        return {
          sample,
          state,
          navigation,
          naturalWidth: decoded.naturalWidth,
          naturalHeight: decoded.naturalHeight,
          imageComplete: decoded.complete,
          imageResourceTiming: resourceTiming.entry,
          imageResourceTimingNote: resourceTiming.note,
          boardAssets,
        };
      });
      samples.push(record);
    }
    for (const sample of samples) {
      expect(sample.naturalWidth, 'uploaded image should decode in every sample').toBeGreaterThan(0);
      expect(sample.navigation.duplicateRequests).toEqual([]);
    }
    await attachEvidence(testInfo, 'audit-performance-media.json', {
      scenario: 'thread page with a real uploaded PNG (thumbnail decode)',
      project: testInfo.project.name,
      browserVersion: browser.version(),
      samples,
      summaries: {
        navigationMs: summarize(samples.map((sample) => sample.navigation.durationMs)),
      },
      coldWarm: 'the upload flow already visited this context before sample 1, so "cold" here labels the first measured sample; sample 2+ are warm',
      note: 'image decode is asserted via naturalWidth > 0; resource timing is recorded when the engine exposes it',
    });
  });

  test('admin panel pages load for a logged-in operator', async ({ page, app, browser }, testInfo) => {
    actor('admin');
    await loginViaRequest(page, app.baseURL);
    const samples: NavigationSample[] = [];
    for (let sample = 1; sample <= SAMPLES; sample += 1) {
      const state = sample === 1 ? 'cold' : 'warm';
      samples.push(await measured(`admin panel sample ${sample} (${state})`, async () => measureNavigation(
        page,
        `${app.baseURL}/admin/panel`,
        `admin panel sample ${sample} (${state})`,
      )));
      await expect(page.getByRole('heading', { name: /admin panel/i })).toBeVisible();
    }
    for (const sample of samples) {
      expect(sample.duplicateRequests, `${sample.label} should not fetch the same resource twice`).toEqual([]);
    }

    const modLogSamples: NavigationSample[] = [];
    for (let sample = 1; sample <= SAMPLES; sample += 1) {
      const state = sample === 1 ? 'cold' : 'warm';
      modLogSamples.push(await measured(`mod log sample ${sample} (${state})`, async () => measureNavigation(
        page,
        `${app.baseURL}/admin/mod-log`,
        `mod log sample ${sample} (${state})`,
      )));
      await expect(page.getByRole('heading', { name: /moderation log/i })).toBeVisible();
    }
    for (const sample of modLogSamples) {
      expect(sample.duplicateRequests, `${sample.label} should not fetch the same resource twice`).toEqual([]);
    }

    await attachEvidence(testInfo, 'audit-performance-admin.json', {
      scenario: 'admin panel and moderation log navigation (request-API login, logged-in admin)',
      project: testInfo.project.name,
      browserVersion: browser.version(),
      samples,
      modLogSamples,
      summaries: {
        adminPanelMs: summarize(samples.map((sample) => sample.durationMs)),
        modLogMs: summarize(modLogSamples.map((sample) => sample.durationMs)),
      },
      coldWarm: 'login used the request API (no page navigation), so sample 1 starts with an untouched page cache; samples 2-5 are warm',
    });
  });

  test('one navigation does not fetch the same resource twice', async ({ page, app }, testInfo) => {
    actor('admin');
    const board = uniqueShort('perf', testInfo);
    app.createBoardCli({ short: board, name: 'Performance Duplicates', description: 'duplicate request audit' });
    setBoardFixtureSettings(app, board, { allowImages: true, postCooldownSecs: 0 });
    const threadId = await createThreadViaRequest(page, app, board, {
      subject: 'duplicate request thread',
      body: 'duplicate request body',
    });
    await loginViaRequest(page, app.baseURL);

    const targets = [
      { name: 'home', url: app.baseURL },
      { name: 'board index', url: `${app.baseURL}/${board}` },
      { name: 'catalog', url: `${app.baseURL}/${board}/catalog` },
      { name: 'thread', url: `${app.baseURL}/${board}/thread/${threadId}` },
      { name: 'admin panel', url: `${app.baseURL}/admin/panel` },
      { name: 'mod log', url: `${app.baseURL}/admin/mod-log` },
    ];
    const results: Array<{ target: string; repeat: number; durationMs: number; requestCount: number }> = [];
    for (const target of targets) {
      for (let repeat = 1; repeat <= 2; repeat += 1) {
        const sample = await measured(`duplicate check ${target.name} repeat ${repeat}`, async () => measureNavigation(
          page,
          target.url,
          `${target.name} repeat ${repeat}`,
        ));
        expect(
          sample.duplicateRequests,
          `${target.name} repeat ${repeat} should not fetch the same resource twice`,
        ).toEqual([]);
        results.push({
          target: target.name,
          repeat,
          durationMs: sample.durationMs,
          requestCount: sample.requestCount,
        });
      }
    }
    await attachEvidence(testInfo, 'audit-performance-duplicates.json', {
      scenario: 'per-navigation duplicate resource check',
      project: testInfo.project.name,
      excludedRepeatEndpoints: REPEATING_ENDPOINT_PATTERNS.map((pattern) => String(pattern)),
      results,
      note: 'duplicates are compared by method + absolute URL within a single navigation; polling endpoints are excluded and labelled',
    });
  });

  test('large and repeatedly downloaded assets are reported', async ({ page, app }, testInfo) => {
    actor('admin');
    const board = uniqueShort('perf', testInfo);
    app.createBoardCli({ short: board, name: 'Performance Sizes', description: 'asset size audit' });
    setBoardFixtureSettings(app, board, { allowImages: true, postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, {
      subject: 'asset size thread',
      body: 'asset size body',
      filePath: app.fixtures().tinyPng,
    });
    await loginViaRequest(page, app.baseURL);

    type AssetSizeRecord = {
      url: string;
      method: string;
      status: number;
      responseBodyBytes: number | null;
      responseHeaderBytes: number | null;
      contentLengthHeaderBytes: number | null;
      resourceType: string;
    };

    async function recordPageSizes(url: string, label: string): Promise<{ label: string; assets: AssetSizeRecord[] }> {
      const pending: Array<Promise<void>> = [];
      const assets: AssetSizeRecord[] = [];
      const listener = (response: Response) => {
        pending.push((async () => {
          const request = response.request();
          let responseBodyBytes: number | null = null;
          let responseHeaderBytes: number | null = null;
          try {
            const sizes = await request.sizes();
            responseBodyBytes = sizes.responseBodySize >= 0 ? sizes.responseBodySize : null;
            responseHeaderBytes = sizes.responseHeadersSize >= 0 ? sizes.responseHeadersSize : null;
          } catch (error) {
            // Engine-dependent; null means "not reported", never zero.
          }
          const contentLengthRaw = response.headers()['content-length'];
          const contentLength = contentLengthRaw === undefined ? Number.NaN : Number(contentLengthRaw);
          assets.push({
            url: response.url(),
            method: request.method(),
            status: response.status(),
            responseBodyBytes,
            responseHeaderBytes,
            contentLengthHeaderBytes: Number.isFinite(contentLength) ? contentLength : null,
            resourceType: request.resourceType(),
          });
        })());
      };
      page.on('response', listener);
      await page.goto(url, { waitUntil: 'load' });
      page.off('response', listener);
      await Promise.allSettled(pending);
      return { label, assets };
    }

    const pages = [
      await recordPageSizes(app.baseURL, 'home'),
      await recordPageSizes(`${app.baseURL}/${board}`, 'board index'),
      await recordPageSizes(`${app.baseURL}/${board}/thread/${threadId}`, 'thread with image'),
      await recordPageSizes(`${app.baseURL}/admin/panel`, 'admin panel'),
    ];
    const report = pages.map((entry) => ({
      label: entry.label,
      assetCount: entry.assets.length,
      topByResponseBody: [...entry.assets]
        .sort((left, right) => (right.responseBodyBytes ?? -1) - (left.responseBodyBytes ?? -1))
        .slice(0, 10),
    }));
    await attachEvidence(testInfo, 'audit-performance-asset-sizes.json', {
      scenario: 'transfer size inventory (no size threshold asserted)',
      project: testInfo.project.name,
      pages: report,
      note: 'sizes come from request.sizes(); -1 (engine-dependent, e.g. some WebKit responses) is recorded as null, never as zero. '
        + 'No failure is raised on size because the application does not document a byte budget for these pages.',
    });
    const totalBytes = report.reduce(
      (sum, entry) => sum + entry.topByResponseBody
        .reduce((inner, asset) => inner + Math.max(0, asset.responseBodyBytes ?? 0), 0),
      0,
    );
    expect(totalBytes, 'asset size evidence should be collected').toBeGreaterThan(0);
  });

  test('server CPU and RSS samples are observed around a navigation workflow', async ({ page, app }, testInfo) => {
    actor('visitor');
    const board = uniqueShort('perf', testInfo);
    app.createBoardCli({ short: board, name: 'Performance Server', description: 'server observation audit' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
    const threadId = await createThreadViaRequest(page, app, board, {
      subject: 'server observation thread',
      body: 'server observation body',
    });
    const pid = app.process?.pid ?? 0;
    expect(pid, 'RustChan fixture process should expose a pid').toBeGreaterThan(0);

    type ProcessSample = { at: string; sample: number; cpuPercent: number; rssKb: number };
    type ProcessSampleResult = ProcessSample | { at: string; sample: number; error: string };

    function sampleProcess(index: number): ProcessSampleResult {
      const result = spawnSync('ps', ['-o', 'pid=,%cpu=,rss=', '-p', String(pid)], { encoding: 'utf8' });
      const at = new Date().toISOString();
      if (result.status !== 0) {
        return { at, sample: index, error: result.stderr.trim() || `ps exited with ${result.status}` };
      }
      const parts = result.stdout.trim().split(/\s+/);
      const cpuPercent = Number(parts[1]);
      const rssKb = Number(parts[2]);
      if (!Number.isFinite(cpuPercent) || !Number.isFinite(rssKb)) {
        return { at, sample: index, error: `unparseable ps output: ${result.stdout.trim()}` };
      }
      return { at, sample: index, cpuPercent, rssKb };
    }

    function isProcessAlive(): boolean {
      try {
        process.kill(pid, 0);
        return true;
      } catch (error) {
        return false;
      }
    }

    expect(isProcessAlive(), 'RustChan fixture process should be alive before observation').toBe(true);
    const samples: ProcessSampleResult[] = [sampleProcess(0)];
    for (let sample = 1; sample <= SAMPLES; sample += 1) {
      await measured(`server observation navigation ${sample}`, async () => {
        await page.goto(app.baseURL, { waitUntil: 'load' });
        await page.goto(`${app.baseURL}/${board}`, { waitUntil: 'load' });
        await page.goto(`${app.baseURL}/${board}/thread/${threadId}`, { waitUntil: 'load' });
      });
      samples.push(sampleProcess(sample));
    }
    expect(isProcessAlive(), 'RustChan fixture process should still be alive after observation').toBe(true);
    const usable = samples.filter((entry): entry is ProcessSample => !('error' in entry));
    expect(usable.length, 'ps should yield usable CPU/RSS samples').toBeGreaterThanOrEqual(SAMPLES);
    for (const entry of usable) {
      expect(entry.rssKb, 'RSS samples should be positive').toBeGreaterThan(0);
    }
    await attachEvidence(testInfo, 'audit-performance-server.json', {
      scenario: 'server CPU/RSS sampled around 5 home → board → thread iterations',
      project: testInfo.project.name,
      pid,
      sampleCount: samples.length,
      samples,
      deltas: usable.length >= 2
        ? {
          rssKb: usable[usable.length - 1].rssKb - usable[0].rssKb,
          cpuPercentRange: [
            Math.min(...usable.map((entry) => entry.cpuPercent)),
            Math.max(...usable.map((entry) => entry.cpuPercent)),
          ],
        }
        : null,
      limitation: 'A single RSS increase is not evidence of a memory leak, and a decrease is not evidence of a fix: '
        + '5 iterations cannot characterize memory behaviour. ps %cpu is an interval average, not instantaneous CPU. '
        + 'These samples are bounded observations only and nothing here asserts a leak or a CPU budget.',
    });
  });
});
