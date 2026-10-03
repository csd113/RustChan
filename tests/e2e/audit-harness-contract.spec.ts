import {
  declarationStatus,
  expectConsoleError,
  expectHttpError,
  newAuditedContext,
  type DiagnosticRecord,
  type ErrorExpectation,
} from './diagnostics';
import { expect, expectServerLogError, test } from './helpers';
import { existsSync } from 'node:fs';

/**
 * Harness contract tests.
 *
 * These verify the *diagnostics layer*, not the application. They are built so a
 * regression in strictness makes them red:
 *  - Negative cases use `test.fail(true, ...)`: if the layer stops failing
 *    closed, the test unexpectedly passes and Playwright reports
 *    "expected to fail but passed".
 *  - The declaration matcher is verified directly as a pure function, so its
 *    semantics do not depend on fixture teardown ordering.
 *
 * Applicability: Chromium only. These are harness assertions; repeating them per
 * engine would add runtime without adding information.
 */

test.describe('deep audit harness contract', () => {
  test('undeclared HTTP 4xx from a page navigation fails the test', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'harness contract runs once');
    test.fail(true, 'proves the diagnostics layer fails closed on an undeclared 4xx');
    await page.goto(`${app.baseURL}/this-route-does-not-exist-deep-audit`);
  });

  test('declared HTTP 4xx is allowed, consumed, and the page contract is still asserted', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'harness contract runs once');
    expectHttpError({
      method: 'GET',
      path: '/this-route-does-not-exist-deep-audit',
      status: 404,
      reason: 'the 404 page itself is the subject of this test',
    });
    const response = await page.goto(`${app.baseURL}/this-route-does-not-exist-deep-audit`);
    expect(response?.status()).toBe(404);
    await expect(page.locator('body')).toContainText(/not found|404/i);
  });

  test('undeclared console errors fail the test', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'harness contract runs once');
    test.fail(true, 'proves console errors are not silently ignored');
    await page.goto(`${app.baseURL}/`);
    await page.evaluate(() => {
      console.error('deep-audit deliberate console error');
    });
  });

  test('declared console errors are consumed and the rest of the test still passes', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'harness contract runs once');
    expectConsoleError({ pattern: /deep-audit deliberate console error/, reason: 'the client failure path under test' });
    await page.goto(`${app.baseURL}/`);
    await page.evaluate(() => {
      console.error('deep-audit deliberate console error');
    });
    await expect(page.locator('body')).toBeVisible();
  });

  test('undeclared uncaught page errors fail the test', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'harness contract runs once');
    test.fail(true, 'proves uncaught page errors are not silently ignored');
    await page.goto(`${app.baseURL}/`);
    await page.evaluate(() => {
      setTimeout(() => {
        throw new Error('deep-audit deliberate uncaught error');
      }, 0);
    });
    await page.waitForTimeout(300);
  });

  test('diagnostics matcher consumes declarations once and reports unused ones', async ({}, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'harness contract runs once');
    const record = (partial: Partial<DiagnosticRecord>): DiagnosticRecord => ({
      ts: '1970-01-01T00:00:00.000Z',
      kind: 'http-error',
      actor: 'anonymous',
      project: 'chromium',
      test: 'matcher',
      worker: 0,
      method: 'POST',
      path: '/admin/ban/add',
      status: 403,
      ...partial,
    });
    const expectation = (partial: Partial<ErrorExpectation>): ErrorExpectation => ({
      kind: 'http-error',
      reason: 'matcher unit test',
      ...partial,
    });

    // Two identical events and one declaration: the declaration matches once and
    // the second event stays unexpected.
    const duplicate = declarationStatus(
      [record({}), record({})],
      [expectation({ method: 'POST', path: '/admin/ban/add', status: 403 })],
    );
    expect(duplicate.consumed).toEqual([0]);
    expect(duplicate.matchedRecords).toEqual([0]);
    expect(duplicate.unmatched).toEqual([]);

    // A declaration that cannot match is reported so it cannot rot.
    const unused = declarationStatus(
      [record({})],
      [expectation({ method: 'POST', path: '/never-requested', status: 500 })],
    );
    expect(unused.unmatched).toEqual([0]);
    expect(unused.matchedRecords).toEqual([]);

    // Method and status must both match; a 500 is not a declared 403.
    const wrongStatus = declarationStatus(
      [record({ status: 500 })],
      [expectation({ method: 'POST', path: '/admin/ban/add', status: 403 })],
    );
    expect(wrongStatus.matchedRecords).toEqual([]);

    // Console declarations without a URL constraint retain pattern matching.
    const consoleStatus = declarationStatus(
      [record({ kind: 'console-error', detail: 'Failed to load resource: 404', method: undefined, path: undefined, status: undefined })],
      [expectation({ kind: 'console-error', pattern: /Failed to load resource/ })],
    );
    expect(consoleStatus.consumed).toEqual([0]);

    // A native-control declaration must not consume an application error with
    // the same words, an unknown source, a different icon, or an extra message.
    // Its finite capacity also leaves a repeated excess event unexpected.
    const nativeMessage = 'Button failed to load, iconName = invalid-placard, layoutTraits = [MacOSLayoutTraits Inline], src = blob:http://127.0.0.1:1234/00000000-0000-4000-8000-000000000000';
    const nativeStatus = declarationStatus([
      record({ kind: 'console-error', url: 'http://127.0.0.1:1234/static/main.js', detail: nativeMessage }),
      record({ kind: 'console-error', url: undefined, detail: nativeMessage }),
      record({ kind: 'console-error', url: '', detail: `${nativeMessage} unexpected application failure` }),
      record({ kind: 'console-error', url: '', detail: nativeMessage.replace('invalid-placard', 'custom-control') }),
      record({ kind: 'console-error', url: '', detail: nativeMessage }),
      record({ kind: 'console-error', url: '', detail: nativeMessage }),
    ], [expectation({
      kind: 'console-error',
      pattern: /^Button failed to load, iconName = invalid-placard, layoutTraits = \[MacOSLayoutTraits Inline\], src = blob:http:\/\/127\.0\.0\.1:1234\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
      url: /^$/,
      times: 1,
    })]);
    expect(nativeStatus.consumed).toEqual([0]);
    expect(nativeStatus.matchedRecords).toEqual([4]);
    expect(nativeStatus.unmatched).toEqual([]);
  });

  test('manual contexts inherit the project JavaScript mode', async ({ browser }, testInfo) => {
    test.skip(
      !['firefox-nojs', 'chromium-nojs'].includes(testInfo.project.name),
      'asserts the no-JS project guard on a project that can actually launch on this host',
    );
    const context = await newAuditedContext(browser);
    const page = await context.newPage();
    await page.goto('data:text/html,<script>document.documentElement.setAttribute("data-js","on")</script><p>probe</p>');
    // With JavaScript disabled the inline script must not run.
    await expect(page.locator('html')).not.toHaveAttribute('data-js', 'on');
    await expect(page.locator('body')).toContainText('probe');
    await context.close();
  });

  test('fixture server belongs to this run and its log is readable', async ({ app, page }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'harness contract runs once');
    const ready = await fetch(`${app.baseURL}/readyz`);
    expect(ready.status).toBe(200);
    // /healthz reports "ok"; /readyz reports the readiness label "ready"
    // (src/server/server/observability.rs).
    expect((await ready.json() as { status?: string }).status).toBe('ready');
    expect(existsSync(app.logPath), 'fixture log exists inside the fixture root').toBe(true);
    const before = app.logSize();
    await page.goto(`${app.baseURL}/healthz`);
    expect((await page.request.get(`${app.baseURL}/healthz`)).status()).toBe(200);
    const delta = await app.logsSince(before);
    // Readiness/health traffic must not add new [ERROR] lines.
    expect(delta.split('\n').filter(line => line.includes('[ERROR]'))).toEqual([]);
    expectServerLogError({
      pattern: /never-matches-deep-audit-sentinel/,
      reason: 'declared as optional because a healthy instance logs no errors',
      optional: true,
    });
  });
});
