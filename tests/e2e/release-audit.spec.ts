import { expectConsoleError, expectHttpError, expectTransportFailure, newAuditedContext } from './diagnostics';
import { test, expect, createThread, createReply, createReplyViaRequest, setBoardFixtureSettings, setThreadFixtureState, setPostFixtureCreatedAt, sqliteQuery, extractCsrf } from './helpers';
import { expectNoHorizontalOverflow } from './phase4-helpers';

async function openForm(page, route: string) {
  const toggle = page.locator('[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) await toggle.click();
  const form = page.locator(`form[action="${route}"]`).first();
  await expect(form.getByLabel('body', { exact: true })).toBeVisible();
  return form;
}

test('release: empty and populated homepage statistics reflect persisted posts and media', async ({ page, app }) => {
  await page.goto(app.baseURL);
  const stat = (label: string) => page.locator('.index-stat').filter({ hasText: label }).locator('.index-stat-value');
  for (const label of ['total posts', 'images uploaded', 'videos uploaded', 'audio files uploaded']) await expect(stat(label)).toHaveText('0');
  await expect(page.locator('.index-stats-unavailable')).toHaveCount(0);
  const id = await createThread(page, app, 'pub', { subject: 'statistics', body: 'one image', filePath: app.fixtures().tinyPng });
  await createReply(page, app, 'pub', id, 'one text reply');
  await page.goto(app.baseURL);
  await expect(stat('total posts')).toHaveText('2');
  await expect(stat('images uploaded')).toHaveText('1');
  await expect(page.locator('.site-footer')).toContainText('home');
  await page.reload();
  await expect(stat('total posts')).toHaveText('2');
});

test('release: invalid saved catalog controls recover to defaults and valid choices persist', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'session storage catalog enhancement requires JavaScript');
  await createThread(page, app, 'pub', { subject: 'catalog preference', body: 'catalog preview text' });
  await page.evaluate(() => {
    sessionStorage.setItem('catalog_sort', 'removed-sort-mode');
    sessionStorage.setItem('catalog_show_comment', 'not-a-mode');
  });
  await page.goto(`${app.baseURL}/pub/catalog`);
  await expect(page.locator('#catalog-sort')).toHaveValue('bump');
  await expect(page.locator('#catalog-show-comment')).toHaveValue('off');
  await expect(page.locator('#catalog-grid')).toHaveClass(/catalog-comments-off/);
  await page.locator('#catalog-sort').selectOption('replies');
  await page.locator('#catalog-show-comment').selectOption('on');
  await page.reload();
  await expect(page.locator('#catalog-sort')).toHaveValue('replies');
  await expect(page.locator('#catalog-show-comment')).toHaveValue('on');
  await expect(page.locator('#catalog-grid')).not.toHaveClass(/catalog-comments-off/);
});

test('release: stale edit form enforces the server window and cross-board ownership boundary', async ({ page, app, browser }) => {
  setBoardFixtureSettings(app, 'pub', { allowEditing: true, allowSelfDelete: true });
  const id = await createThread(page, app, 'pub', { body: 'original window body' });
  const post = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id=${id}`));
  const other = await newAuditedContext(browser);
  try {
    const foreign = await other.request.get(`${app.baseURL}/pub/post/${post}/edit`);
    expect(foreign.status()).toBe(403);
    const wrongBoard = await page.request.get(`${app.baseURL}/img/post/${post}/edit`);
    expect(wrongBoard.status()).toBe(404);
  } finally { await other.close(); }
  await page.goto(`${app.baseURL}/pub/post/${post}/edit`);
  await page.getByRole('textbox', { name: 'edit post body', exact: true }).fill('expired mutation');
  setPostFixtureCreatedAt(app, post, Math.floor(Date.now() / 1000) - 61);
  expectHttpError({
    method: 'POST',
    path: `/pub/post/${post}/edit`,
    status: 403,
    reason: 'a stale edit after the 60-second window is refused and persists nothing',
  });
  const [response] = await Promise.all([
    page.waitForResponse(r => r.request().method() === 'POST' && r.url().endsWith(`/post/${post}/edit`)),
    page.getByRole('button', { name: /save edit/i }).click(),
  ]);
  expect(response.status()).toBe(403);
  await expect(page.locator('body')).toContainText(/60-second|closed|expired/i);
  expect(sqliteQuery(app, `SELECT body FROM posts WHERE id=${post}`)).toBe('original window body');
});

test('release: a stale reply form rejects a concurrent lock without persisting its body', async ({ page, app }) => {
  const id = await createThread(page, app, 'pub', { body: 'lock race baseline' });
  const route = `/pub/thread/${id}`;
  const form = await openForm(page, route);
  await form.getByLabel('body', { exact: true }).fill('must not persist after lock');
  setThreadFixtureState(app, id, { locked: true });
  expectHttpError({
    method: 'POST',
    path: route,
    status: 403,
    reason: 'a stale reply submitted after the thread was locked is refused',
  });
  const [response] = await Promise.all([
    page.waitForResponse(r => r.request().method() === 'POST' && new URL(r.url()).pathname === route),
    form.getByRole('button', { name: /post reply/i }).click(),
  ]);
  expect(response.status()).toBe(403);
  await expect(page.locator('body')).toContainText(/locked/i);
  expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id=${id}`)).toBe('1');
});

