import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { expectHttpError, expectTransportFailure } from './diagnostics';
import { createThread, createThreadViaRequest, expect, isNoJsProject, setBoardFixtureSettings, sqliteQuery, test } from './helpers';

const evidence = path.resolve('output/playwright/public-polish');

async function capture(page: import('@playwright/test').Page, name: string) {
  fs.mkdirSync(evidence, { recursive: true });
  await page.screenshot({ path: path.join(evidence, name), fullPage: true });
}

test('CAPTCHA refresh preserves unsent text, poll choices and selected upload', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'native CAPTCHA refresh is a document navigation');
  setBoardFixtureSettings(app, 'pub', { allowCaptcha: true, postCooldownSecs: 0 });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form.post-form').first();
  await form.locator('[name="subject"]').fill('雪 long subject');
  await form.locator('[name="body"]').fill('Keep this recoverable text >>>/pub/123\n> nested quote');
  await form.locator('.poll-creator summary').first().click();
  await form.locator('[name="poll_question"]').fill('Retain my question?');
  await form.locator('[name="poll_option"]').nth(0).fill('Yes ☃');
  await form.locator('[name="poll_option"]').nth(1).fill('No');
  await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
  const token = await form.locator('[name="submission_token"]').inputValue();
  const captcha = await form.locator('[name="captcha_id"]').inputValue();
  await capture(page, `captcha-refresh-${process.env.RUSTCHAN_POLISH_PHASE ?? 'after'}-${info.project.name}.png`);
  await form.locator('.captcha-refresh-link').click();
  await expect(form.locator('[name="captcha_id"]')).not.toHaveValue(captcha);
  await expect(form.locator('[name="subject"]')).toHaveValue('雪 long subject');
  await expect(form.locator('[name="body"]')).toHaveValue('Keep this recoverable text >>>/pub/123\n> nested quote');
  await expect(form.locator('[name="poll_question"]')).toHaveValue('Retain my question?');
  await expect(form.locator('[name="poll_option"]').nth(0)).toHaveValue('Yes ☃');
  expect(await form.locator('input[type="file"]').first().evaluate((input: HTMLInputElement) => input.files?.length)).toBe(1);
  await expect(form.locator('[name="submission_token"]')).toHaveValue(token);
  await expect.poll(() => form.locator('.captcha-image').evaluate((image: HTMLImageElement) => image.complete && image.naturalWidth > 0)).toBe(true);
  await capture(page, `captcha-refreshed-${process.env.RUSTCHAN_POLISH_PHASE ?? 'after'}-${info.project.name}.png`);
});

test('upload submission stays busy until confirmation and refuses unconfirmed success', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'XHR response contract');
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form.post-form').first();
  await form.locator('[name="body"]').fill('Unconfirmed response must retain this');
  await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
  let requests = 0;
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/pub', async route => {
    if (route.request().method() !== 'POST') return route.continue();
    requests += 1;
    await pending;
    await route.fulfill({ status: 200, contentType: 'text/html', body: '<html><body>proxy response without confirmation</pubody></html>' });
  });
  await form.locator('button[type="submit"]').click();
  await expect(form.locator('button[type="submit"]')).toBeDisabled();
  await form.evaluate((element: HTMLFormElement) => element.requestSubmit());
  await expect.poll(() => requests).toBe(1);
  release();
  await expect(page.locator('.post-error-banner').first()).toContainText(/confirmation|may.*succeeded|before.*retry/i);
  await expect(form.locator('[name="body"]')).toHaveValue('Unconfirmed response must retain this');
  await expect(form.locator('button[type="submit"]')).toBeEnabled();
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('0');
  expect(requests).toBe(1);
  await capture(page, `uncertain-response-${process.env.RUSTCHAN_POLISH_PHASE ?? 'after'}-${info.project.name}.png`);
});

test('aborted upload explains uncertainty and releases controls without losing input', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'XHR abort contract');
  await page.addInitScript(() => {
    const send = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.send = function (body) {
      send.call(this, body);
      if (body instanceof FormData) this.abort();
    };
  });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form.post-form').first();
  await form.locator('[name="body"]').fill('Interrupted draft');
  await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
  await form.locator('button[type="submit"]').click();
  await expect(page.locator('.post-error-banner').first()).toContainText(/interrupted|cancelled/i);
  await expect(page.locator('.post-error-banner').first()).toContainText(/may.*succeeded|before.*retry/i);
  await expect(form.locator('[name="body"]')).toHaveValue('Interrupted draft');
  await expect(form.locator('button[type="submit"]')).toBeEnabled();
});

