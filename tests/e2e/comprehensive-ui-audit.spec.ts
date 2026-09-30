import { test, expect, adminLogin, createBoard, createThreadViaRequest, createReplyViaRequest, setBoardFixtureSettings, setThreadFixtureState, sqliteExec, sqliteQuery } from './helpers';
import { expectConsoleError, expectHttpError, expectTransportFailure } from './diagnostics';
import { expectNoHorizontalOverflow } from './phase4-helpers';

const widths = [320, 360, 390, 430, 768, 1024, 1280, 1440];

test('audit: long public surfaces and expanded admin sections fit every release viewport', async ({ page, app }, testInfo) => {
  test.setTimeout(180_000);
  const token = 'LongUnbrokenContent'.repeat(10);
  setBoardFixtureSettings(app, 'pub', { name: token, description: token, allowArchive: true, postCooldownSecs: 0 });
  for (let i = 0; i < 12; i++) app.createBoardCli({ short: `nav${i}`, name: `Navigation ${i} ${token}` });
  await app.restart();
  const id = await createThreadViaRequest(page, app, 'pub', { subject: token, body: `${token}\nhttps://example.test/${token}\n> ${token}` });
  for (let i = 0; i < 12; i++) await createReplyViaRequest(page, app, 'pub', id, `${i} ${token}\n>>${id}`);
  const archived = await createThreadViaRequest(page, app, 'pub', { subject: 'archived thread', body: token });
  setThreadFixtureState(app, archived, { archived: true, locked: true });
  sqliteExec(app, `UPDATE threads SET reply_count=2147483647 WHERE id=${id}`);
  expectHttpError({
    method: 'GET',
    path: '/missing-board',
    status: 404,
    times: 'any',
    reason: 'every release viewport intentionally loads the missing-board 404 page in this loop',
  });
  for (const width of widths) {
    await page.setViewportSize({ width, height: 844 });
    for (const route of ['/', '/pub', '/pub/catalog', '/pub/hidden', '/pub/archive', `/pub/thread/${id}`, `/pub/thread/${archived}`, '/pub/search?q=' + token, '/missing-board', '/admin']) {
      await page.goto(app.baseURL + route);
      await expectNoHorizontalOverflow(page, `${width}px ${route}`, 1);
      if (route === `/pub/thread/${id}`) await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('2147483647');
      if (width === 320 || width === 1440) await page.screenshot({ path: testInfo.outputPath(`${width}-${route.replace(/[^a-z0-9]/gi, '_').slice(0, 90)}.png`) });
    }
    await page.goto(app.baseURL);
    const preferences = page.locator('.user-preferences-panel');
    await preferences.locator('summary').first().click();
    await expectNoHorizontalOverflow(page, `${width}px expanded preferences`, 1);
    if (width === 320) await page.screenshot({ path: testInfo.outputPath('320-preferences.png') });
    await page.goto(app.baseURL);
    const mobile = page.locator('.mobile-board-menu');
    if (await mobile.isVisible()) {
      await mobile.locator('summary').click();
      await expectNoHorizontalOverflow(page, `${width}px expanded navigation`, 1);
    }
  }
  await adminLogin(page, app);
  // This is a layout audit; prepare all expanded cards together instead of
  // scrolling through changing container bounds to click every nested summary.
  await page.locator('.admin-panel details:not([data-admin-diagnostics])').evaluateAll(details => {
    for (const element of details) {
      if (element instanceof HTMLDetailsElement) element.open = true;
    }
  });
  for (const width of widths) {
    await page.setViewportSize({ width, height: 844 });
    await expectNoHorizontalOverflow(page, `${width}px all admin sections expanded`, 1);
    const clipped = await page.locator('.admin-section').evaluateAll(sections => sections
      .filter(section => section.scrollWidth - section.clientWidth > 1)
      .map(section => ({ id: section.id, overflow: section.scrollWidth - section.clientWidth })));
    expect(clipped, `${width}px admin cards must not clip their content`).toEqual([]);
    if ([320, 768, 1440].includes(width)) {
      for (const section of ['site-settings', 'boards', 'moderation', 'appearance', 'backups', 'maintenance']) {
        // Group wrappers change height as live diagnostics refresh. Scroll the
        // first heading/summary, whose bounds stay stable, for the screenshot.
        await page.locator(`#${section}`).locator('h2, summary').first().scrollIntoViewIfNeeded({ timeout: 15_000 });
        await page.screenshot({ path: testInfo.outputPath(`${width}-admin-${section}.png`) });
      }
    }
  }
});