test('release: repeated Enter while a thread POST is pending creates one thread', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'pending enhanced submission guard');
  await page.goto(`${app.baseURL}/pub`);
  const form = await openForm(page, '/pub');
  await form.getByLabel('subject', { exact: true }).fill('repeated Enter');
  await form.getByLabel('body', { exact: true }).fill('single persisted submission');
  await form.locator('input[type=file]').setInputFiles(app.fixtures().tinyPng);
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let intercepted!: () => void;
  const started = new Promise<void>(resolve => { intercepted = resolve; });
  let count = 0;
  await page.route(`${app.baseURL}/pub`, async route => {
    if (route.request().method() !== 'POST') return route.continue();
    count += 1; intercepted(); await gate; await route.continue();
  });
  try {
    const submit = form.locator('button[type=submit]');
    await submit.focus();
    await page.keyboard.press('Enter');
    await started;
    await expect(submit).toBeDisabled();
    await page.keyboard.press('Enter');
    await page.keyboard.press('Enter');
    release();
    await expect(page).toHaveURL(/\/pub\/thread\/\d+/);
    expect(count).toBe(1);
    await page.reload();
    expect(sqliteQuery(app, 'SELECT COUNT(*) FROM threads;')).toBe('1');
    await expect(page.locator('.post')).toHaveCount(1);
  } finally { release(); await page.unrouteAll({ behavior: 'wait' }); }
});

test('release: signed public CSRF survives cookie loss but tampered forms fail closed', async ({ page, app }) => {
  await page.goto(`${app.baseURL}/pub`);
  const form = await openForm(page, '/pub');
  await form.getByLabel('body', { exact: true }).fill('stale CSRF body');
  const token = await form.locator('input[name="_csrf"]').inputValue();
  await page.context().clearCookies();
  const [response] = await Promise.all([
    page.waitForResponse(r => r.request().method() === 'POST' && new URL(r.url()).pathname === '/pub'),
    form.getByRole('button', { name: /post thread/i }).click(),
  ]);
  // Public tokens are deliberately signed and cookie-independent (unlike admin CSRF).
  expect(response.status()).toBe(303);
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('1');
  await page.goto(`${app.baseURL}/pub`);
  expect(await page.locator('form[action="/pub"] input[name="_csrf"]').inputValue()).not.toBe(token);
  const tampered = await openForm(page, '/pub');
  await tampered.getByLabel('body', { exact: true }).fill('tampered token body');
  await tampered.locator('input[name="_csrf"]').evaluate((input: HTMLInputElement) => { input.value = 'invalid-signature'; });
  expectHttpError({
    method: 'POST',
    path: '/pub',
    status: 403,
    reason: 'a tampered CSRF token is refused without persisting a post',
  });
  const [rejected] = await Promise.all([
    page.waitForResponse(r => r.request().method() === 'POST' && new URL(r.url()).pathname === '/pub'),
    tampered.getByRole('button', { name: /post thread/i }).click(),
  ]);
  expect(rejected.status()).toBe(403);
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('1');
  await createThread(page, app, 'pub', { body: 'fresh CSRF recovers' });
  await expect(page.locator('.post')).toContainText('fresh CSRF recovers');
});