test('server rejected reply takes precedence over stale autosave', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'localStorage autosave is a JS enhancement');
  const thread = await createThread(page, app, 'pub', { body: 'Original thread' });
  const key = await page.locator('#thread-config').getAttribute('data-draft-key');
  await page.evaluate(key => localStorage.setItem(key!, 'stale autosave'), key);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form.post-form[action="/pub/thread/' + thread + '"]');
  // Another tab can overwrite shared autosave while this request is navigating.
  await page.addInitScript(key => localStorage.setItem(key!, 'stale autosave from another tab'), key);
  await form.locator('[name="body"]').evaluate((field: HTMLTextAreaElement) => { field.value = 'Latest submitted reply'; });
  setBoardFixtureSettings(app, 'pub', { allowCaptcha: true });
  expectHttpError({ method: 'POST', path: `/pub/thread/${thread}`, status: 422, reason: 'board enables CAPTCHA after composer rendered' });
  await form.locator('button[type="submit"]').click();
  await expect(page.locator('.post-error-banner').first()).toContainText(/CAPTCHA/i);
  await expect(page.locator('#reply-body')).toHaveValue('Latest submitted reply');
});

test('rejected upload keeps attachments and can refresh its consumed CAPTCHA', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'enhanced upload recovery');
  setBoardFixtureSettings(app, 'pub', { allowCaptcha: true, postCooldownSecs: 0 });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form[action="/pub"]');
  await form.locator('[name="body"]').fill('Upload recovery body');
  await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
  await form.locator('[name="captcha_answer"]').fill('WRONG');
  const original = await form.locator('[name="captcha_id"]').inputValue();
  const token = await form.locator('[name="submission_token"]').inputValue();
  expectHttpError({ method: 'POST', path: '/pub', status: 422, reason: 'wrong CAPTCHA with selected upload' });
  await form.locator('button[type="submit"]').click();
  await expect(page.locator('.post-error-banner').first()).toContainText(/choose new challenge/i);
  await expect(form.locator('[name="body"]')).toHaveValue('Upload recovery body');
  await expect(form.locator('button[type="submit"]')).toBeEnabled();
  expect(await form.locator('input[type="file"]').first().evaluate((input: HTMLInputElement) => input.files?.length)).toBe(1);
  expectHttpError({ method: 'POST', path: '/pub', status: 422, reason: 'replaying a consumed challenge exercises the server expired-challenge recovery path' });
  await form.locator('button[type="submit"]').click();
  await expect(page.locator('.post-error-banner').first()).toContainText(/CAPTCHA expired.*new challenge/i);
  await expect(form.locator('[name="body"]')).toHaveValue('Upload recovery body');
  expect(await form.locator('input[type="file"]').first().evaluate((input: HTMLInputElement) => input.files?.length)).toBe(1);
  await form.locator('.captcha-refresh-link').click();
  await expect(form.locator('[name="captcha_id"]')).not.toHaveValue(original);
  await expect(form.locator('[name="captcha_answer"]')).toHaveValue('');
  await expect(form.locator('[name="submission_token"]')).toHaveValue(token);
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('0');
});

test('CAPTCHA refresh failure remains recoverable and explains the next action', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'in-place CAPTCHA refresh');
  setBoardFixtureSettings(app, 'pub', { allowCaptcha: true });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form[action="/pub"]');
  await form.locator('[name="body"]').fill('Retain while CAPTCHA service fails');
  const challenge = await form.locator('[name="captcha_id"]').inputValue();
  expectHttpError({ method: 'GET', path: /\/pub\?captcha_refresh=/, status: 403, reason: 'simulated CAPTCHA refresh permission denial' });
  await page.route('**/pub?captcha_refresh=*', route => route.fulfill({ status: 403, contentType: 'text/plain', body: 'Unavailable' }));
  await form.locator('.captcha-refresh-link').click();
  await expect(page.locator('.post-error-banner').first()).toContainText(/could not refresh.*choose new challenge again/i);
  await expect(form.locator('[name="body"]')).toHaveValue('Retain while CAPTCHA service fails');
  await expect(form.locator('.captcha-refresh-link')).not.toHaveAttribute('aria-disabled', 'true');
  await page.unroute('**/pub?captcha_refresh=*');
  await form.locator('.captcha-refresh-link').click();
  await expect(form.locator('[name="captcha_id"]')).not.toHaveValue(challenge);
});

test('synchronous send failure restores the composer controls', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'enhanced upload send failure');
  await page.addInitScript(() => {
    const send = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.send = function(body) {
      if (body instanceof FormData) throw new DOMException('Simulated send failure');
      send.call(this, body);
    };
  });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form[action="/pub"]');
  await form.locator('[name="body"]').fill('Draft before send failure');
  await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
  await form.locator('button[type="submit"]').click();
  await expect(page.locator('.post-error-banner').first()).toContainText(/could not send.*try again/i);
  await expect(form.locator('button[type="submit"]')).toBeEnabled();
  await expect(form.locator('[name="body"]')).toHaveValue('Draft before send failure');
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('0');
});