test('audit: live pin changes preserve selected uploads and update admin actions', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'live pin controls');
  const id = await createThreadViaRequest(page, app, 'pub', { body: 'pin transition baseline' });
  await adminLogin(page, app);
  await page.goto(`${app.baseURL}/pub/thread/${id}`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const upload = page.locator('#post-form-wrap input[type="file"]').first();
  await upload.setInputFiles(app.fixtures().tinyPng);
  await page.locator('#reply-body').fill('draft across pin changes');
  for (const sticky of [true, false]) {
    setThreadFixtureState(app, id, { sticky });
    await page.locator('[data-action="fetch-updates"]').first().click();
    await expect(page.locator('.post.op .thread-state-badge-pin')).toHaveCount(sticky ? 1 : 0);
    await expect(page.locator('.admin-toolbar input[name="action"]').first()).toHaveValue(sticky ? 'unsticky' : 'sticky');
    await expect(page.locator('#reply-body')).toHaveValue('draft across pin changes');
    await expect.poll(() => upload.evaluate((input: HTMLInputElement) => input.files?.length)).toBe(1);
  }
});

test('audit: malformed admin hash does not stop initialization or section navigation', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'JavaScript fragment decoding');
  await adminLogin(page, app);
  await page.goto(`${app.baseURL}/admin/panel#%`);
  await page.locator('#live-log > details > summary').click();
  await expect(page.locator('[data-admin-live-log-controls]')).not.toHaveAttribute('hidden', '');
  await page.locator('.admin-section-index a[href="#boards"]').click();
  await expect(page.locator('#boards details').first()).toHaveAttribute('open', '');
});

test('audit: delayed preference save cannot overwrite a newer selection', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'enhanced preference saves');
  await page.goto(app.baseURL);
  await page.locator('.user-preferences-summary').click();
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let started!: () => void;
  const firstStarted = new Promise<void>(resolve => { started = resolve; });
  let requests = 0;
  await page.route(`${app.baseURL}/preferences`, async route => {
    requests++;
    if (requests === 1) { started(); await gate; }
    await route.continue();
  });
  try {
    const select = page.locator('.user-preferences-form select[name="theme"]');
    await select.selectOption('blue-sky');
    await firstStarted;
    await select.selectOption('terminal');
    await expect(page.locator('html')).not.toHaveAttribute('data-theme', 'blue-sky');
    // The latest selection remains pending until the older request is settled.
    await expect(page.locator('.user-preferences-status')).toHaveText('Saving…');
    release();
    await expect(page.locator('.user-preferences-status')).toHaveText('Saved.');
    await expect.poll(async () => (await page.context().cookies()).find(c => c.name === 'rustchan_theme')?.value).toBe('terminal');
    await page.reload();
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'terminal');
  } finally { release(); await page.unrouteAll({ behavior: 'wait' }); }
});