test('release: manual updates recover after network failure and append a reply once', async ({ page, browser, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'thread update enhancement');
  const id = await createThread(page, app, 'pub', { body: 'polling baseline' });
  const update = page.locator('[data-action="fetch-updates"]').first();
  const pattern = `${app.baseURL}/pub/thread/${id}/updates**`;
  expectTransportFailure({
    url: `${app.baseURL}/pub/thread/${id}/updates`,
    reason: 'the deliberately aborted update request must surface as a failure banner',
  });
  expectConsoleError({
    pattern: /Failed to load resource: net::ERR_FAILED/,
    reason: 'Chromium logs the aborted resource; other engines report only the transport failure',
    optional: true,
  });
  await page.route(pattern, route => route.abort('failed'));
  await update.click();
  await expect(page.locator('[data-role="autoupdate-status"]').first()).toContainText('Update failed');
  await expect(update).toBeEnabled();
  await page.unroute(pattern);
  const other = await newAuditedContext(browser);
  try { await createReplyViaRequest(await other.newPage(), app, 'pub', id, 'live recovery reply'); }
  finally { await other.close(); }
  await update.click();
  await expect(page.locator('.post')).toHaveCount(2);
  await expect(page.locator('.post.reply')).toContainText('live recovery reply');
  await expect(update).toBeEnabled();
  await update.click();
  await expect(page.locator('[data-role="autoupdate-status"]').first()).toContainText('Updated.');
  await expect(page.locator('.post')).toHaveCount(2);
  await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('1');
  await page.setViewportSize({ width: 320, height: 740 });
  await expectNoHorizontalOverflow(page, '320px updated thread', 1);
});

test('release: natural multi-board activity survives history restoration without stale New badges', async ({ page, browser, app, javaScriptEnabled }, testInfo) => {
  if (javaScriptEnabled) await page.addInitScript(() => {
    addEventListener('pageshow', event => {
      const key = 'release-history-events';
      const events = JSON.parse(sessionStorage.getItem(key) || '[]');
      events.push({ path: location.pathname, persisted: event.persisted, type: performance.getEntriesByType('navigation')[0]?.type });
      sessionStorage.setItem(key, JSON.stringify(events));
    });
  });
  const first = await createThread(page, app, 'pub', { subject: 'first activity target', body: 'visited first' });
  const second = await createThread(page, app, 'txt', { subject: 'second activity target', body: 'visited second' });
  await page.goto(`${app.baseURL}/pub/thread/${first}`);
  await page.goto(`${app.baseURL}/txt/thread/${second}`);
  const writer = await newAuditedContext(browser, { javaScriptEnabled });
  try {
    const tab = await writer.newPage();
    await createReply(tab, app, 'pub', first, 'unread first reply');
    await createReply(tab, app, 'txt', second, 'unread second reply');
  } finally { await writer.close(); }
  await page.goto(`${app.baseURL}/pub`);
  await expect(page.locator(`#t${first} .thread-summary-activity-badge`)).toContainText('1 New');
  await page.locator(`#t${first} a[href="/pub/thread/${first}"]`).first().click();
  await expect(page.locator('.post.reply')).toContainText('unread first reply');
  await page.goBack({ waitUntil: 'domcontentloaded' });
  await expect(page.locator(`#t${first}`)).toBeVisible();
  await expect(page.locator(`#t${first} .thread-summary-activity-badge`)).toHaveCount(0);
  await page.goForward({ waitUntil: 'domcontentloaded' });
  await expect(page.locator('.post.reply')).toContainText('unread first reply');
  await page.goto(`${app.baseURL}/txt`);
  await expect(page.locator(`#t${second} .thread-summary-activity-badge`)).toContainText('1 New');
  await page.locator(`#t${second} a[href="/txt/thread/${second}"]`).first().click();
  await expect(page.locator('.post.reply')).toContainText('unread second reply');
  await page.goBack({ waitUntil: 'domcontentloaded' });
  await expect(page.locator(`#t${second}`)).toBeVisible();
  await expect(page.locator(`#t${second} .thread-summary-activity-badge`)).toHaveCount(0);
  await page.reload();
  await expect(page.locator(`#t${second} .thread-summary-activity-badge`)).toHaveCount(0);
  if (javaScriptEnabled) await testInfo.attach('history-lifecycle.json', {
    body: await page.evaluate(() => sessionStorage.getItem('release-history-events') || '[]'), contentType: 'application/json',
  });
});
