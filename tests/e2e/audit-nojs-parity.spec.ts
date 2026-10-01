/**
 * No-JavaScript parity audit of the server-rendered HTML/plain-form contract.
 *
 * Project applicability (documented here and gated with test.skip):
 *  - chromium with `test.use({ javaScriptEnabled: false })` below.
 *  - chromium-nojs, the project-level JavaScript-disabled configuration.
 *  - firefox-nojs remains in the gate set for configuration parity with the
 *    recorded project contract, but the Playwright Firefox build cannot create
 *    a profile on this host, so no assertion depends on it running.
 *  - webkit / firefox / mobile-firefox / mobile-webkit are excluded: those
 *    projects run with JavaScript enabled, all engines receive byte-identical
 *    server HTML, and their no-JS behaviour is already covered by
 *    mobile-nojs.spec.ts and firefox-nojs-public.spec.ts. Adding them here
 *    would only duplicate engine-agnostic markup coverage and triple runtime.
 *
 * Every interaction below uses only real links, form controls, native Enter
 * submission, file inputs, redirects, validation, and browser history. The file
 * never hides scripts, never calls app JavaScript, uses no route mocking and no
 * timer synchronisation. The JS-path parity steps explicitly create an audited
 * context with `javaScriptEnabled: true` so the enhancement is the thing under
 * test rather than the project default.
 */
import {
  expectHttpError,
  newAuditedContext,
  type Page,
} from './diagnostics';
import {
  ADMIN_PASSWORD,
  adminLogin,
  adminPasswordHash,
  createThread,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  sqliteQuery,
  test,
  threadIdFromUrl,
  uniqueShort,
} from './helpers';

test.use({ javaScriptEnabled: false });

const NOJS_PROJECTS = ['chromium', 'chromium-nojs', 'firefox-nojs'];

test.beforeEach(async ({}, testInfo) => {
  test.skip(
    !NOJS_PROJECTS.includes(testInfo.project.name),
    'no-JS parity runs on chromium and chromium-nojs with JavaScript disabled at context creation (firefox-nojs cannot launch on this host)',
  );
});

function marker(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1_000_000)}`;
}

/**
 * Thread and reply POSTs redirect to `/{board}/thread/{id}#p{postId}`. Wait for
 * that redirect and fail if the fragment (the post anchor contract) is absent.
 */
async function waitForThreadRedirect(page: Page, board: string): Promise<number> {
  await page.waitForURL(new RegExp(`/${board}/thread/\\d+(?:#p\\d+)?$`));
  const url = page.url();
  expect(url, `thread redirect for /${board}/`).toMatch(new RegExp(`/${board}/thread/\\d+#p\\d+$`));
  return threadIdFromUrl(url);
}

async function waitForReplyRedirect(page: Page, board: string, threadId: number): Promise<void> {
  await page.waitForURL(new RegExp(`/${board}/thread/${threadId}(?:#p\\d+)?$`));
  expect(page.url(), `reply redirect for /${board}/thread/${threadId}`).toMatch(
    new RegExp(`/${board}/thread/${threadId}#p\\d+$`),
  );
}

type MutationWatch = { requests: string[]; statuses: number[] };

function watchMutations(page: Page, pathname: string): MutationWatch {
  const watch: MutationWatch = { requests: [], statuses: [] };
  page.on('request', (request) => {
    if (request.method() === 'POST' && new URL(request.url()).pathname === pathname) {
      watch.requests.push(`${request.method()} ${pathname}`);
    }
  });
  page.on('response', (response) => {
    if (response.request().method() === 'POST' && new URL(response.url()).pathname === pathname) {
      watch.statuses.push(response.status());
    }
  });
  return watch;
}

async function firstPostId(page: Page, selector = '.post.op'): Promise<number> {
  const id = await page.locator(selector).first().getAttribute('id');
  const numeric = Number(id?.replace(/^p/, ''));
  expect(Number.isInteger(numeric) && numeric > 0, `post id for ${selector}`).toBe(true);
  return numeric;
}

async function submitThreadNoJs(
  page: Page,
  board: string,
  options: { subject?: string; body?: string; filePath?: string } = {},
): Promise<void> {
  const form = page.locator(`form[action="/${board}"]`).first();
  await expect(form.locator('textarea[name="body"]')).toBeVisible();
  if (options.subject !== undefined) {
    await form.locator('input[name="subject"]').fill(options.subject);
  }
  await form.locator('textarea[name="body"]').fill(options.body ?? `thread body ${Date.now()}`);
  if (options.filePath) {
    await form.locator('input[type="file"]').first().setInputFiles(options.filePath);
  }
  await form.getByRole('button', { name: /post thread/i }).click();
}

