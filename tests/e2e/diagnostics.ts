import {
  test as base,
  expect,
  type Browser,
  type BrowserContext,
  type BrowserContextOptions,
  type Page,
  type TestInfo,
  type WebError,
} from '@playwright/test';

export * from '@playwright/test';

/**
 * Hardened browser diagnostics for the RustChan E2E suite.
 *
 * Contract (documented in tests/e2e/README.md § Diagnostics):
 *  - Uncaught page errors fail the test unless declared with expectPageError.
 *  - Console errors fail the test unless declared with expectConsoleError.
 *  - HTTP 4xx/5xx responses from page traffic fail the test unless declared with
 *    expectHttpError. `page.request.*` traffic is out of scope by construction:
 *    Playwright does not emit context response events for APIRequestContext
 *    responses, so API-driven negative tests need no allowlist.
 *  - Transport failures fail the test unless declared with
 *    expectTransportFailure. Navigation-cancelled requests (net::ERR_ABORTED)
 *    are counted and reported but are not failures, because navigating away or
 *    closing a page cancels in-flight requests by design.
 *  - Declarations are per test and must name a kind plus method/path/status or a
 *    message pattern. Declarations are consumed by observed events. A
 *    declaration that never matches fails the test, so expectations cannot rot
 *    into a blanket allowlist.
 *  - All records carry actor, project, test, and timestamp, plus x-request-id
 *    and Location where the server provided them, so browser evidence can be
 *    joined with server-side artifacts.
 */

export type DiagnosticKind =
  | 'page-error'
  | 'console-error'
  | 'console-warning'
  | 'transport-failure'
  | 'request-aborted'
  | 'http-error'
  | 'http-3xx'
  | 'asset-error'
  | 'note';

export type DiagnosticRecord = {
  ts: string;
  kind: DiagnosticKind;
  actor: string;
  project: string;
  test: string;
  worker: number;
  method?: string;
  path?: string;
  url?: string;
  status?: number;
  contentType?: string;
  location?: string;
  requestId?: string;
  detail?: string;
};

export type ErrorExpectation = {
  kind: 'http-error' | 'console-error' | 'transport-failure' | 'page-error';
  method?: string | RegExp;
  path?: string | RegExp;
  status?: number | readonly number[];
  pattern?: string | RegExp;
  url?: string | RegExp;
  optional?: boolean;
  /**
   * How many observations this declaration may consume. Defaults to 1. Use a
   * number for a bounded retry loop, or 'any' for a legitimate repeated surface
   * such as "every theme rendered the missing-board 404 page". 'any' must still
   * name an exact method/path/status, so it cannot become a blanket allowlist.
   */
  times?: number | 'any';
  reason: string;
  actor?: string;
};

export type TimingRecord = {
  name: string;
  actor: string;
  project: string;
  test: string;
  durationMs: number;
  ok: boolean;
};

type Diagnostics = {
  records: DiagnosticRecord[];
  pageErrors: string[];
  serverErrors: string[];
  expectations: ErrorExpectation[];
  timing: TimingRecord[];
  contexts: Set<BrowserContext>;
  actor: string;
  actors: Set<string>;
  testLabel: string;
  project: string;
  worker: number;
  failures: string[];
};

const activeDiagnostics = new WeakMap<TestInfo, Diagnostics>();

const DEEP = process.env.RUSTCHAN_E2E_DEEP_DIAGNOSTICS === '1';
const TIMING = process.env.RUSTCHAN_E2E_TIMING === '1' || DEEP;
const MAX_DEEP_ARTIFACTS = Number(process.env.RUSTCHAN_E2E_DEEP_DIAGNOSTICS_MAX ?? '400');
let deepArtifacts = 0;

/**
 * Project names configured with JavaScript disabled in playwright.config.ts.
 * A spec that disables JavaScript locally with `test.use({ javaScriptEnabled:
 * false })` is not visible here: Playwright does not expose describe-level `use`
 * overrides to fixtures. Such specs already pass `javaScriptEnabled: false`
 * explicitly when they create a context (see firefox-nojs-public.spec.ts,
 * captcha.spec.ts, activity-notifications.spec.ts, theme-history.spec.ts).
 * Any new spec that disables JavaScript locally MUST do the same; that
 * requirement is documented in tests/e2e/README.md and checked by
 * audit-harness-contract.spec.ts for the firefox-nojs project.
 */