test('cooldown rejection retains the enhanced composer and permits a deliberate retry', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'native cooldown recovery is covered in public-polish-native');
  await createThreadViaRequest(page, app, 'pub', { body: 'First post establishes the cooldown' });
  setBoardFixtureSettings(app, 'pub', { postCooldownSecs: 60 });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form[action="/pub"]');
  await form.locator('[name="body"]').fill('A draft waiting for the cooldown 日本語');
  await form.locator('.poll-creator > summary').click();
  await form.locator('[name="poll_question"]').fill('Retain the choices?');
  await form.locator('[name="poll_option"]').nth(0).fill('Yes');
  await form.locator('[name="poll_option"]').nth(1).fill('Both choices');
  await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
  const token = await form.locator('[name="submission_token"]').inputValue();
  expectHttpError({ method: 'POST', path: '/pub', status: 422, reason: 'real posting cooldown rejects a second post' });
  await form.locator('button[type="submit"]').click();
  await expect(page.locator('.post-error-banner').first()).toContainText(/Please wait \d+ more seconds/i);
  await expect(form.locator('[name="body"]')).toHaveValue('A draft waiting for the cooldown 日本語');
  await expect(form.locator('[name="poll_option"]').nth(1)).toHaveValue('Both choices');
  await expect(form.locator('[name="submission_token"]')).toHaveValue(token);
  await expect(form.locator('button[type="submit"]')).toBeEnabled();
  expect(await form.locator('input[type="file"]').first().evaluate((input: HTMLInputElement) => input.files?.length)).toBe(1);
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('1');
  setBoardFixtureSettings(app, 'pub', { postCooldownSecs: 0 });
  await form.locator('button[type="submit"]').click();
  await page.waitForURL(/\/pub\/thread\/\d+/);
  await expect(page.locator('.post-body').first()).toContainText('A draft waiting for the cooldown 日本語');
  await expect(page.locator('.poll-container')).toContainText('Both choices');
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('2');
});

test('upload timeout retains input and a confirmed retry reuses the submission token', async ({ page, app }, info) => {
  test.skip(isNoJsProject(info), 'shortened XHR timeout exercises the real server while its SQLite writer is held');
  await page.addInitScript(() => {
    const send = XMLHttpRequest.prototype.send;
    let firstPost = true;
    XMLHttpRequest.prototype.send = function(body) {
      if (body instanceof FormData && firstPost) { this.timeout = 250; firstPost = false; }
      send.call(this, body);
    };
  });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form[action="/pub"]');
  await form.locator('[name="body"]').fill('Timeout recovery draft');
  await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
  const token = await form.locator('[name="submission_token"]').inputValue();
  // WAL readers still work; only the real posting transaction is delayed.
  const lock = spawn('sqlite3', [app.dbPath()], { stdio: ['pipe', 'pipe', 'pipe'] });
  const exited = new Promise<void>((resolve, reject) => {
    lock.once('error', reject);
    lock.once('exit', code => code === 0 ? resolve() : reject(new Error(`SQLite lock process exited ${code}`)));
  });
  await new Promise<void>((resolve, reject) => {
    lock.once('error', reject);
    lock.stdout.once('data', chunk => String(chunk).includes('locked') ? resolve() : reject(new Error('SQLite lock not acquired')));
    lock.stdin.write('BEGIN IMMEDIATE;\nSELECT "locked";\n');
  });
  expectTransportFailure({ url: /\/pub$/, optional: true, reason: 'timeout cancellation is a transport failure on some engines and a recorded request abort on others' });
  try {
    await form.locator('button[type="submit"]').click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/timed out.*may still have succeeded.*another tab/i);
    await expect(form.locator('[name="body"]')).toHaveValue('Timeout recovery draft');
    await expect(form.locator('button[type="submit"]')).toBeEnabled();
    expect(await form.locator('input[type="file"]').first().evaluate((input: HTMLInputElement) => input.files?.length)).toBe(1);
    await expect(form.locator('[name="submission_token"]')).toHaveValue(token);
  } finally {
    lock.stdin.end('COMMIT;\n.quit\n');
    await exited;
  }
  // The interrupted response may still have committed. Verify this before an
  // explicit retry, then verify idempotency on the actual upload path.
  await expect.poll(() => sqliteQuery(app, "SELECT COUNT(*) FROM posts WHERE body = 'Timeout recovery draft';")).toBe('1');
  await form.locator('button[type="submit"]').click();
  await page.waitForURL(/\/pub\/thread\/\d+/);
  expect(sqliteQuery(app, "SELECT COUNT(*) FROM posts WHERE body = 'Timeout recovery draft';")).toBe('1');
  await expect(page.locator('.post-body').first()).toContainText('Timeout recovery draft');
});