test.describe('no-JavaScript server-rendered parity', () => {
  test('an anonymous visitor creates a thread and a reply through plain forms with real redirects', async ({ page, app }, testInfo) => {
    // Preconditions: public /pub board, no cooldown.
    const subject = marker('nojs-thread');
    const body = marker('nojs-body');
    await page.goto(`${app.baseURL}/pub`);
    await expect(page.locator('html')).toHaveClass(/no-js/);
    await expectSafePage(page);
    await expect(page.locator(`form[action="/pub"]`)).toHaveCount(1);

    const threadWatch = watchMutations(page, '/pub');
    await submitThreadNoJs(page, 'pub', { subject, body });
    const threadId = await waitForThreadRedirect(page, 'pub');
    expect(threadWatch.requests).toHaveLength(1);
    expect(threadWatch.statuses).toEqual([303]);

    await expectSafePage(page);
    await expect(page.locator('.post.op')).toHaveCount(1);
    expect(sqliteQuery(app, `SELECT subject FROM threads WHERE id = ${threadId};`)).toBe(subject);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId} AND is_op = 1;`)).toBe('1');

    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.locator('body')).toContainText(body);

    const replyBody = marker('nojs-reply');
    const replyForm = page.locator(`form[action="/pub/thread/${threadId}"]`).first();
    await expect(replyForm.locator('textarea[name="body"]')).toBeVisible();
    await replyForm.locator('textarea[name="body"]').fill(replyBody);
    const replyWatch = watchMutations(page, `/pub/thread/${threadId}`);
    await replyForm.getByRole('button', { name: /post reply/i }).click();
    await waitForReplyRedirect(page, 'pub', threadId);
    expect(replyWatch.requests).toHaveLength(1);
    expect(replyWatch.statuses).toEqual([303]);
    await expect(page.locator('body')).toContainText(replyBody);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.locator('body')).toContainText(replyBody);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId} AND is_op = 0;`)).toBe('1');
    await expectSafePage(page);
  });

  test('the plain edit and delete pages complete the own-post lifecycle', async ({ page, app }, testInfo) => {
    // Preconditions: board allows editing and self-delete; OP has no replies.
    const board = uniqueShort('njsown', testInfo);
    app.createBoardCli({ short: board, name: 'NoJS Own Post' });
    setBoardFixtureSettings(app, board, { allowEditing: true, allowSelfDelete: true, postCooldownSecs: 0 });

    const subject = marker('nojs-edit');
    await page.goto(`${app.baseURL}/${board}`);
    await submitThreadNoJs(page, board, { subject, body: 'no-js original body' });
    const threadId = await waitForThreadRedirect(page, board);
    const postId = await firstPostId(page);

    const editHref = await page.locator('.self-action-controls .edit-btn').first().getAttribute('href');
    expect(editHref).toBe(`/${board}/post/${postId}/edit`);
    await page.goto(`${app.baseURL}${editHref}`);
    const editForm = page.locator(`form[action$="${editHref}"]`);
    await expect(editForm.locator('textarea[name="body"]')).toHaveValue('no-js original body');
    await editForm.locator('textarea[name="body"]').fill('no-js edited body');
    const editWatch = watchMutations(page, editHref!);
    await editForm.getByRole('button', { name: /save edit/i }).click();
    await page.waitForURL(new RegExp(`/${board}/thread/${threadId}#p${postId}$`));
    expect(editWatch.requests).toHaveLength(1);
    expect(editWatch.statuses).toEqual([303]);
    expect(sqliteQuery(app, `SELECT body FROM posts WHERE id = ${postId};`)).toBe('no-js edited body');

    const deleteHref = await page.locator(`#p${postId} .self-action-controls .del-btn`).getAttribute('href');
    expect(deleteHref).toBe(`/${board}/post/${postId}/delete`);
    await page.goto(`${app.baseURL}${deleteHref}`);
    const deleteForm = page.locator(`form[action$="${deleteHref}"]`);
    await expect(deleteForm).toBeVisible();
    const deleteWatch = watchMutations(page, deleteHref!);
    await deleteForm.getByRole('button', { name: /delete post/i }).click();
    await page.waitForURL(new RegExp(`/${board}/catalog$`));
    expect(deleteWatch.requests).toHaveLength(1);
    expect(deleteWatch.statuses).toEqual([303]);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE id = ${postId};`)).toBe('0');
    await expectSafePage(page);
  });

  test('catalog report, pin, hide, and unhide work through the server-rendered fallback forms', async ({ page, app }, testInfo) => {
    // Preconditions: public /pub board; one thread on the catalog.
    const subject = marker('nojs-catalog');
    await page.goto(`${app.baseURL}/pub`);
    await submitThreadNoJs(page, 'pub', { subject, body: marker('nojs-catalog-body') });
    const threadId = await waitForThreadRedirect(page, 'pub');

    await page.goto(`${app.baseURL}/pub/catalog`);
    const card = page.locator('.catalog-item').filter({ hasText: subject });
    await expect(card).toBeVisible();

    await test.step('report through the fallback details form with Enter submission', async () => {
      const report = card.locator('.report-fallback-form');
      await report.locator('summary').click();
      await expect(report.getByLabel('reason')).toBeVisible();
      await report.getByLabel('reason').fill('no-js catalog report');
      const reportWatch = watchMutations(page, '/report');
      await report.getByLabel('reason').press('Enter');
      await page.waitForURL(new RegExp(`/pub/thread/${threadId}\\?reported=1`));
      expect(reportWatch.requests).toHaveLength(1);
      expect(reportWatch.statuses).toEqual([303]);
      await expect(page.locator('.post-success-banner')).toContainText(/report submitted/i);
      expect(sqliteQuery(app, 'SELECT COUNT(*) FROM reports;')).toBe('1');
    });

    await test.step('pin and unpin through the fallback form', async () => {
      await page.goto(`${app.baseURL}/pub/catalog`);
      const fallback = card.locator('details.catalog-thread-fallback-actions');
      await fallback.getByRole('button', { name: 'Pin thread', exact: true }).click();
      await expect(card).toHaveAttribute('data-pinned', '1');
      expect(sqliteQuery(app, `SELECT pinned FROM user_thread_preferences WHERE thread_id = ${threadId};`)).toBe('1');
      await fallback.getByRole('button', { name: 'Unpin thread', exact: true }).click();
      await expect(card).toHaveAttribute('data-pinned', '0');
      expect(Number(sqliteQuery(app, `SELECT COUNT(*) FROM user_thread_preferences WHERE thread_id = ${threadId};`))).toBe(0);
    });

    await test.step('hide and unhide through the fallback form and the hidden page', async () => {
      await page.goto(`${app.baseURL}/pub/catalog`);
      const fallback = card.locator('details.catalog-thread-fallback-actions');
      await fallback.getByRole('button', { name: 'Hide thread', exact: true }).click();
      await expect(page).toHaveURL(/\/pub\/catalog$/);
      expect(sqliteQuery(app, `SELECT hidden FROM user_thread_preferences WHERE thread_id = ${threadId};`)).toBe('1');
      await expect(card).toHaveCount(0);

      await page.goto(`${app.baseURL}/pub/hidden`);
      const unhide = page.getByRole('button', { name: 'Unhide thread', exact: true }).first();
      await expect(unhide).toBeVisible();
      await unhide.click();
      await expect(page).toHaveURL(/\/pub\/catalog$/);
      await expect(card).toBeVisible();
      expect(Number(sqliteQuery(app, `SELECT COUNT(*) FROM user_thread_preferences WHERE thread_id = ${threadId};`))).toBe(0);
      await expectSafePage(page);
    });
  });

  test('poll creation and noscript voting render one voting form and one persisted vote', async ({ page, app }) => {
    // Preconditions: public /pub board.
    const optionThree = marker('nojs-poll-option-3');
    await page.goto(`${app.baseURL}/pub`);
    await page.locator('.poll-creator > summary').click();
    await page.getByText('More poll options', { exact: true }).click();
    await page.locator('textarea[name="body"]').fill(marker('nojs-poll-body'));
    await page.locator('input[name="poll_question"]').fill('No-JS poll question');
    await page.getByRole('textbox', { name: 'poll option 1', exact: true }).fill('No-JS option one');
    await page.getByRole('textbox', { name: 'poll option 2', exact: true }).fill('No-JS option two');
    await page.getByRole('textbox', { name: 'poll option 3', exact: true }).fill(optionThree);
    const createWatch = watchMutations(page, '/pub');
    await page.getByRole('button', { name: /post thread/i }).click();
    await waitForThreadRedirect(page, 'pub');
    expect(createWatch.requests).toHaveLength(1);
    expect(createWatch.statuses).toEqual([303]);

    await expect(page.locator('.poll-container')).toContainText(optionThree);
    await expect(page.locator('.poll-container .poll-vote-form')).toHaveCount(1);
    await expect(page.locator('.poll-vote-form button[type="submit"]')).toHaveCount(1);
    await expect(page.locator('.poll-vote-option input[name="option_id"]')).toHaveCount(3);
    await expect(page.locator('.poll-add-btn')).toBeHidden();

    const pollId = Number(sqliteQuery(app, 'SELECT id FROM polls ORDER BY id DESC LIMIT 1;'));
    const option = Number(sqliteQuery(app, `SELECT id FROM poll_options WHERE poll_id = ${pollId} ORDER BY id ASC LIMIT 1;`));
    const voteForm = page.locator('.poll-vote-form');
    await voteForm.locator(`input[name="option_id"][value="${option}"]`).check();
    const voteWatch = watchMutations(page, '/vote');
    await voteForm.getByRole('button', { name: /cast vote/i }).click();
    await page.waitForURL(/\/pub\/thread\/\d+#poll$/);
    expect(voteWatch.requests).toHaveLength(1);
    expect(voteWatch.statuses).toEqual([303]);
    await expect(page.locator('.poll-container')).toContainText('1 total vote');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM poll_votes WHERE poll_id = ${pollId};`)).toBe('1');
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.locator('.poll-container')).toContainText('1 total vote');
    await expectSafePage(page);
  });

  test('preferences forms and theme links apply immediately and persist by cookie', async ({ page, app }) => {
    // Preconditions: public /pub board.
    await page.goto(`${app.baseURL}/pub`);
    await page.locator('.user-preferences-summary').click();
    const preferences = page.locator('.user-preferences-noscript');

    const themeWatch = watchMutations(page, '/preferences');
    await preferences.locator('button[name="theme"][value="forest"]').click();
    await page.waitForURL(/\/pub$/);
    expect(themeWatch.requests).toHaveLength(1);
    expect(themeWatch.statuses).toEqual([303]);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'forest');
    let cookies = await page.context().cookies(app.baseURL);
    expect(cookies.find((cookie) => cookie.name === 'rustchan_theme')?.value).toBe('forest');

    await page.locator('.user-preferences-summary').click();
    const badgesWatch = watchMutations(page, '/preferences');
    await preferences.locator('button[name="show_activity_badges"][value="0"]').click();
    await page.waitForURL(/\/pub$/);
    expect(badgesWatch.requests).toHaveLength(1);
    expect(badgesWatch.statuses).toEqual([303]);
    cookies = await page.context().cookies(app.baseURL);
    expect(cookies.find((cookie) => cookie.name === 'rustchan_activity_badges')?.value).toBe('0');

    await page.goto(`${app.baseURL}/theme/blue-sky?return_to=/pub`);
    await expect(page).toHaveURL(/\/pub$/);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
    await expectSafePage(page);
  });

  test('admin login, a site-setting save, a board-setting save, and a lock all work through plain forms', async ({ page, app }, testInfo) => {
    // Preconditions: seeded admin; a thread exists for the moderation action.
    const board = 'pub';
    const siteName = marker('nojs-site');
    const boardName = marker('nojs-board-name');
    await page.goto(`${app.baseURL}/${board}`);
    await submitThreadNoJs(page, board, { subject: marker('nojs-mod-thread'), body: marker('nojs-mod-body') });
    const threadId = await waitForThreadRedirect(page, board);

    await adminLogin(page, app);

    await test.step('site settings save through the plain form', async () => {
      await page.goto(`${app.baseURL}/admin/panel#site-settings`);
      const form = page.locator('form.admin-site-settings-form');
      await expect(form.locator('input[name="site_name"]')).toBeVisible();
      await form.locator('input[name="site_name"]').fill(siteName);
      const watch = watchMutations(page, '/admin/site/settings');
      await form.getByRole('button', { name: /save settings/i }).click();
      await page.waitForURL(/\/admin\/panel/);
      expect(watch.requests).toHaveLength(1);
      expect(watch.statuses).toEqual([303]);
      expect(sqliteQuery(app, `SELECT value FROM site_settings WHERE key = 'site_name';`)).toBe(siteName);
    });

    await test.step('board settings save through the plain form', async () => {
      await page.goto(`${app.baseURL}/admin/panel#board-${board}`);
      const details = page.locator(`details#board-${board}`);
      if ((await details.getAttribute('open')) === null) {
        await details.locator('summary').first().click();
      }
      await expect(details).toHaveAttribute('open', '');
      const form = details.locator('form.board-settings-form');
      await form.locator('input[name="name"]').fill(boardName);
      const watch = watchMutations(page, '/admin/board/settings');
      await form.getByRole('button', { name: /save settings/i }).click();
      await page.waitForURL(/\/admin\/panel/);
      expect(watch.requests).toHaveLength(1);
      expect(watch.statuses).toEqual([303]);
      expect(sqliteQuery(app, `SELECT name FROM boards WHERE short_name = '${board}';`)).toBe(boardName);
    });

    await test.step('a moderation lock posts through the plain admin toolbar form', async () => {
      await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
      const lockForm = page.locator('form[action="/admin/thread/action"]:has(input[name="action"][value="lock"])').first();
      await expect(lockForm).toBeVisible();
      const watch = watchMutations(page, '/admin/thread/action');
      const [lockResponse] = await Promise.all([
        page.waitForResponse((response) => (
          new URL(response.url()).pathname === '/admin/thread/action' && response.request().method() === 'POST'
        )),
        lockForm.getByRole('button').click(),
      ]);
      expect(lockResponse.status()).toBe(303);
      expect(watch.requests).toHaveLength(1);
      expect(watch.statuses).toEqual([303]);
      expect(sqliteQuery(app, `SELECT locked FROM threads WHERE id = ${threadId};`)).toBe('1');
      await page.reload({ waitUntil: 'domcontentloaded' });
      await expect(page.locator('body')).toContainText(/locked/i);
    });
    await expectSafePage(page, { allowAdminInternals: true });
  });

  test('an invalid form re-renders with an understandable error and preserves the other fields', async ({ page, app }, testInfo) => {
    // Preconditions: public /pub board; body is required when no file is attached.
    const subject = marker('nojs-preserve-subject');
    await page.goto(`${app.baseURL}/pub`);
    const form = page.locator('form[action="/pub"]').first();
    await form.locator('input[name="subject"]').fill(subject);
    await form.locator('textarea[name="body"]').fill('');

    const watch = watchMutations(page, '/pub');
    expectHttpError({
      method: 'POST',
      path: '/pub',
      status: 422,
      reason: 'an empty body re-renders the form with a validation error',
    });
    await form.getByRole('button', { name: /post thread/i }).click();

    await expect(page.locator('.post-error-banner').first()).toContainText('Post must include either text or an attached file.');
    await expect(page.locator(`form[action="/pub"] input[name="subject"]`)).toHaveValue(subject);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE subject = '${subject}';`)).toBe('0');

    const corrected = page.locator('form[action="/pub"]').first();
    await corrected.locator('textarea[name="body"]').fill('corrected body after validation');
    await corrected.getByRole('button', { name: /post thread/i }).click();
    const threadId = await waitForThreadRedirect(page, 'pub');
    expect(watch.statuses).toEqual([422, 303]);
    expect(sqliteQuery(app, `SELECT subject FROM threads WHERE id = ${threadId};`)).toBe(subject);
    expect(sqliteQuery(app, `SELECT body FROM posts WHERE thread_id = ${threadId} AND is_op = 1;`)).toBe('corrected body after validation');
    await expectSafePage(page);
  });

  test('refresh, Back, and Forward after a successful POST never duplicate rows', async ({ page, app }) => {
    // Preconditions: public /pub board; a thread with exactly one reply.
    const subject = marker('nojs-history');
    await page.goto(`${app.baseURL}/pub`);
    await submitThreadNoJs(page, 'pub', { subject, body: marker('nojs-history-body') });
    const threadId = await waitForThreadRedirect(page, 'pub');
    const postsAfterCreate = Number(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`));

    const replyForm = page.locator(`form[action="/pub/thread/${threadId}"]`).first();
    await replyForm.locator('textarea[name="body"]').fill(marker('nojs-history-reply'));
    await replyForm.getByRole('button', { name: /post reply/i }).click();
    await waitForReplyRedirect(page, 'pub', threadId);
    const postsAfterReply = Number(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`));
    expect(postsAfterReply).toBe(postsAfterCreate + 1);

    await page.reload({ waitUntil: 'domcontentloaded' });
    expect(Number(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`))).toBe(postsAfterReply);

    await page.getByRole('link', { name: '[ Return ]', exact: true }).first().click();
    await expect(page).toHaveURL(/\/pub$/);
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await expect(page).toHaveURL(new RegExp(`/pub/thread/${threadId}`));
    await expect(page.locator('.post')).toHaveCount(postsAfterReply);
    await page.goForward({ waitUntil: 'domcontentloaded' });
    await expect(page).toHaveURL(/\/pub$/);
    expect(Number(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`))).toBe(postsAfterReply);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE subject = '${subject}';`)).toBe('1');
    await expectSafePage(page);
  });

  test('thread-creation parity: JS and no-JS paths persist the same row shape with exactly one request each', async ({ page, browser, app }, testInfo) => {
    // Preconditions: public /pub board, independent subjects for each path. The
    // JS path must explicitly enable JavaScript because the project default is off.
    const noJsSubject = marker('parity-thread-nojs');
    const jsSubject = marker('parity-thread-js');
    await page.goto(`${app.baseURL}/pub`);
    await expect(page.locator('form[action="/pub"]')).toHaveCount(1);
    const noJsWatch = watchMutations(page, '/pub');
    await submitThreadNoJs(page, 'pub', { subject: noJsSubject, body: 'parity thread no-js body' });
    const noJsThreadId = await waitForThreadRedirect(page, 'pub');
    expect(noJsWatch.requests).toHaveLength(1);
    expect(noJsWatch.statuses).toEqual([303]);

    const jsContext = await newAuditedContext(browser, { javaScriptEnabled: true });
    const jsPage = await jsContext.newPage();
    let jsThreadId = 0;
    try {
      await jsPage.goto(`${app.baseURL}/pub`);
      await expect(jsPage.locator('html')).not.toHaveClass(/no-js/);
      await expect(jsPage.locator('form[action="/pub"]')).toHaveCount(1);
      const jsWatch = watchMutations(jsPage, '/pub');
      await createThread(jsPage, app, 'pub', { subject: jsSubject, body: 'parity thread js body' });
      jsThreadId = threadIdFromUrl(jsPage.url());
      await expect.poll(() => jsWatch.statuses.length).toBe(1);
      expect(jsWatch.requests).toHaveLength(1);
      expect(jsWatch.statuses).toEqual([303]);
      expect(jsThreadId).not.toBe(noJsThreadId);
    } finally {
      await jsContext.close();
    }

    await test.step('both paths produce identical persisted rows and redirect contracts', async () => {
      for (const [subject, body, threadId] of [
        [noJsSubject, 'parity thread no-js body', noJsThreadId],
        [jsSubject, 'parity thread js body', jsThreadId],
      ] as const) {
        expect(sqliteQuery(app, `SELECT subject FROM threads WHERE subject = '${subject}';`)).toBe(subject);
        expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE body = '${body}' AND is_op = 1;`)).toBe('1');
        expect(sqliteQuery(app, `SELECT board_id FROM threads WHERE id = ${threadId};`)).toBe(
          sqliteQuery(app, `SELECT id FROM boards WHERE short_name = 'pub';`),
        );
      }
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE subject IN ('${noJsSubject}', '${jsSubject}');`)).toBe('2');
    });
  });

  test('reply parity: JS and no-JS replies create the same reply rows with one request each', async ({ page, browser, app }, testInfo) => {
    // Preconditions: two independent threads on /pub.
    const noJsReply = marker('parity-reply-nojs');
    const jsReply = marker('parity-reply-js');
    await page.goto(`${app.baseURL}/pub`);
    await submitThreadNoJs(page, 'pub', { subject: marker('parity-reply-thread-nojs'), body: marker('parity-op-nojs') });
    const noJsThreadId = await waitForThreadRedirect(page, 'pub');
    const noJsForm = page.locator(`form[action="/pub/thread/${noJsThreadId}"]`).first();
    await noJsForm.locator('textarea[name="body"]').fill(noJsReply);
    const noJsWatch = watchMutations(page, `/pub/thread/${noJsThreadId}`);
    await noJsForm.getByRole('button', { name: /post reply/i }).click();
    await waitForReplyRedirect(page, 'pub', noJsThreadId);
    expect(noJsWatch.requests).toHaveLength(1);
    expect(noJsWatch.statuses).toEqual([303]);

    const jsContext = await newAuditedContext(browser, { javaScriptEnabled: true });
    const jsPage = await jsContext.newPage();
    let jsThreadId = 0;
    try {
      jsThreadId = await createThread(jsPage, app, 'pub', { subject: marker('parity-reply-thread-js'), body: marker('parity-op-js') });
      const replyToggle = jsPage.locator('[data-action="toggle-post-form"]').first();
      if (await replyToggle.isVisible()) {
        await replyToggle.click();
      }
      const jsForm = jsPage.locator(`form[action="/pub/thread/${jsThreadId}"]`).first();
      await expect(jsForm.locator('textarea[name="body"]')).toBeVisible();
      await jsForm.locator('textarea[name="body"]').fill(jsReply);
      const jsWatch = watchMutations(jsPage, `/pub/thread/${jsThreadId}`);
      await Promise.all([
        jsPage.waitForURL(new RegExp(`/pub/thread/${jsThreadId}`)),
        jsForm.getByRole('button', { name: /post reply/i }).click(),
      ]);
      await expect.poll(() => jsWatch.statuses.length).toBe(1);
      expect(jsWatch.requests).toHaveLength(1);
      expect(jsWatch.statuses).toEqual([303]);
    } finally {
      await jsContext.close();
    }

    await test.step('both replies exist with the same shape and distinct ids', async () => {
      const noJsReplyId = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${noJsThreadId} AND is_op = 0;`));
      const jsReplyId = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${jsThreadId} AND is_op = 0;`));
      expect(noJsReplyId).toBeGreaterThan(0);
      expect(jsReplyId).toBeGreaterThan(0);
      expect(noJsReplyId).not.toBe(jsReplyId);
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${noJsThreadId};`)).toBe('2');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${jsThreadId};`)).toBe('2');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE body = '${noJsReply}';`)).toBe('1');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE body = '${jsReply}';`)).toBe('1');
      expect(sqliteQuery(app, `SELECT reply_count FROM threads WHERE id = ${noJsThreadId};`)).toBe('1');
      expect(sqliteQuery(app, `SELECT reply_count FROM threads WHERE id = ${jsThreadId};`)).toBe('1');
    });
  });

  test('catalog pin parity: the no-JS fallback and the JS menu write the same preference with one request each', async ({ page, browser, app }, testInfo) => {
    // Preconditions: two independent threads on /pub/catalog.
    const noJsSubject = marker('parity-pin-nojs');
    const jsSubject = marker('parity-pin-js');
    await page.goto(`${app.baseURL}/pub`);
    await submitThreadNoJs(page, 'pub', { subject: noJsSubject, body: marker('parity-pin-body-nojs') });
    const noJsThreadId = await waitForThreadRedirect(page, 'pub');

    await page.goto(`${app.baseURL}/pub/catalog`);
    const noJsCard = page.locator('.catalog-item').filter({ hasText: noJsSubject });
    const noJsWatch = watchMutations(page, '/pub/thread-preference');
    await noJsCard.locator('details.catalog-thread-fallback-actions').getByRole('button', { name: 'Pin thread', exact: true }).click();
    await expect(noJsCard).toHaveAttribute('data-pinned', '1');
    expect(noJsWatch.requests).toHaveLength(1);
    expect(noJsWatch.statuses).toEqual([303]);

    const jsContext = await newAuditedContext(browser, { javaScriptEnabled: true });
    const jsPage = await jsContext.newPage();
    let jsThreadId = 0;
    try {
      jsThreadId = await createThread(jsPage, app, 'pub', { subject: jsSubject, body: marker('parity-pin-body-js') });
      await jsPage.goto(`${app.baseURL}/pub/catalog`);
      const jsCard = jsPage.locator('.catalog-item').filter({ hasText: jsSubject });
      await jsCard.locator('.catalog-thread-menu-toggle').click();
      const jsWatch = watchMutations(jsPage, '/pub/thread-preference');
      await jsCard.locator('.catalog-thread-menu:not([hidden])').getByRole('button', { name: 'Pin thread', exact: true }).click();
      await expect(jsCard).toHaveAttribute('data-pinned', '1');
      await expect.poll(() => jsWatch.statuses.length).toBe(1);
      expect(jsWatch.requests).toHaveLength(1);
      expect(jsWatch.statuses).toEqual([303]);
    } finally {
      await jsContext.close();
    }

    expect(sqliteQuery(app, `SELECT pinned FROM user_thread_preferences WHERE thread_id = ${noJsThreadId};`)).toBe('1');
    expect(sqliteQuery(app, `SELECT pinned FROM user_thread_preferences WHERE thread_id = ${jsThreadId};`)).toBe('1');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM user_thread_preferences WHERE thread_id IN (${noJsThreadId}, ${jsThreadId});`)).toBe('2');
  });

  test('the no-JS unlock flow rejects a wrong password and then grants access', async ({ page, app }, testInfo) => {
    // Preconditions: view_password board whose password hash matches ADMIN_PASSWORD.
    const board = uniqueShort('njsunlock', testInfo);
    const bodyMarker = marker('nojs-unlock-body');
    app.createBoardCli({ short: board, name: 'NoJS Unlock' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
    await page.goto(`${app.baseURL}/${board}`);
    await submitThreadNoJs(page, board, { subject: marker('nojs-unlock-subject'), body: bodyMarker });
    const threadId = await waitForThreadRedirect(page, board);
    setBoardFixtureSettings(app, board, {
      accessMode: 'view_password',
      accessPasswordHash: adminPasswordHash(app),
    });

    await page.context().clearCookies();
    expectHttpError({
      method: 'GET',
      path: `/${board}`,
      status: 403,
      reason: 'a view-password board renders only the gate without the unlock cookie',
    });
    await page.goto(`${app.baseURL}/${board}`);
    await expect(page.locator('body')).toContainText('password protected board');
    await expect(page.locator('body')).not.toContainText(bodyMarker);
    const unlockForm = page.locator(`form[action="/${board}/unlock"]`).first();
    await unlockForm.locator('input[name="password"]').fill('definitely-wrong');
    expectHttpError({
      method: 'POST',
      path: `/${board}/unlock`,
      status: 403,
      reason: 'a wrong board password re-renders the gate with a 403',
    });
    await unlockForm.getByRole('button', { name: /unlock board/i }).click();
    await expect(page.locator('body')).toContainText('Incorrect board password.');
    await expect(page.locator('body')).not.toContainText(bodyMarker);

    const retry = page.locator(`form[action="/${board}/unlock"]`).first();
    await retry.locator('input[name="password"]').fill(ADMIN_PASSWORD);
    await retry.getByRole('button', { name: /unlock board/i }).click();
    await page.waitForURL(new RegExp(`/${board}$`));
    await expect(page.locator('body')).toContainText(bodyMarker);
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    await expect(page.locator('body')).toContainText(bodyMarker);
    await expectSafePage(page);
  });

  test('the no-JS captcha path renders a challenge, refreshes it, and reports the server-rendered failure', async ({ page, app }, testInfo) => {
    // Preconditions: captcha-enabled board.
    const board = uniqueShort('njscap', testInfo);
    app.createBoardCli({ short: board, name: 'NoJS Captcha' });
    setBoardFixtureSettings(app, board, { allowCaptcha: true, postCooldownSecs: 0 });

    await page.goto(`${app.baseURL}/${board}`);
    const form = page.locator(`form[action="/${board}"]`).first();
    const firstCaptchaId = await form.locator('input[name="captcha_id"]').getAttribute('value');
    expect(firstCaptchaId).toMatch(/^[a-f0-9]{32}$/);
    await expect(form.locator('.captcha-image')).toBeVisible();
    const imageSrc = await form.locator('.captcha-image').getAttribute('src');
    expect(imageSrc).toMatch(new RegExp(`^/captcha/[a-f0-9]{32}\\?board=${board}`));
    const image = await page.request.get(`${app.baseURL}${imageSrc}`);
    expect(image.status()).toBe(200);
    expect(image.headers()['content-type']).toContain('image/png');

    await form.locator('.captcha-refresh-link').click();
    await page.waitForURL(new RegExp(`/${board}\\?captcha_refresh=[a-f0-9]{32}$`));
    const refreshedForm = page.locator(`form[action="/${board}"]`).first();
    const refreshedCaptchaId = await refreshedForm.locator('input[name="captcha_id"]').getAttribute('value');
    expect(refreshedCaptchaId).toMatch(/^[a-f0-9]{32}$/);
    expect(refreshedCaptchaId).not.toBe(firstCaptchaId);

    await refreshedForm.locator('input[name="subject"]').fill(marker('nojs-captcha'));
    await refreshedForm.locator('textarea[name="body"]').fill('no-js captcha body');
    await refreshedForm.locator('input[name="captcha_answer"]').fill('WRONG');
    expectHttpError({
      method: 'POST',
      path: `/${board}`,
      status: 422,
      reason: 'a wrong captcha answer re-renders the form with an inline error',
    });
    await refreshedForm.getByRole('button', { name: /post thread/i }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/CAPTCHA verification failed/i);
    const errorCaptchaId = await page.locator(`form[action="/${board}"] input[name="captcha_id"]`).first().getAttribute('value');
    expect(errorCaptchaId).toMatch(/^[a-f0-9]{32}$/);
    expect(errorCaptchaId).not.toBe(refreshedCaptchaId);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE subject = 'nojs-captcha';`)).toBe('0');
    await expectSafePage(page);
  });

  test('the no-JS file-upload path stores and serves a file, and a disallowed type shows a readable error', async ({ page, app }, testInfo) => {
    // Preconditions: img board accepts images only; PDFs are disabled there.
    const subject = marker('nojs-upload');
    await page.goto(`${app.baseURL}/img`);
    const form = page.locator('form[action="/img"]').first();
    await form.locator('input[name="subject"]').fill(subject);
    await form.locator('textarea[name="body"]').fill(marker('nojs-upload-body'));
    await form.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPng);
    await form.getByRole('button', { name: /post thread/i }).click();
    await waitForThreadRedirect(page, 'img');
    await expect(page.locator('.post.op .file-info')).toContainText('tiny.png');
    const mediaHref = await page.locator('.post.op .file-info a').first().getAttribute('href');
    expect(mediaHref).toMatch(/^\/boards\/img\//);
    const media = await page.request.get(`${app.baseURL}${mediaHref}`);
    expect(media.status()).toBe(200);
    expect(media.headers()['content-type']).toMatch(/^image\/png/);
    expect(media.headers()['x-content-type-options']).toBe('nosniff');

    await page.goto(`${app.baseURL}/img`);
    const rejectBody = marker('nojs-upload-reject-body');
    const rejectForm = page.locator('form[action="/img"]').first();
    await rejectForm.locator('input[name="subject"]').fill(marker('nojs-upload-reject'));
    await rejectForm.locator('textarea[name="body"]').fill(rejectBody);
    await rejectForm.locator('input[type="file"]').first().setInputFiles(app.fixtures().tinyPdf);
    expectHttpError({
      method: 'POST',
      path: '/img',
      status: 422,
      reason: 'a PDF on an image-only board re-renders with an inline error',
    });
    await rejectForm.getByRole('button', { name: /post thread/i }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/pdf uploads are disabled|only accepts|not allowed/i);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE body = '${rejectBody}';`)).toBe('0');
    await expectSafePage(page);
  });
});