test('audit: stale cross-board preview response does not replace the current quote', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'quote hover previews');
  const first = await createThreadViaRequest(page, app, 'txt', { body: 'first quote content' });
  const second = await createThreadViaRequest(page, app, 'txt', { body: 'second quote content' });
  const firstPost = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id=${first}`));
  const secondPost = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id=${second}`));
  const thread = await createThreadViaRequest(page, app, 'pub', { body: `>>>/txt/${firstPost}\n>>>/txt/${secondPost}` });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let started!: () => void;
  const firstStarted = new Promise<void>(resolve => { started = resolve; });
  await page.route(`${app.baseURL}/api/post/txt/${firstPost}`, async route => { started(); await gate; await route.continue(); });
  try {
    await page.locator(`#thread-posts a.crosslink[data-pid="${firstPost}"]`).hover();
    await firstStarted;
    await page.locator('.site-header').hover({ position: { x: 4, y: 4 } });
    await expect(page.locator('#ql-popup')).toBeHidden();
    await page.locator(`#thread-posts a.crosslink[data-pid="${secondPost}"]`).hover();
    await expect(page.locator('#ql-popup')).toContainText('second quote content');
    const response = page.waitForResponse(`${app.baseURL}/api/post/txt/${firstPost}`);
    release();
    await (await response).finished();
    // Rewritten href is applied by the same response callback that updates the popup.
    await expect(page.locator(`#thread-posts a.crosslink[data-pid="${firstPost}"]`)).toHaveAttribute('href', `/txt/thread/${first}#p${firstPost}`);
    await expect(page.locator('#ql-popup')).toContainText('second quote content');
    await expect(page.locator('#ql-popup')).not.toContainText('first quote content');
  } finally { release(); await page.unrouteAll({ behavior: 'wait' }); }
});

test('audit: valid cross-board quotes are not marked missing or wired as local quotes', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'quote enhancement');
  const other = await createThreadViaRequest(page, app, 'txt', { body: 'real external-board post' });
  const post = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id=${other}`));
  const thread = await createThreadViaRequest(page, app, 'pub', { body: `>>>/txt/${post}` });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  const link = page.locator('#thread-posts a.crosslink');
  await expect(link).not.toHaveClass(/missing-post-ref/);
  await link.click();
  await expect(page).toHaveURL(`${app.baseURL}/txt/thread/${other}#p${post}`);
  await expect(page.locator('.post-body')).toContainText('real external-board post');
});

test('audit: thread updates refresh both desktop and mobile board navigation', async ({ page, context, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'live navigation refresh');
  const thread = await createThreadViaRequest(page, app, 'pub', { body: 'live navigation baseline' });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  const admin = await context.newPage();
  try { await createBoard(admin, app, { short: 'newnav', name: 'New navigation board' }); }
  finally { await admin.close(); }
  await page.locator('[data-action="fetch-updates"]').first().click();
  await expect(page.locator('nav.board-list a[href="/newnav/catalog"]')).toHaveCount(1);
  await page.setViewportSize({ width: 320, height: 844 });
  await page.locator('.mobile-board-menu > summary').click();
  await expect(page.locator('.mobile-board-menu-panel a[href="/newnav/catalog"]')).toBeVisible();
  await expectNoHorizontalOverflow(page, 'updated mobile navigation', 1);
});

test('audit: thread updates reflect lock and unlock transitions while preserving the draft', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'live thread state');
  const thread = await createThreadViaRequest(page, app, 'pub', { body: 'lock transition baseline' });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  const toggle = page.locator('[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) await toggle.click();
  await page.locator('#reply-body').fill('draft survives a moderator lock');
  setThreadFixtureState(app, thread, { locked: true });
  await page.locator('[data-action="fetch-updates"]').first().click();
  await expect(page.locator('.locked-notice')).toContainText('locked');
  await expect(page.locator('#post-form-wrap form.post-form')).toHaveCount(0);
  await expect(page.locator('.self-action-controls')).toHaveCount(0);
  setThreadFixtureState(app, thread, { locked: false });
  await page.locator('[data-action="fetch-updates"]').first().click();
  await expect(page.locator('#reply-body')).toHaveValue('draft survives a moderator lock');
  await expect(page.locator('.locked-notice')).toHaveCount(0);
});