export const NO_JS_PROJECTS = new Set(['firefox-nojs', 'chromium-nojs']);

/** True when a manually created context must enable JavaScript for this project. */
export function javaScriptEnabledFor(testInfo: TestInfo): boolean {
  return !NO_JS_PROJECTS.has(testInfo.project.name);
}

function now(): string {
  return new Date().toISOString();
}

/** Remove credentials, session cookie values, and CSRF tokens from exported text. */
export function redact(text: string): string {
  return text
    .replace(/(_csrf|csrf_token)(["'\s=:]|&quot;)+([^"'&\s;]+)/gi, '$1$2[redacted]')
    .replace(/(set-cookie:\s*[^=\n;]+=)[^;\n]+/gi, '$1[redacted]')
    .replace(/\b(rustchan_admin_session|rustchan_visitor_id|deletion_token|rustchan_owned_posts|rustchan_access_[A-Za-z0-9_]+)=[^;,\s"']+/g, '$1=[redacted]')
    .replace(/"((?:access_password|password_hash|cookie_secret|admin_password))"\s*:\s*"[^"]*"/gi, '"$1":"[redacted]"')
    .replace(/(--password[= ]+)\S+/g, '$1[redacted]');
}

function current(): Diagnostics {
  const diagnostics = activeDiagnostics.get(base.info());
  if (!diagnostics) {
    throw new Error('Browser diagnostics are only available inside a test body');
  }
  return diagnostics;
}

function record(partial: Omit<DiagnosticRecord, 'ts' | 'actor' | 'project' | 'test' | 'worker'>): void {
  const diagnostics = activeDiagnostics.get(base.info());
  if (!diagnostics) return;
  diagnostics.records.push({
    ts: now(),
    actor: diagnostics.actor,
    project: diagnostics.project,
    test: diagnostics.testLabel,
    worker: diagnostics.worker,
    ...partial,
  });
}

function describeRequest(request: { method(): string; url(): string }): { method: string; path: string; url: string } {
  const url = request.url();
  let path = url;
  try {
    const parsed = new URL(url);
    path = `${parsed.pathname}${parsed.search}`;
  } catch {
    // data:/blob:/about: URLs are recorded verbatim.
  }
  return { method: request.method(), path, url };
}

const ASSET_CONTENT_TYPE_EXPECTATIONS: Array<{ match: (path: string) => boolean; type: string }> = [
  { match: path => path.startsWith('/static/') && path.split('?')[0].endsWith('.css'), type: 'text/css' },
  { match: path => path.startsWith('/static/') && path.split('?')[0].endsWith('.js'), type: 'javascript' },
  { match: path => path.startsWith('/theme-css/'), type: 'text/css' },
];

/**
 * Markers for a request cancelled by navigation or teardown rather than by a
 * transport problem. Each engine reports this differently: Chromium
 * `net::ERR_ABORTED`, WebKit `cancelled`, Firefox `NS_BINDING_ABORTED`.
 */
const NAVIGATION_CANCEL_MARKERS = [
  'ERR_ABORTED',           // Chromium
  'cancelled',             // WebKit
  'Frame load interrupted', // WebKit, e.g. an embedded PDF frame replaced by navigation
  'NS_BINDING_ABORTED',    // Firefox
];

/** True when a request-failure text means "cancelled", not "transport problem". */
export function isNavigationCancel(failureText: string): boolean {
  const normalised = failureText.toLowerCase();
  return NAVIGATION_CANCEL_MARKERS.some(marker => normalised.includes(marker.toLowerCase()));
}

function observeContext(context: BrowserContext, diagnostics: Diagnostics): void {
  if (diagnostics.contexts.has(context)) return;
  diagnostics.contexts.add(context);
  context.on('weberror', (error: WebError) => {
    const message = error.error().message;
    diagnostics.pageErrors.push(message);
    record({ kind: 'page-error', url: error.page()?.url(), detail: message });
  });
  context.on('console', message => {
    const kind = message.type() === 'error' ? 'console-error' : message.type() === 'warning' ? 'console-warning' : null;
    if (!kind) return;
    record({ kind, url: message.location().url, detail: message.text() });
  });
  context.on('requestfailed', request => {
    const failure = request.failure()?.errorText ?? 'unknown failure';
    const { method, path, url } = describeRequest(request);
    record({
      kind: isNavigationCancel(failure) ? 'request-aborted' : 'transport-failure',
      method,
      path,
      url,
      detail: failure,
    });
  });
  context.on('response', response => {
    const request = response.request();
    const { method, path, url } = describeRequest(request);
    const status = response.status();
    const headers = response.headers();
    const contentType = headers['content-type'];
    const location = headers.location;
    const requestId = headers['x-request-id'];
    if (status >= 500) {
      diagnostics.serverErrors.push(`${status} ${method} ${path}`);
    }
    if (status >= 400) {
      record({ kind: 'http-error', method, path, url, status, contentType, location, requestId });
      return;
    }
    if (status >= 300 && status !== 304) {
      record({ kind: 'http-3xx', method, path, url, status, location, requestId });
      return;
    }
    if (status === 200 && contentType) {
      const expected = ASSET_CONTENT_TYPE_EXPECTATIONS.find(entry => entry.match(path));
      if (expected && !contentType.includes(expected.type)) {
        record({
          kind: 'asset-error',
          method,
          path,
          url,
          status,
          contentType,
          detail: `expected content-type containing ${expected.type}, received ${contentType}`,
        });
      }
    }
  });
}

/** Close pages owned by a disposable runtime before its listener stops. */
export async function closeAuditedPagesForOrigin(origin: string): Promise<void> {
  // Helpers are also used by local scripts outside a Playwright test body.
  let info: TestInfo;
  try { info = base.info(); } catch { return; }
  const diagnostics = activeDiagnostics.get(info);
  if (!diagnostics) return;
  const pages = [...diagnostics.contexts].flatMap(context => context.pages())
    .filter(page => !page.isClosed() && page.url().startsWith(origin + '/'));
  await Promise.all(pages.map(page => page.close()));
}

/** Create a diagnostics-observed context that inherits the project JavaScript mode. */
export async function newAuditedContext(browser: Browser, options?: BrowserContextOptions): Promise<BrowserContext> {
  const context = await browser.newContext({
    javaScriptEnabled: javaScriptEnabledFor(base.info()),
    ...options,
  });
  observeContext(context, current());
  return context;
}

/** Create a diagnostics-observed page whose context inherits the project JavaScript mode. */
export async function newAuditedPage(browser: Browser) {
  const page = await browser.newPage({ javaScriptEnabled: javaScriptEnabledFor(base.info()) });
  observeContext(page.context(), current());
  return page;
}

/** Label subsequent events with the acting user/role for evidence correlation. */
export function actor(label: string): void {
  const diagnostics = current();
  diagnostics.actor = label;
  diagnostics.actors.add(label);
}

/** Record a redacted evidence note that is attached to the report but never fails a test. */
export function noteEvent(kind: string, detail: string): void {
  record({ kind: 'note', detail: `${kind}: ${redact(detail)}` });
}

export function diagnosticRecords(): readonly DiagnosticRecord[] {
  return current().records;
}

export function diagnosticTimings(): readonly TimingRecord[] {
  return current().timing;
}

function matchesStringOrRegExp(value: string | undefined, pattern: string | RegExp | undefined): boolean {
  if (pattern === undefined) return true;
  if (value === undefined) return false;
  if (pattern instanceof RegExp) return pattern.test(value);
  return value === pattern || value.startsWith(pattern);
}

function matchStatus(status: number, expected: number | readonly number[] | undefined): boolean {
  if (expected === undefined) return true;
  if (Array.isArray(expected)) return (expected as readonly number[]).includes(status);
  return status === (expected as number);
}

function matchesExpectation(expectation: ErrorExpectation, value: DiagnosticRecord): boolean {
  switch (expectation.kind) {
    case 'http-error':
      return value.kind === 'http-error'
        && matchStatus(value.status ?? 0, expectation.status)
        && matchesStringOrRegExp(value.method, expectation.method)
        && matchesStringOrRegExp(value.path, expectation.path);
    case 'console-error':
      return value.kind === 'console-error'
        && matchesStringOrRegExp(value.detail, expectation.pattern)
        && matchesStringOrRegExp(value.url, expectation.url);
    case 'page-error':
      return value.kind === 'page-error'
        && matchesStringOrRegExp(value.detail, expectation.pattern);
    case 'transport-failure':
      return value.kind === 'transport-failure'
        && matchesStringOrRegExp(value.url, expectation.url)
        && matchesStringOrRegExp(value.detail, expectation.pattern);
  }
}

function declare(expectations: ErrorExpectation[]): void {
  current().expectations.push(...expectations);
}

/** Declare one expected HTTP error response produced by page traffic. */
export function expectHttpError(expectation: Omit<Extract<ErrorExpectation, { kind: 'http-error' }>, 'kind'>): void {
  declare([{ ...expectation, kind: 'http-error' }]);
}

/** Declare one expected console error message. */
export function expectConsoleError(expectation: Omit<Extract<ErrorExpectation, { kind: 'console-error' }>, 'kind'>): void {
  declare([{ ...expectation, kind: 'console-error' }]);
}

/** Declare one expected uncaught page error. */
export function expectPageError(expectation: Omit<Extract<ErrorExpectation, { kind: 'page-error' }>, 'kind'>): void {
  declare([{ ...expectation, kind: 'page-error' }]);
}

/** Declare one expected transport failure (DNS, refusal, TLS, timeout). */
export function expectTransportFailure(expectation: Omit<Extract<ErrorExpectation, { kind: 'transport-failure' }>, 'kind'>): void {
  declare([{ ...expectation, kind: 'transport-failure' }]);
}

/** Fail the current test with a redacted diagnostic message. Usable from helpers. */
export function reportDiagnosticFailure(message: string): void {
  current().failures.push(redact(message));
}

/**
 * Measure an action and the point at which its visible result is asserted. The
 * caller owns the assertion so a slow-but-wrong result still fails the test.
 */
export async function measureStep<T>(name: string, action: () => Promise<T>): Promise<T> {
  const started = Date.now();
  let ok = true;
  try {
    return await action();
  } catch (error) {
    ok = false;
    throw error;
  } finally {
    if (TIMING) {
      const diagnostics = activeDiagnostics.get(base.info());
      diagnostics?.timing.push({
        name,
        actor: diagnostics.actor,
        project: diagnostics.project,
        test: diagnostics.testLabel,
        durationMs: Date.now() - started,
        ok,
      });
    }
  }
}

export const DEEP_JOURNEY = 'deep-journey';

/**
 * Assert the page is renderable, then (only in deep mode or for tests annotated
 * `deep-journey`) attach a bounded screenshot of the successful step.
 */
export async function journeyStep(testInfo: TestInfo, page: Page, name: string): Promise<void> {
  await measureStep(`step:${name}`, async () => {
    await expect(page.locator('body')).toBeVisible();
  });
  const deep = DEEP || testInfo.annotations.some(annotation => annotation.type === DEEP_JOURNEY);
  if (!deep || deepArtifacts >= MAX_DEEP_ARTIFACTS) return;
  deepArtifacts += 1;
  await testInfo.attach(`journey-${String(deepArtifacts).padStart(3, '0')}-${name}.png`, {
    body: await page.screenshot(),
    contentType: 'image/png',
  });
}

/**
 * Pure declaration matcher, exported so `audit-harness-contract.spec.ts` can
 * verify the strictness rules directly instead of relying on teardown ordering.
 * Declarations are consumed by observed events in observation order; each
 * declaration matches at most one event.
 */
export function declarationStatus(
  records: readonly DiagnosticRecord[],
  expectations: readonly ErrorExpectation[],
): {
  consumed: number[];
  matchedRecords: number[];
  unmatched: number[];
  pairs: Array<[expectationIndex: number, recordIndex: number]>;
} {
  const consumed = new Set<number>();
  const matchedRecords = new Set<number>();
  const pairs: Array<[number, number]> = [];
  const remaining = expectations.map(expectation => (
    expectation.times === 'any' ? Number.POSITIVE_INFINITY : Math.max(1, expectation.times ?? 1)
  ));
  for (const [recordIndex, value] of records.entries()) {
    const expectationIndex = expectations.findIndex(
      (expectation, index) => remaining[index] > 0 && matchesExpectation(expectation, value),
    );
    if (expectationIndex >= 0) {
      remaining[expectationIndex] -= 1;
      consumed.add(expectationIndex);
      matchedRecords.add(recordIndex);
      pairs.push([expectationIndex, recordIndex]);
    }
  }
  return {
    consumed: [...consumed].sort((a, b) => a - b),
    matchedRecords: [...matchedRecords].sort((a, b) => a - b),
    pairs: pairs.sort((a, b) => a[0] - b[0]),
    unmatched: expectations
      .map((expectation, index) => (consumed.has(index) ? -1 : index))
      .filter(index => index >= 0),
  };
}

/**
 * Console messages that are a direct consequence of an already-declared HTTP
 * error or transport failure. Chromium logs
 * "Failed to load resource: the server responded with a status of 404" for a 404
 * *document* navigation as well as for subresources, so requiring a second
 * console declaration for the same response would be noise rather than
 * strictness. Derivation is exact: the console message's URL or text must
 * literally contain the declared record's URL or path. An undeclared 4xx or
 * transport failure still fails the test, and its console message with it.
 */
function consoleMessagesExplainedByDeclaredRecords(
  diagnostics: Diagnostics,
  pairs: ReadonlyArray<[number, number]>,
  matchedRecords: ReadonlySet<number>,
): Set<number> {
  const explained = new Set<number>();
  for (const [expectationIndex, recordIndex] of pairs) {
    const kind = diagnostics.expectations[expectationIndex].kind;
    if (kind !== 'http-error' && kind !== 'transport-failure') continue;
    const declared = diagnostics.records[recordIndex];
    const keys = [declared.url, declared.path].filter((key): key is string => typeof key === 'string' && key.length > 0);
    if (keys.length === 0) continue;
    diagnostics.records.forEach((candidate, index) => {
      if (candidate.kind !== 'console-error' || matchedRecords.has(index) || explained.has(index)) return;
      const haystacks = [candidate.url, candidate.detail].filter((value): value is string => typeof value === 'string');
      if (keys.some(key => haystacks.some(value => value.includes(key)))) explained.add(index);
    });
  }
  return explained;
}

/**
 * True when a console message the *current* test already declared or that is a
 * direct consequence of a declared record. `phase4-helpers.watchClientErrors`
 * uses this so a page-level "no client errors" check does not re-report a
 * surface the test has deliberately exercised.
 */
export function consoleErrorIsDeclared(text: string, url?: string): boolean {
  const diagnostics = activeDiagnostics.get(base.info());
  if (!diagnostics) return false;
  const status = declarationStatus(diagnostics.records, diagnostics.expectations);
  const matched = new Set(status.matchedRecords);
  const explained = consoleMessagesExplainedByDeclaredRecords(diagnostics, status.pairs, matched);
  const haystacks = [url, text].filter((value): value is string => typeof value === 'string' && value.length > 0);
  return diagnostics.records.some((record, index) => {
    if (record.kind !== 'console-error') return false;
    if (matched.has(index) || explained.has(index)) {
      return haystacks.some(value => value === record.detail || value.includes(record.url ?? '\u0000'));
    }
    return false;
  });
}

function formatRecord(value: DiagnosticRecord): string {
  const where = [value.method, value.path ?? value.url].filter(Boolean).join(' ');
  const suffix = [
    value.status === undefined ? '' : String(value.status),
    value.contentType === undefined ? '' : `ct=${value.contentType}`,
    value.location === undefined ? '' : `-> ${value.location}`,
    value.requestId === undefined ? '' : `req=${value.requestId}`,
  ].filter(Boolean).join(' ');
  return `${value.kind} [${value.actor}] ${where} ${suffix} ${value.detail ?? ''}`.replace(/\s+/g, ' ').trim();
}

function summarise(diagnostics: Diagnostics): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const value of diagnostics.records) {
    counts[value.kind] = (counts[value.kind] ?? 0) + 1;
  }
  return counts;
}

export const test = base.extend<{ browserDiagnostics: void }>({
  browserDiagnostics: [async ({ context }, use, testInfo) => {
    const diagnostics: Diagnostics = {
      records: [],
      pageErrors: [],
      serverErrors: [],
      expectations: [],
      timing: [],
      contexts: new Set<BrowserContext>(),
      actor: 'anonymous',
      actors: new Set<string>(['anonymous']),
      testLabel: testInfo.titlePath.filter(part => !part.includes('.spec.')).join(' › '),
      project: testInfo.project.name,
      worker: testInfo.workerIndex,
      failures: [],
    };
    activeDiagnostics.set(testInfo, diagnostics);
    observeContext(context, diagnostics);
    await use();
    activeDiagnostics.delete(testInfo);

    // Consume declarations in observation order; each declaration matches at most one event.
    const status = declarationStatus(diagnostics.records, diagnostics.expectations);
    const matchedRecords = new Set(status.matchedRecords);
    const explainedConsole = consoleMessagesExplainedByDeclaredRecords(diagnostics, status.pairs, matchedRecords);

    const aborted = diagnostics.records.filter(value => value.kind === 'request-aborted');
    const legacyText = diagnostics.records.map(formatRecord).join('\n');

    const shouldAttach = diagnostics.records.length > 0
      || diagnostics.failures.length > 0
      || testInfo.status !== testInfo.expectedStatus;
    if (shouldAttach) {
      await testInfo.attach('browser-diagnostics.json', {
        body: JSON.stringify({
          project: diagnostics.project,
          test: diagnostics.testLabel,
          worker: diagnostics.worker,
          actors: [...diagnostics.actors],
          counts: summarise(diagnostics),
          explainedConsoleMessages: [...explainedConsole].map(index => diagnostics.records[index]?.detail),
          requestIds: diagnostics.records.map(value => value.requestId).filter(Boolean),
          expectations: diagnostics.expectations,
          records: diagnostics.records.map(value => ({
            ...value,
            detail: value.detail === undefined ? undefined : redact(value.detail),
          })),
        }, null, 2),
        contentType: 'application/json',
      });
      await testInfo.attach('browser-diagnostics.txt', {
        body: redact(legacyText) || 'No browser errors recorded',
        contentType: 'text/plain',
      });
    }
    if (TIMING && diagnostics.timing.length > 0) {
      await testInfo.attach('timings.json', {
        body: JSON.stringify(diagnostics.timing, null, 2),
        contentType: 'application/json',
      });
    }

    const failures: string[] = [...diagnostics.failures];
    const unmatchedExpectations = status.unmatched
      .map(index => diagnostics.expectations[index])
      .filter(expectation => expectation.optional !== true);

    const unexpected = diagnostics.records.filter((value, index) => (
      (value.kind === 'http-error' || value.kind === 'asset-error') && !matchedRecords.has(index)
    ));
    const unexpectedTransport = diagnostics.records.filter((value, index) => (
      value.kind === 'transport-failure' && !matchedRecords.has(index)
    ));
    const unexpectedConsole = diagnostics.records.filter((value, index) => (
      value.kind === 'console-error' && !matchedRecords.has(index) && !explainedConsole.has(index)
    ));
    const unexpectedPageErrors = diagnostics.pageErrors.filter(message => (
      !diagnostics.records.some((value, index) => value.kind === 'page-error' && value.detail === message && matchedRecords.has(index))
    ));

    if (unexpectedPageErrors.length) {
      failures.push(`uncaught browser exceptions: ${JSON.stringify(unexpectedPageErrors)}`);
    }
    if (diagnostics.serverErrors.length) {
      failures.push(`unexpected HTTP 5xx responses: ${JSON.stringify(diagnostics.serverErrors)}`);
    }
    if (unexpected.length) {
      failures.push(`unexpected HTTP error responses: ${JSON.stringify(unexpected.map(formatRecord))}`);
    }
    if (unexpectedConsole.length) {
      failures.push(`unexpected console errors: ${JSON.stringify(unexpectedConsole.map(value => value.detail))}`);
    }
    if (unexpectedTransport.length) {
      failures.push(`unexpected transport failures: ${JSON.stringify(unexpectedTransport.map(value => `${value.method} ${value.url} ${value.detail}`))}`);
    }
    if (unexpected.length || unexpectedConsole.length || unexpectedTransport.length || unexpectedPageErrors.length) {
      failures.push(`observed browser events:\n${legacyText || '(none)'}`);
    }
    if (unmatchedExpectations.length && testInfo.status === testInfo.expectedStatus) {
      failures.push(`declared browser-error expectations that never matched: ${JSON.stringify(unmatchedExpectations)}`);
    }
    expect(failures.join('\n\n'), 'browser diagnostics contract').toBe('');
    void aborted;
  }, { auto: true }],
});