test('audit: closed threads hide unavailable owner edit and delete actions', async ({ page, app }) => {
  setBoardFixtureSettings(app, 'pub', { allowEditing: true, allowSelfDelete: true, allowArchive: true });
  const thread = await createThreadViaRequest(page, app, 'pub', { body: 'owner controls baseline' });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  await expect(page.locator('.self-action-controls .edit-btn')).toBeVisible();
  await expect(page.locator('.self-action-controls .del-btn')).toBeVisible();
  for (const state of [{ locked: true, archived: false }, { locked: true, archived: true }]) {
    setThreadFixtureState(app, thread, state);
    await page.reload();
    await expect(page.locator('.locked-notice')).toBeVisible();
    await expect(page.locator('.self-action-controls')).toHaveCount(0);
  }
  setThreadFixtureState(app, thread, { locked: false, archived: false });
  await createReplyViaRequest(page, app, 'pub', thread, 'an OP with replies cannot be self-deleted');
  await page.reload();
  await expect(page.locator('.post.op .edit-btn')).toBeVisible();
  await expect(page.locator('.post.op .del-btn')).toHaveCount(0);
});

test('audit: report dialog keeps forward and reverse keyboard focus inside its controls', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'modal keyboard interaction');
  const thread = await createThreadViaRequest(page, app, 'pub', { body: 'keyboard modal baseline' });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  const trigger = page.locator('.report-btn').first();
  await trigger.click();
  const dialog = page.locator('#report-modal');
  const first = dialog.locator('#report-reason');
  const last = dialog.getByRole('button').last();
  await first.focus();
  await page.keyboard.press('Shift+Tab');
  await expect(last).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(first).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(trigger).toBeFocused();
});

test('audit: transient cross-board preview failures can be retried without reloading', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'quote previews');
  const target = await createThreadViaRequest(page, app, 'txt', { body: 'quote after network recovery' });
  const post = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id=${target}`));
  const thread = await createThreadViaRequest(page, app, 'pub', { body: `>>>/txt/${post}` });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  const url = `${app.baseURL}/api/post/txt/${post}`;
  await page.route(url, route => route.abort('failed'));
  expectTransportFailure({
    url,
    reason: 'the deliberately aborted cross-board preview must surface as a failed preview',
  });
  expectConsoleError({
    pattern: /net::ERR_FAILED/,
    reason: 'Chromium logs the aborted resource; WebKit reports only the transport failure',
    optional: true,
  });
  const link = page.locator('#thread-posts a.crosslink');
  await link.hover();
  await expect(page.locator('#ql-popup')).not.toContainText('loading');
  await page.unroute(url);
  await link.click();
  await expect(page).toHaveURL(`${app.baseURL}/txt/thread/${target}#p${post}`);
  await expect(page.locator('.post-body')).toContainText('quote after network recovery');
});

test('audit: new reply jump control supports keyboard activation', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'live new reply notification');
  const thread = await createThreadViaRequest(page, app, 'pub', { body: 'long post line\n'.repeat(70) });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  await createReplyViaRequest(page, app, 'pub', thread, 'new bottom reply');
  await page.locator('[data-action="fetch-updates"]').first().click();
  const pill = page.locator('#new-replies-pill');
  await expect(pill).toBeVisible();
  await expect(pill).toHaveRole('button');
  await pill.focus();
  await expect(pill).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(pill).toBeHidden();
  await expect.poll(() => page.evaluate(() => scrollY)).toBeGreaterThan(500);
});

test('audit: manual update failure does not promise an automatic retry', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'thread update feedback');
  const thread = await createThreadViaRequest(page, app, 'pub', { body: 'manual retry feedback' });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  await page.route(`${app.baseURL}/pub/thread/${thread}/updates**`, route => route.abort('failed'));
  expectTransportFailure({
    url: `${app.baseURL}/pub/thread/${thread}/updates`,
    reason: 'the deliberately aborted manual update must surface as "Update failed."',
  });
  expectConsoleError({
    pattern: /net::ERR_FAILED/,
    reason: 'Chromium logs the aborted resource; WebKit reports only the transport failure',
    optional: true,
  });
  await page.locator('[data-action="fetch-updates"]').first().click();
  const status = page.locator('[data-role="autoupdate-status"]').first();
  await expect(status).toContainText('Update failed.');
  await expect(status).not.toContainText('Retrying in');
  await expect(status).toContainText('Update now');
});

test('audit: live replies remove an OP delete action that the server can no longer honor', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'live owner controls');
  setBoardFixtureSettings(app, 'pub', { allowEditing: true, allowSelfDelete: true });
  const thread = await createThreadViaRequest(page, app, 'pub', { body: 'live OP ownership' });
  await page.goto(`${app.baseURL}/pub/thread/${thread}`);
  await expect(page.locator('.post.op .del-btn')).toBeVisible();
  await createReplyViaRequest(page, app, 'pub', thread, 'first reply changes OP permissions');
  await page.locator('[data-action="fetch-updates"]').first().click();
  await expect(page.locator('.post.reply')).toHaveCount(1);
  await expect(page.locator('.post.op .del-btn')).toHaveCount(0);
  await expect(page.locator('.post.op .edit-btn')).toBeVisible();
});

test('audit: archive navigation and controls honor saved visitor preferences', async ({ page, app }) => {
  setBoardFixtureSettings(app, 'pub', { allowArchive: true });
  await page.context().addCookies([
    { name: 'rustchan_hide_nsfw', value: '1', url: app.baseURL },
    { name: 'rustchan_preferred_view', value: 'index', url: app.baseURL },
    { name: 'rustchan_video_audio', value: 'mute', url: app.baseURL },
    { name: 'rustchan_activity_badges', value: '0', url: app.baseURL },
  ]);
  await page.goto(`${app.baseURL}/pub/archive`);
  await expect(page.locator('.board-list a[href="/nsfw/catalog"]')).toHaveCount(0);
  await expect(page.locator('.board-list a[href="/pub"]')).toHaveCount(1);
  await expect(page.locator('.user-preferences-form input[name="hide_nsfw_boards"]')).toBeChecked();
  await expect(page.locator('.user-preferences-form input[name="video_audio"][value="mute"]')).toBeChecked();
  await expect(page.locator('.user-preferences-form input[name="show_activity_badges"]')).not.toBeChecked();
});

test('audit: no-JS catalog clearly identifies unavailable client-only controls', async ({ page, app, javaScriptEnabled }) => {
  test.skip(javaScriptEnabled, 'no-JS enhancement boundary');
  await createThreadViaRequest(page, app, 'pub', { body: 'visible no-JS catalog comment' });
  await page.goto(`${app.baseURL}/pub/catalog`);
  await expect(page.locator('#catalog-sort')).toBeDisabled();
  await expect(page.locator('#catalog-show-comment')).toBeDisabled();
  // Playwright's aggregate element text omits noscript descendants.
  const explanation = page.locator('.catalog-controls noscript p');
  await expect(explanation).toBeVisible();
  await expect(explanation).toContainText('require JavaScript');
  await expect(page.locator('#catalog-show-comment')).toHaveValue('on');
  await expect(page.locator('.catalog-comment')).toBeVisible();
});

test('audit: catalog controls still work when browser storage is unavailable', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'client controls with unavailable storage');
  await page.addInitScript(() => {
    Object.defineProperty(window, 'sessionStorage', { get() { throw new DOMException('Storage denied', 'SecurityError'); } });
  });
  await createThreadViaRequest(page, app, 'pub', { body: 'catalog with denied storage' });
  await page.goto(`${app.baseURL}/pub/catalog`);
  await expect(page.locator('#catalog-sort')).toBeEnabled();
  await expect(page.locator('#catalog-show-comment')).toBeEnabled();
  await expect(page.locator('#catalog-sort')).toHaveValue('bump');
  await expect(page.locator('#catalog-show-comment')).toHaveValue('off');
  await page.locator('#catalog-show-comment').selectOption('on');
  await expect(page.locator('.catalog-comment')).toBeVisible();
});
