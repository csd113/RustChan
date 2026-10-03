/**
 * Negative-testing / permissions audit for RustChan.
 *
 * Scope:
 *  - owner-vs-unrelated-actor authority for self-service post edit/delete
 *  - CSRF (public double-submit/signed tokens, admin session-scoped tokens)
 *  - Origin/Referer policy for admin mutations
 *  - anonymous access to every admin mutation route
 *  - stale/banked admin sessions
 *  - tampered identifiers and forged ownership grants
 *  - view-password / post-password board isolation
 *  - bans and appeals
 *  - escaped user content and hostile upload filenames
 *  - sensitive-data exposure and Set-Cookie attributes
 *
 * Transport note for the strict diagnostics layer: direct crafted requests go
 * through Playwright APIRequestContext (`page.request` / `request.newContext`).
 * Those responses are deliberately not observed as page traffic, so they do not
 * need `expectHttpError` declarations. Declarations are used only where a real
 * browser navigation receives the 4xx response, and each declared path is
 * triggered exactly once. Every denial asserts both the response contract and
 * the absence of the persisted state change (SQLite read).
 *
 * Preconditions are listed per test. Fresh isolated RustChan per test via the
 * `app` fixture (admin + boards pub,img,vid,aud,nsfw,txt) unless a dedicated
 * standalone instance is created for ban tests.
 */
import { randomBytes } from 'node:crypto';
import fsp from 'node:fs/promises';
import path from 'node:path';
import {
  actor,
  expectHttpError,
  newAuditedContext,
  noteEvent,
  request,
  type APIRequestContext,
  type APIResponse,
  type Page,
} from './diagnostics';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  adminCsrf,
  adminLogin,
  adminLogout,
  adminPasswordHash,
  createReply,
  createStandaloneApp,
  createThread,
  expect,
  expectNoDialog,
  expectSafePage,
  expectSafeResponse,
  expectServerLogError,
  extractCsrf,
  publicCsrf,
  setBoardFixtureSettings,
  sqliteQuery,
  test,
  uniqueShort,
  updateBoardSettings,
  type RustChanServer,
} from './helpers';

const EDIT_DENIED = 'Edit permission for this post is no longer available in this browser.';
const DELETE_DENIED = 'Delete permission for this post is no longer available in this browser.';
const CSRF_DENIED = 'CSRF token mismatch.';
const ADMIN_LOGIN_DENIED = 'Not logged in.';
const SESSION_EXPIRED = 'Session expired or invalid.';
const ORIGIN_MISMATCH = 'Origin/Referer origin mismatch.';

type OwnedGrant = {
  post_id: number;
  thread_id: number;
  board_short: string;
  deletion_token: string;
  expires_at: number;
};

function marker(prefix: string): string {
  return `${prefix}_${randomBytes(6).toString('hex')}`;
}

async function postIdFrom(page: Page, selector = '.post.op'): Promise<number> {
  const id = await page.locator(selector).first().getAttribute('id');
  const numeric = Number(id?.replace(/^p/, ''));
  expect(Number.isInteger(numeric) && numeric > 0, `post id for ${selector}`).toBe(true);
  return numeric;
}

async function postIdOf(page: Page, selector: string): Promise<number> {
  return postIdFrom(page, selector);
}

function tableCount(app: RustChanServer, table: string, where = ''): number {
  return Number(sqliteQuery(app, `SELECT COUNT(*) FROM ${table}${where ? ` WHERE ${where}` : ''};`));
}

function bodyOf(app: RustChanServer, postId: number): string {
  return sqliteQuery(app, `SELECT body FROM posts WHERE id = ${postId};`);
}

function decodeOwnedGrants(cookieValue: string): OwnedGrant[] {
  const payloadHex = cookieValue.split('.')[0] ?? '';
  const parsed = JSON.parse(Buffer.from(payloadHex, 'hex').toString('utf8')) as { grants: OwnedGrant[] };
  return parsed.grants;
}

function encodeOwnedPayload(grants: unknown[]): string {
  return Buffer.from(JSON.stringify({ grants }), 'utf8').toString('hex');
}

async function signedCsrf(api: APIRequestContext, pathname: string): Promise<string> {
  const response = await api.get(pathname);
  expect(response.status(), `GET ${pathname} for a signed CSRF token`).toBe(200);
  return extractCsrf(await response.text());
}

/** Open the collapsed new-thread form on a board index before using its fields. */
async function openPostForm(page: Page, board: string): Promise<void> {
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  await expect(page.locator(`form[action="/${board}"] textarea[name="body"]`)).toBeVisible();
}

function setCookieValue(response: APIResponse, name: string): string | undefined {
  return response
    .headersArray()
    .filter((header) => header.name.toLowerCase() === 'set-cookie')
    .map((header) => header.value)
    .find((value) => value.startsWith(`${name}=`));
}

async function submitThread(
  api: APIRequestContext,
  board: string,
  csrf: string | undefined,
  submissionToken: string,
  body: string,
): Promise<APIResponse> {
  const multipart: Record<string, string> = {
    submission_token: submissionToken,
    body,
  };
  if (csrf !== undefined) {
    multipart._csrf = csrf;
  }
  return api.post(`/${board}`, { multipart, maxRedirects: 0 });
}

const ADMIN_MUTATION_ROUTES: Array<{ path: string; multipartField?: string }> = [
  { path: '/admin/board/create' },
  { path: '/admin/board/delete' },
  { path: '/admin/board/settings' },
  { path: '/admin/board/reorder' },
  { path: '/admin/site/settings' },
  { path: '/admin/media/settings' },
  { path: '/admin/theme/create' },
  { path: '/admin/theme/update' },
  { path: '/admin/theme/delete' },
  { path: '/admin/backup/create' },
  { path: '/admin/backup/settings' },
  { path: '/admin/backup/delete' },
  { path: '/admin/board/backup/create' },
  { path: '/admin/backup/restore-saved' },
  { path: '/admin/board/backup/restore-saved' },
  { path: '/admin/backup/extract-board' },
  { path: '/admin/db/check' },
  { path: '/admin/db/repair' },
  { path: '/admin/vacuum' },
  { path: '/admin/ban/add' },
  { path: '/admin/ban/remove' },
  { path: '/admin/thread/action' },
  { path: '/admin/thread/delete' },
  { path: '/admin/post/delete' },
  { path: '/admin/post/ban-delete' },
  { path: '/admin/report/resolve' },
  { path: '/admin/filter/add' },
  { path: '/admin/filter/remove' },
  { path: '/admin/appeal/accept' },
  { path: '/admin/appeal/dismiss' },
  { path: '/admin/setup/reopen' },
  { path: '/admin/setup/close' },
  { path: '/admin/site-health/jobs/dismiss' },
  { path: '/admin/board/favicon/clear' },
  { path: '/admin/board/banner/clear' },
  { path: '/admin/banner/update' },
  { path: '/admin/banner/delete' },
  { path: '/admin/banner/move' },
  { path: '/admin/site/favicon', multipartField: 'favicon' },
  { path: '/admin/board/favicon', multipartField: 'favicon' },
  { path: '/admin/site/banner', multipartField: 'banner' },
  { path: '/admin/home/banner', multipartField: 'banner' },
  { path: '/admin/board/banner', multipartField: 'banner' },
  // /admin/restore and /admin/board/restore are intentionally NOT in this list:
  // their anonymous denial is a 200 "Redirecting" interstitial plus a server
  // [ERROR] line, asserted separately in the matrix test.
];

function adminDenialText(body: string): boolean {
  return body.includes(CSRF_DENIED)
    || body.includes('Missing Origin/Referer header.')
    || body.includes(ADMIN_LOGIN_DENIED);
}

function adminStateSnapshot(app: RustChanServer): Record<string, string> {
  const snapshot: Record<string, string> = {};
  for (const table of [
    'boards',
    'threads',
    'posts',
    'bans',
    'ban_appeals',
    'reports',
    'word_filters',
    'themes',
    'admin_sessions',
    'post_submissions',
    'site_settings',
    'mod_log',
    'polls',
    'poll_options',
    'poll_votes',
    'banner_assets',
  ]) {
    snapshot[table] = sqliteQuery(app, `SELECT COUNT(*) FROM ${table};`);
  }
  snapshot.site_name = sqliteQuery(app, `SELECT value FROM site_settings WHERE key = 'site_name';`);
  snapshot.pub_name = sqliteQuery(app, `SELECT name FROM boards WHERE short_name = 'pub';`);
  return snapshot;
}

async function backupFiles(app: RustChanServer): Promise<string[]> {
  const dir = path.join(app.dataDir, 'backups');
  try {
    return (await fsp.readdir(dir, { recursive: true })).sort();
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
      return [];
    }
    throw error;
  }
}

test.describe('permissions, CSRF, and ownership', () => {
  test('an unrelated actor cannot edit my post even with a valid CSRF token', async ({ page, browser, app }, testInfo) => {
    // Preconditions: board allows editing and self-delete; anonymous actor.
    actor('owner');
    const board = uniqueShort('ownedit', testInfo);
    app.createBoardCli({ short: board, name: 'Ownership Edit' });
    setBoardFixtureSettings(app, board, { allowEditing: true, allowSelfDelete: true, postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, { subject: marker('owner-subject'), body: 'original owner body' });
    const postId = await postIdFrom(page);
    const editHref = `/${board}/post/${postId}/edit`;

    await test.step('the owner edits through the real page', async () => {
      await page.goto(`${app.baseURL}${editHref}`);
      const form = page.locator(`form[action$="${editHref}"]`);
      await expect(form.locator('textarea[name="body"]')).toHaveValue('original owner body');
      await form.locator('textarea[name="body"]').fill('owner edited body');
      await Promise.all([
        page.waitForURL(new RegExp(`/${board}/thread/${threadId}#p${postId}$`)),
        form.getByRole('button', { name: /save edit/i }).click(),
      ]);
      expect(bodyOf(app, postId)).toBe('owner edited body');
    });

    actor('other');
    const otherContext = await newAuditedContext(browser);
    const other = await otherContext.newPage();
    try {
      await test.step('the unrelated browser is denied the edit page', async () => {
        expectHttpError({
          method: 'GET',
          path: editHref,
          status: 403,
          reason: 'without the ownership cookie the edit page must fail closed',
        });
        await other.goto(`${app.baseURL}${editHref}`);
        const body = await other.locator('body').innerText();
        expect(body).toContain(EDIT_DENIED);
        expect(body).not.toContain('original owner body');
        expect(body).not.toContain('owner edited body');
        expect(await other.locator('textarea[name="body"]').count()).toBe(0);
        expect(other.url()).toContain(editHref);
        await expectSafePage(other);
      });

      await test.step('the crafted POST with the other actor valid CSRF token is denied and persists nothing', async () => {
        const csrf = await publicCsrf(other, app, `/${board}/thread/${threadId}`);
        const response = await other.request.post(`${app.baseURL}${editHref}`, {
          form: { _csrf: csrf, body: 'attacker rewrite' },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(EDIT_DENIED);
        expect(bodyOf(app, postId)).toBe('owner edited body');
      });
    } finally {
      await otherContext.close();
    }
  });

  test('an unrelated actor cannot delete my post and the owner delete still works', async ({ page, browser, app }, testInfo) => {
    // Preconditions: board allows editing and self-delete; anonymous owner; OP has no replies.
    actor('owner');
    const board = uniqueShort('owndel', testInfo);
    app.createBoardCli({ short: board, name: 'Ownership Delete' });
    setBoardFixtureSettings(app, board, { allowEditing: true, allowSelfDelete: true, postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, { subject: marker('delete-subject'), body: 'delete target body' });
    const postId = await postIdFrom(page);
    const deleteHref = `/${board}/post/${postId}/delete`;

    actor('other');
    const otherContext = await newAuditedContext(browser);
    const other = await otherContext.newPage();
    try {
      await test.step('the unrelated browser is denied the delete page', async () => {
        expectHttpError({
          method: 'GET',
          path: deleteHref,
          status: 403,
          reason: 'without the delete grant the confirmation page must fail closed',
        });
        await other.goto(`${app.baseURL}${deleteHref}`);
        const body = await other.locator('body').innerText();
        expect(body).toContain(DELETE_DENIED);
        expect(body).not.toContain('delete target body');
        expect(await other.locator('form[action$="/delete"]').count()).toBe(0);
        await expectSafePage(other);
      });

      await test.step('the crafted POST with a valid CSRF token is denied and keeps the row', async () => {
        const csrf = await publicCsrf(other, app, `/${board}/thread/${threadId}`);
        const response = await other.request.post(`${app.baseURL}${deleteHref}`, {
          form: { _csrf: csrf },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(DELETE_DENIED);
        expect(tableCount(app, 'posts', `id = ${postId}`)).toBe(1);
      });
    } finally {
      await otherContext.close();
    }

    await test.step('the owner deletes the post through the plain confirmation page', async () => {
      await page.goto(`${app.baseURL}${deleteHref}`);
      const form = page.locator(`form[action$="${deleteHref}"]`);
      await expect(form).toBeVisible();
      await Promise.all([
        page.waitForURL(new RegExp(`/${board}(/catalog)?$`)),
        form.getByRole('button', { name: /delete post/i }).click(),
      ]);
      expect(tableCount(app, 'posts', `id = ${postId}`)).toBe(0);
      expect(tableCount(app, 'threads', `id = ${threadId}`)).toBe(0);
    });
  });

  test('forged ownership grants, swapped identifiers, and extreme ids fail closed without cross-record writes', async ({ page, app }, testInfo) => {
    // Preconditions: two editable public boards; owner owns one post; no replies on it.
    actor('owner');
    const board = uniqueShort('tamp', testInfo);
    const swappedBoard = uniqueShort('swap', testInfo);
    app.createBoardCli({ short: board, name: 'Tamper Board' });
    app.createBoardCli({ short: swappedBoard, name: 'Swap Board' });
    setBoardFixtureSettings(app, board, { allowEditing: true, allowSelfDelete: true, postCooldownSecs: 0 });
    setBoardFixtureSettings(app, swappedBoard, { allowEditing: true, allowSelfDelete: true, postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, { subject: marker('tamper-subject'), body: 'tamper target body' });
    const postId = await postIdFrom(page);
    const editHref = `/${board}/post/${postId}/edit`;
    const signed = await publicCsrf(page, app, `/${board}/thread/${threadId}`);
    const ownedCookie = (await page.context().cookies(app.baseURL)).find((cookie) => cookie.name === 'rustchan_owned_posts');
    expect(ownedCookie).toBeTruthy();
    const grant = decodeOwnedGrants(ownedCookie!.value).find((candidate) => candidate.post_id === postId);
    expect(grant, 'ownership grant for the created post').toBeTruthy();
    const signature = ownedCookie!.value.split('.')[1];

    const api = await request.newContext({ baseURL: app.baseURL });
    try {
      await test.step('the genuine signed grant is the positive control', async () => {
        const response = await api.post(editHref, {
          form: { _csrf: signed, body: 'owner verified body' },
          headers: { Cookie: `rustchan_owned_posts=${ownedCookie!.value}` },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(303);
        expect(bodyOf(app, postId)).toBe('owner verified body');
      });

      await test.step('a forged deletion_token under the original signature is rejected', async () => {
        const forged = `${encodeOwnedPayload([{ ...grant!, deletion_token: 'forged-token' }])}.${signature}`;
        const response = await api.post(editHref, {
          form: { _csrf: signed, body: 'forged rewrite' },
          headers: { Cookie: `rustchan_owned_posts=${forged}` },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(EDIT_DENIED);
        expect(bodyOf(app, postId)).toBe('owner verified body');
      });

      await test.step('an empty deletion_token with a stale signature is rejected', async () => {
        const emptied = `${encodeOwnedPayload([{ ...grant!, deletion_token: '' }])}.${signature}`;
        const response = await api.post(editHref, {
          form: { _csrf: signed, body: 'empty-token rewrite' },
          headers: { Cookie: `rustchan_owned_posts=${emptied}` },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(EDIT_DENIED);
        expect(bodyOf(app, postId)).toBe('owner verified body');
      });

      await test.step('a grant for one post cannot be reused on another post', async () => {
        const secondThread = await createThread(page, app, board, { subject: marker('tamper-second'), body: 'second target body' });
        const secondPostId = await postIdFrom(page);
        const secondEditHref = `/${board}/post/${secondPostId}/edit`;
        expect(secondPostId).not.toBe(postId);
        const response = await api.post(secondEditHref, {
          form: { _csrf: signed, body: 'swapped grant rewrite' },
          headers: { Cookie: `rustchan_owned_posts=${ownedCookie!.value}` },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(EDIT_DENIED);
        expect(bodyOf(app, secondPostId)).toBe('second target body');
        expect(tableCount(app, 'threads', `id = ${secondThread}`)).toBe(1);
      });

      await test.step('a post id addressed through another board is a 404, not a cross-board edit', async () => {
        const response = await api.get(`/${swappedBoard}/post/${postId}/edit`, { maxRedirects: 0 });
        expect(response.status()).toBe(404);
        expect(await expectSafeResponse(response)).toContain('Post not found in this board.');
        expect(bodyOf(app, postId)).toBe('owner verified body');
      });

      await test.step('negative and extreme ids are rejected without touching records', async () => {
        const negative = await api.get(`/${board}/post/-1/edit`, { maxRedirects: 0 });
        expect(negative.status()).toBe(404);
        expect(await expectSafeResponse(negative)).toContain('Post not found.');

        const hugePost = await api.get(`/${board}/post/9223372036854775807/delete`, { maxRedirects: 0 });
        expect(hugePost.status()).toBe(404);
        expect(await expectSafeResponse(hugePost)).toContain('Post not found.');

        const csrf = await signedCsrf(api, `/${board}/thread/${threadId}`);
        const hugeThread = await api.post(`/${board}/thread/9223372036854775807`, {
          multipart: { _csrf: csrf, submission_token: marker('huge'), body: 'reply to nowhere' },
          maxRedirects: 0,
        });
        expect(hugeThread.status()).toBe(404);
        expect(await expectSafeResponse(hugeThread)).toContain('Thread not found.');
        expect(tableCount(app, 'posts', `body = 'reply to nowhere'`)).toBe(0);
      });

      await test.step('a swapped board field in thread-preference is a 400 with no preference row', async () => {
        const response = await api.post(`/${board}/thread-preference`, {
          form: { _csrf: signed, thread_id: String(threadId), board: swappedBoard, action: 'pin', return_to: `/${board}` },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(400);
        expect(await expectSafeResponse(response)).toContain('Board mismatch.');
        expect(tableCount(app, 'user_thread_preferences', `thread_id = ${threadId}`)).toBe(0);
      });
    } finally {
      await api.dispose();
    }
  });

  test('public CSRF: missing, empty, unsigned foreign, and signed cross-context tokens follow the documented contract', async ({ page, browser, app }) => {
    // Preconditions: public /pub board, no cooldown. Requests use APIRequestContext
    // so they are not page traffic for the diagnostics layer.
    const api = await request.newContext({ baseURL: app.baseURL });
    const otherContext = await newAuditedContext(browser);
    const other = await otherContext.newPage();
    try {
      const baseline = tableCount(app, 'threads');
      await other.goto(`${app.baseURL}/pub`);
      const otherRawToken = (await otherContext.cookies(app.baseURL))
        .find((cookie) => cookie.name === 'csrf_token')!.value;

      await test.step('missing, empty, and wrong tokens are denied with no row', async () => {
        for (const [label, csrf] of [
          ['missing', undefined],
          ['empty', ''],
          ['wrong', 'bogus.csrf-token'],
        ] as const) {
          const response = await submitThread(api, 'pub', csrf, marker(`csrf-${label}`), `csrf ${label} body`);
          expect(response.status(), `public CSRF ${label}`).toBe(403);
          expect(await expectSafeResponse(response)).toContain(CSRF_DENIED);
        }
        expect(tableCount(app, 'threads')).toBe(baseline);
      });

      await test.step('an unsigned raw token copied from another actor is not accepted', async () => {
        const response = await submitThread(api, 'pub', otherRawToken, marker('raw-foreign'), 'raw foreign body');
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(CSRF_DENIED);
        expect(tableCount(app, 'threads')).toBe(baseline);
      });

      await test.step('a correctly signed public token is portable by design and creates exactly one thread', async () => {
        noteEvent('csrf-contract', 'validate_csrf accepts any correctly signed token without binding the cookie; this is the documented bearer-token contract.');
        const signedFromOther = await publicCsrf(other, app, '/pub');
        const response = await submitThread(api, 'pub', signedFromOther, marker('signed-foreign'), 'signed foreign body');
        expect(response.status()).toBe(303);
        expect(response.headers()['location']).toMatch(/^\/pub\/thread\/\d+/);
        expect(tableCount(app, 'posts', `body = 'signed foreign body' AND is_op = 1`)).toBe(1);
        expect(tableCount(app, 'threads')).toBe(baseline + 1);
      });

      await test.step('the cookie-equality path accepts a raw token only with the matching cookie', async () => {
        const equalityApi = await request.newContext({ baseURL: app.baseURL });
        try {
          const pageResponse = await equalityApi.get('/pub');
          expect(pageResponse.status()).toBe(200);
          const rawToken = (await equalityApi.storageState()).cookies
            .find((cookie) => cookie.name === 'csrf_token')!.value;
          const response = await equalityApi.post('/pub', {
            multipart: { _csrf: rawToken, submission_token: marker('raw-own'), body: 'raw own body' },
            maxRedirects: 0,
          });
          expect(response.status()).toBe(303);
          expect(tableCount(app, 'posts', `body = 'raw own body' AND is_op = 1`)).toBe(1);
        } finally {
          await equalityApi.dispose();
        }
      });
    } finally {
      await otherContext.close();
      await api.dispose();
    }
  });

  test('admin CSRF scope: login/public tokens cannot drive admin mutations and admin tokens cannot drive public mutations', async ({ page, browser, app }, testInfo) => {
    // Preconditions: fresh admin session in the default page.
    actor('admin');
    const loginCsrf = await (async () => {
      const response = await page.request.get(`${app.baseURL}/admin`);
      expect(response.status()).toBe(200);
      return extractCsrf(await response.text());
    })();
    const publicToken = await publicCsrf(page, app, '/pub');
    await adminLogin(page, app);
    const panelCsrf = await adminCsrf(page, app);

    const boardsBefore = tableCount(app, 'boards');
    const threadsBefore = tableCount(app, 'threads');

    await test.step('a login-scoped token is rejected for a session mutation', async () => {
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: { _csrf: loginCsrf, short_name: uniqueShort('lgs', testInfo), name: 'Login Scope', description: '' },
        headers: { Origin: app.baseURL },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(403);
      expect(await expectSafeResponse(response)).toContain(CSRF_DENIED);
      expect(tableCount(app, 'boards')).toBe(boardsBefore);
    });

    await test.step('a signed public token is rejected for a session mutation', async () => {
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: { _csrf: publicToken, short_name: uniqueShort('pbs', testInfo), name: 'Public Scope', description: '' },
        headers: { Origin: app.baseURL },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(403);
      expect(await expectSafeResponse(response)).toContain(CSRF_DENIED);
      expect(tableCount(app, 'boards')).toBe(boardsBefore);
    });

    await test.step('a token from a different admin session is rejected', async () => {
      const siblingContext = await newAuditedContext(browser);
      const sibling = await siblingContext.newPage();
      try {
        await adminLogin(sibling, app);
        const siblingCsrf = await adminCsrf(sibling, app);
        const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
          form: { _csrf: siblingCsrf, short_name: uniqueShort('csx', testInfo), name: 'Cross Session', description: '' },
          headers: { Origin: app.baseURL },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(CSRF_DENIED);
      } finally {
        await siblingContext.close();
      }
      expect(tableCount(app, 'boards')).toBe(boardsBefore);
    });

    await test.step('the admin session-scoped token cannot drive a public thread even with the raw cookie', async () => {
      const rawAdminCsrf = (await page.context().cookies(app.baseURL))
        .find((cookie) => cookie.name === 'csrf_token')!.value;
      const api = await request.newContext({ baseURL: app.baseURL });
      try {
        const response = await api.post('/pub', {
          multipart: { _csrf: panelCsrf, submission_token: marker('admin-scope'), body: 'admin scoped public body' },
          headers: { Cookie: `csrf_token=${rawAdminCsrf}` },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(CSRF_DENIED);
        expect(tableCount(app, 'posts', `body = 'admin scoped public body'`)).toBe(0);
      } finally {
        await api.dispose();
      }
    });
    expect(tableCount(app, 'threads')).toBe(threadsBefore);
  });

  test('admin Origin policy: foreign origin and referer fail closed, null and loopback origins agree with the Host header', async ({ page, app }, testInfo) => {
    actor('admin');
    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);
    const boardsBefore = tableCount(app, 'boards');
    const foreignBody = (short: string) => ({
      _csrf: csrf,
      short_name: short,
      name: 'Origin Audit',
      description: '',
    });

    await test.step('a foreign Origin is rejected even with a valid admin CSRF token', async () => {
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: foreignBody(uniqueShort('for', testInfo)),
        headers: { Origin: 'http://evil.example' },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(403);
      expect(await expectSafeResponse(response)).toContain(ORIGIN_MISMATCH);
      expect(tableCount(app, 'boards')).toBe(boardsBefore);
    });

    await test.step('a foreign Referer is rejected with no Origin header', async () => {
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: foreignBody(uniqueShort('ref', testInfo)),
        headers: { Referer: 'http://evil.example/path' },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(403);
      expect(await expectSafeResponse(response)).toContain(ORIGIN_MISMATCH);
      expect(tableCount(app, 'boards')).toBe(boardsBefore);
    });

    await test.step('a null Origin is accepted on a loopback host and the board is created', async () => {
      // A non-loopback Host cannot be simulated through APIRequestContext:
      // Playwright does not deliver an overridden Host header to the server.
      // The loopback-null acceptance below is therefore the reachable contract;
      // the non-loopback rejection branch is covered by Rust unit tests for
      // require_same_origin_request.
      const short = uniqueShort('nul', testInfo);
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: foreignBody(short),
        headers: { Origin: 'null' },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(303);
      expect(tableCount(app, 'boards', `short_name = '${short}'`)).toBe(1);
    });

    await test.step('a loopback alias origin matching the request host is accepted', async () => {
      const short = uniqueShort('lop', testInfo);
      const response = await page.request.post(`${app.baseURL}/admin/board/create`, {
        form: foreignBody(short),
        headers: {
          Host: `localhost:${app.port}`,
          Origin: `http://127.0.0.1:${app.port}`,
        },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(303);
      expect(tableCount(app, 'boards', `short_name = '${short}'`)).toBe(1);
    });

    await test.step('GET on POST-only mutation routes is method-not-allowed, never a success page', async () => {
      const adminGet = await page.request.get(`${app.baseURL}/admin/board/create`, { maxRedirects: 0 });
      expect(adminGet.status()).toBe(405);
      const adminBody = await expectSafeResponse(adminGet);
      expect(adminBody).not.toContain('board management');

      const publicGet = await page.request.get(`${app.baseURL}/vote`, { maxRedirects: 0 });
      expect(publicGet.status()).toBe(405);
    });
  });

  test('an anonymous attacker cannot reach any admin mutation route or persist anything', async ({ app, browser }, testInfo) => {
    actor('anonymous');
    const fixtureThread = await (async () => {
      const setupContext = await newAuditedContext(browser);
      const setupPage = await setupContext.newPage();
      try {
        return await createThread(setupPage, app, 'pub', { subject: marker('anon-matrix'), body: 'matrix fixture body' });
      } finally {
        await setupContext.close();
      }
    })();
    const fixturePost = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${fixtureThread} AND is_op = 1 LIMIT 1;`));
    const pubBoardId = Number(sqliteQuery(app, `SELECT id FROM boards WHERE short_name = 'pub' LIMIT 1;`));

    const before = adminStateSnapshot(app);
    const backupsBefore = await backupFiles(app);
    const tinyPng = await fsp.readFile(app.fixtures().tinyPng);
    const api = await request.newContext({ baseURL: app.baseURL });
    try {
      const form: Record<string, string> = {
        _csrf: marker('anonymous'),
        board_id: String(pubBoardId),
        thread_id: String(fixtureThread),
        post_id: String(fixturePost),
        banner_id: '1',
        report_id: '1',
        ban_id: '1',
        filter_id: '1',
        appeal_id: '1',
        ip_hash: 'a'.repeat(64),
        reason: 'anonymous audit attempt',
        duration_hours: '0',
        board: 'pub',
        board_short: 'pub',
        action: 'sticky',
        direction: 'up',
        pattern: 'anonymous-audit-pattern',
        replacement: 'anonymous-audit-replacement',
        slug: 'anonymous-audit-theme',
        existing_slug: 'anonymous-audit-theme',
        display_name: 'Anonymous Audit Theme',
        description: '',
        name: 'Anonymous Audit Board',
        short_name: uniqueShort('anmx', testInfo),
        site_name: 'Anonymous Audit Site',
        site_subtitle: '',
        filename: 'missing-backup.zip',
        kind: 'full',
        target_type: 'none',
        storage_mode: 'directory',
      };

      await test.step('every admin mutation route answers 403 with an exact denial body', async () => {
        for (const route of ADMIN_MUTATION_ROUTES) {
          const response = route.multipartField
            ? await api.post(route.path, {
              multipart: {
                ...form,
                [route.multipartField]: { name: 'audit.png', mimeType: 'image/png', buffer: tinyPng },
              },
              headers: { Origin: app.baseURL },
              maxRedirects: 0,
            })
            : await api.post(route.path, {
              form,
              headers: { Origin: app.baseURL },
              maxRedirects: 0,
            });
          expect(response.status(), `${route.path} must deny anonymous mutations`).toBe(403);
          const body = await expectSafeResponse(response);
          expect(adminDenialText(body), `${route.path} denial body: ${body.slice(0, 160)}`).toBe(true);
        }
      });

      await test.step('restore upload routes reject anonymous uploads with the documented interstitial', async () => {
        // Restore is a full-page multipart upload; the handler renders a 200
        // "Redirecting" interstitial whose Continue link points at the
        // login-gated panel and logs one [ERROR] line. Assert that exact
        // contract (not a generic 403) and declare the two expected log lines.
        expectServerLogError({
          pattern: /Full restore failed - route: \/admin\/restore/,
          reason: 'anonymous full restore is rejected and logged before any upload is applied',
        });
        expectServerLogError({
          pattern: /Board restore failed - route: \/admin\/board\/restore/,
          reason: 'anonymous board restore is rejected and logged before any upload is applied',
        });
        for (const [path, message] of [
          ['/admin/restore', 'Restore failed.'],
          ['/admin/board/restore', 'Board restore failed.'],
        ] as const) {
          const response = await api.post(path, {
            multipart: {
              ...form,
              file: { name: 'audit.png', mimeType: 'image/png', buffer: tinyPng },
            },
            headers: { Origin: app.baseURL },
            maxRedirects: 0,
          });
          expect(response.status(), `${path} reports the denial on an interstitial page`).toBe(200);
          const body = await expectSafeResponse(response);
          expect(body).toContain(message);
          expect(body).toContain('Continue');
          expect(body).toContain(`/admin/panel?restore_error=`);
          expect(body).not.toContain('site-settings-panel');
          expect(body).not.toContain('admin-panel-logout');
        }
      });

      await test.step('no database table, setting, or backup file changed', async () => {
        const after = adminStateSnapshot(app);
        expect(after).toEqual(before);
        expect(await backupFiles(app)).toEqual(backupsBefore);
      });
    } finally {
      await api.dispose();
    }
  });

  test('an anonymous visitor gets a 403 admin panel, never a dashboard or login redirect', async ({ page, app }) => {
    actor('anonymous');
    expectHttpError({
      method: 'GET',
      path: '/admin/panel',
      status: 403,
      reason: 'the unauthenticated admin panel must fail closed',
    });
    await page.goto(`${app.baseURL}/admin/panel`);
    expect(new URL(page.url()).pathname).toBe('/admin/panel');
    const body = await page.locator('body').innerText();
    expect(body).toContain(ADMIN_LOGIN_DENIED);
    expect(body).not.toContain('site settings');
    expect(body).not.toContain('logout');
    expect(await page.locator('#site-settings-panel, form[action="/admin/board/create"]').count()).toBe(0);
    await expectSafePage(page);
  });

  test('logging out invalidates restored and sibling admin sessions for POST mutations', async ({ page, browser, app }, testInfo) => {
    // Preconditions: one admin session; a sibling context is opened before logout.
    await adminLogin(page, app);
    const capturedCsrf = await adminCsrf(page, app);
    const storage = await page.context().storageState();
    const sessionCookie = (await page.context().cookies(app.baseURL)).find((cookie) => cookie.name === 'chan_admin_session');
    expect(sessionCookie).toBeTruthy();

    const siblingContext = await newAuditedContext(browser, { storageState: storage });
    const sibling = await siblingContext.newPage();
    const restoredContext = await newAuditedContext(browser, { storageState: storage });
    const restored = await restoredContext.newPage();
    try {
      await sibling.goto(`${app.baseURL}/admin/panel`);
      expect(new URL(sibling.url()).pathname).toBe('/admin/panel');

      await adminLogout(page);

      const boardsBefore = tableCount(app, 'boards');
      await test.step('the restored storageState context is denied and persists nothing', async () => {
        const response = await restored.request.post(`${app.baseURL}/admin/board/create`, {
          form: { _csrf: capturedCsrf, short_name: uniqueShort('stale', testInfo), name: 'Stale Session', description: '' },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(SESSION_EXPIRED);
        expect(tableCount(app, 'boards')).toBe(boardsBefore);
      });

      await test.step('a sibling context sharing the pre-logout session is denied too', async () => {
        const response = await sibling.request.post(`${app.baseURL}/admin/board/create`, {
          form: { _csrf: capturedCsrf, short_name: uniqueShort('sibl', testInfo), name: 'Sibling Session', description: '' },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(403);
        expect(await expectSafeResponse(response)).toContain(SESSION_EXPIRED);
        expect(tableCount(app, 'boards')).toBe(boardsBefore);
      });
    } finally {
      await restoredContext.close();
      await siblingContext.close();
    }
  });

  test('a view-password board leaks no content to an unrelated browser before unlock', async ({ page, browser, app }, testInfo) => {
    // Preconditions: board is public while content is seeded, then switched to view_password.
    actor('owner');
    const board = uniqueShort('vleak', testInfo);
    const bodyMarker = marker('protected-body');
    const subjectMarker = marker('protected-subject');
    app.createBoardCli({ short: board, name: 'View Leak' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, {
      subject: subjectMarker,
      body: bodyMarker,
      filePath: app.fixtures().tinyPng,
    });
    const postId = await postIdFrom(page);
    const mediaHref = await page.locator('.post.op .file-info a').first().getAttribute('href');
    expect(mediaHref).toBeTruthy();
    setBoardFixtureSettings(app, board, {
      accessMode: 'view_password',
      accessPasswordHash: adminPasswordHash(app),
    });

    actor('other');
    const otherContext = await newAuditedContext(browser);
    const other = await otherContext.newPage();
    try {
      for (const pathname of [`/${board}`, `/${board}/catalog`, `/${board}/thread/${threadId}`]) {
        await test.step(`the unrelated browser is gated at ${pathname}`, async () => {
          expectHttpError({
            method: 'GET',
            path: pathname,
            status: 403,
            reason: 'view-password boards must render only the unlock gate without the cookie',
          });
          await other.goto(`${app.baseURL}${pathname}`);
          const body = await other.locator('body').innerText();
          expect(body).toContain('password protected board');
          expect(body).not.toContain(bodyMarker);
          expect(body).not.toContain(subjectMarker);
          expect(body).not.toContain('tiny.png');
          await expect(other.locator('input[name="password"]')).toBeVisible();
          await expectSafePage(other);
        });
      }
    } finally {
      await otherContext.close();
    }

    const api = await request.newContext({ baseURL: app.baseURL });
    try {
      await test.step('direct media, post redirects, and updates endpoints deny without the cookie', async () => {
        const media = await api.get(mediaHref!);
        expect(media.status()).toBe(403);
        expect(await media.text()).not.toContain(bodyMarker);

        const postRedirect = await api.get(`/${board}/post/${postId}`, { maxRedirects: 0 });
        expect(postRedirect.status()).toBe(303);
        expect(postRedirect.headers()['location']).toContain(`/${board}/unlock?return_to=`);
        expect(await postRedirect.text()).not.toContain(bodyMarker);

        const updates = await api.get(`/${board}/thread/${threadId}/updates?since=0`);
        expect(updates.status()).toBe(403);
        expect(await updates.text()).not.toContain(bodyMarker);
      });
    } finally {
      await api.dispose();
    }
  });

  test('unlock cookies are board-scoped and a password change retires the old password and old cookie', async ({ page, app }, testInfo) => {
    // Preconditions: two view_password boards sharing a known password hash; the
    // password of board A is rotated mid-test through the real admin UI.
    const boardA = uniqueShort('unlA', testInfo);
    const boardB = uniqueShort('unlB', testInfo);
    const boardABody = marker('unlock-a-body');
    app.createBoardCli({ short: boardA, name: 'Unlock A' });
    app.createBoardCli({ short: boardB, name: 'Unlock B' });
    setBoardFixtureSettings(app, boardA, { postCooldownSecs: 0 });
    await createThread(page, app, boardA, { subject: marker('unlock-a-subject'), body: boardABody });
    const initialHash = adminPasswordHash(app);
    setBoardFixtureSettings(app, boardA, { accessMode: 'view_password', accessPasswordHash: initialHash });
    setBoardFixtureSettings(app, boardB, { accessMode: 'view_password', accessPasswordHash: initialHash });

    const api = await request.newContext({ baseURL: app.baseURL });
    try {
      const unlockCsrf = await signedCsrf(api, `/${boardA}/unlock`);
      const unlocked = await api.post(`/${boardA}/unlock`, {
        form: { _csrf: unlockCsrf, password: ADMIN_PASSWORD, return_to: `/${boardA}` },
        maxRedirects: 0,
      });
      expect(unlocked.status()).toBe(303);
      const oldCookie = setCookieValue(unlocked, `rustchan_board_access_${boardA}`);
      expect(oldCookie).toBeTruthy();

      await test.step('the board A cookie opens A but not B', async () => {
        const viewA = await api.get(`/${boardA}`);
        expect(viewA.status()).toBe(200);
        expect(await viewA.text()).toContain(boardABody);
        const viewB = await api.get(`/${boardB}`);
        expect(viewB.status()).toBe(403);
        const viewBBody = await viewB.text();
        expect(viewBBody).toContain('password protected board');
        expect(viewBBody).not.toContain(boardABody);
      });

      await test.step('rotating the password retires the old cookie and old password', async () => {
        await updateBoardSettings(page, app, boardA, {
          accessMode: 'view_password',
          accessPassword: 'rotated-secret-1',
        });
        const newHash = sqliteQuery(app, `SELECT access_password_hash FROM boards WHERE short_name = '${boardA}';`);
        expect(newHash).not.toBe(initialHash);

        const staleView = await api.get(`/${boardA}`);
        expect(staleView.status()).toBe(403);
        expect(await staleView.text()).toContain('password protected board');

        const staleUnlockCsrf = await signedCsrf(api, `/${boardA}/unlock`);
        const oldPassword = await api.post(`/${boardA}/unlock`, {
          form: { _csrf: staleUnlockCsrf, password: ADMIN_PASSWORD, return_to: `/${boardA}` },
          maxRedirects: 0,
        });
        expect(oldPassword.status()).toBe(403);
        expect(await oldPassword.text()).toContain('Incorrect board password.');
        expect(setCookieValue(oldPassword, `rustchan_board_access_${boardA}`)).toBeUndefined();

        const newPassword = await api.post(`/${boardA}/unlock`, {
          form: { _csrf: staleUnlockCsrf, password: 'rotated-secret-1', return_to: `/${boardA}` },
          maxRedirects: 0,
        });
        expect(newPassword.status()).toBe(303);
        const freshCookie = setCookieValue(newPassword, `rustchan_board_access_${boardA}`);
        expect(freshCookie).toBeTruthy();
        expect(freshCookie).not.toBe(oldCookie);

        const freshView = await api.get(`/${boardA}`);
        expect(freshView.status()).toBe(200);
        expect(await freshView.text()).toContain(boardABody);
      });
    } finally {
      await api.dispose();
    }
  });

  test('a post-password board allows viewing but denies thread and reply creation until unlock', async ({ page, app }, testInfo) => {
    // Preconditions: content is seeded while public, then the board is switched to post_password.
    const board = uniqueShort('postpw', testInfo);
    const bodyMarker = marker('postpw-body');
    app.createBoardCli({ short: board, name: 'Post Password Audit' });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
    const threadId = await createThread(page, app, board, { subject: marker('postpw-subject'), body: bodyMarker });
    const postId = await postIdFrom(page);
    setBoardFixtureSettings(app, board, {
      accessMode: 'post_password',
      accessPasswordHash: adminPasswordHash(app),
    });

    const api = await request.newContext({ baseURL: app.baseURL });
    try {
      await test.step('viewing stays public and shows the posting gate', async () => {
        const view = await api.get(`/${board}`);
        expect(view.status()).toBe(200);
        const html = await view.text();
        expect(html).toContain(bodyMarker);
        expect(html).toContain('id="board-access-gate"');
        expect(html).toContain('Viewing is public, but creating threads and replies on this board requires the board password.');
        expect(html).toContain(`action="/${board}/unlock"`);
      });

      await test.step('crafted thread creation redirects to unlock and persists nothing', async () => {
        const csrf = await signedCsrf(api, `/${board}`);
        const postsBefore = tableCount(app, 'posts');
        const response = await api.post(`/${board}`, {
          multipart: { _csrf: csrf, submission_token: marker('pp-thread'), body: 'post password blocked thread' },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(303);
        expect(response.headers()['location']).toContain(`/${board}/unlock?return_to=`);
        expect(tableCount(app, 'posts')).toBe(postsBefore);
        expect(tableCount(app, 'threads', `id = ${threadId}`)).toBe(1);
      });

      await test.step('crafted reply redirects to unlock and persists nothing', async () => {
        const csrf = await signedCsrf(api, `/${board}/thread/${threadId}`);
        const postsBefore = tableCount(app, 'posts');
        const response = await api.post(`/${board}/thread/${threadId}`, {
          multipart: { _csrf: csrf, submission_token: marker('pp-reply'), body: 'post password blocked reply' },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(303);
        expect(response.headers()['location']).toContain(`/${board}/unlock?return_to=`);
        expect(tableCount(app, 'posts')).toBe(postsBefore);
      });

      await test.step('the self-service edit page fails closed without leaking the body', async () => {
        const response = await api.get(`/${board}/post/${postId}/edit`, { maxRedirects: 0 });
        expect(response.status()).toBe(403);
        const body = await expectSafeResponse(response);
        expect(body).toContain('posting is password protected');
        expect(body).not.toContain(bodyMarker);
      });

      await test.step('after unlocking, the crafted reply is accepted exactly once', async () => {
        const unlockCsrf = await signedCsrf(api, `/${board}/unlock`);
        const unlocked = await api.post(`/${board}/unlock`, {
          form: { _csrf: unlockCsrf, password: ADMIN_PASSWORD, return_to: `/${board}` },
          maxRedirects: 0,
        });
        expect(unlocked.status()).toBe(303);
        const postsBefore = tableCount(app, 'posts');
        const csrf = await signedCsrf(api, `/${board}/thread/${threadId}`);
        const response = await api.post(`/${board}/thread/${threadId}`, {
          multipart: { _csrf: csrf, submission_token: marker('pp-ok'), body: marker('postpw-reply') },
          maxRedirects: 0,
        });
        expect(response.status()).toBe(303);
        expect(tableCount(app, 'posts')).toBe(postsBefore + 1);
      });
    } finally {
      await api.dispose();
    }
  });

  test('swapped report/thread identifiers and cross-record report resolution stay scoped', async ({ page, app }) => {
    // Preconditions: public board, two actors, two reports; admin session for resolution.
    const subject = marker('report-target');
    const threadId = await createThread(page, app, 'pub', { subject, body: marker('report-body') });
    const postId = await postIdOf(page, '.post.op');
    const csrf = await publicCsrf(page, app, `/pub/thread/${threadId}`);

    await test.step('a report with a mismatched thread id is rejected and writes no report row', async () => {
      const reportsBefore = tableCount(app, 'reports');
      const response = await page.request.post(`${app.baseURL}/report`, {
        form: { _csrf: csrf, post_id: String(postId), thread_id: String(threadId + 9999), board: 'pub', reason: 'swapped thread' },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(400);
      expect(await expectSafeResponse(response)).toContain('Reported thread does not match the selected post.');
      expect(tableCount(app, 'reports')).toBe(reportsBefore);
    });

    await test.step('resolving one report leaves the other report open', async () => {
      const secondThread = await createThread(page, app, 'pub', { subject: marker('report-target-2'), body: marker('report-body-2') });
      const secondPost = await postIdOf(page, '.post.op');

      const firstReport = await page.request.post(`${app.baseURL}/report`, {
        form: { _csrf: csrf, post_id: String(postId), thread_id: String(threadId), board: 'pub', reason: 'audit report one' },
        maxRedirects: 0,
      });
      expect(firstReport.status()).toBe(303);
      const secondCsrf = await publicCsrf(page, app, `/pub/thread/${secondThread}`);
      const secondReport = await page.request.post(`${app.baseURL}/report`, {
        form: { _csrf: secondCsrf, post_id: String(secondPost), thread_id: String(secondThread), board: 'pub', reason: 'audit report two' },
        maxRedirects: 0,
      });
      expect(secondReport.status()).toBe(303);
      expect(tableCount(app, 'reports')).toBe(2);

      const firstReportId = Number(sqliteQuery(app, `SELECT id FROM reports WHERE reason = 'audit report one' ORDER BY id DESC LIMIT 1;`));
      const secondReportId = Number(sqliteQuery(app, `SELECT id FROM reports WHERE reason = 'audit report two' ORDER BY id DESC LIMIT 1;`));
      expect(firstReportId).toBeGreaterThan(0);
      expect(secondReportId).toBeGreaterThan(0);

      await adminLogin(page, app);
      const resolveCsrf = await adminCsrf(page, app);
      const resolved = await page.request.post(`${app.baseURL}/admin/report/resolve`, {
        form: { _csrf: resolveCsrf, report_id: String(firstReportId) },
        maxRedirects: 0,
      });
      expect(resolved.status()).toBe(303);
      expect(sqliteQuery(app, `SELECT status FROM reports WHERE id = ${firstReportId};`)).toBe('resolved');
      expect(sqliteQuery(app, `SELECT status FROM reports WHERE id = ${secondReportId};`)).toBe('open');
    });
  });

  test('poll voting enforces CSRF, invalid option ids, duplicate votes, and cross-board gates', async ({ page, app }, testInfo) => {
    // Preconditions: a poll on public /pub and a poll on a board that becomes post_password.
    actor('anonymous');
    await page.goto(`${app.baseURL}/pub`);
    await openPostForm(page, 'pub');
    await page.locator('.poll-creator > summary').click();
    await page.locator('textarea[name="body"]').fill(marker('poll-body'));
    await page.locator('input[name="poll_question"]').fill('Audit poll question');
    await page.getByRole('textbox', { name: 'poll option 1', exact: true }).fill('Audit option one');
    await page.getByRole('textbox', { name: 'poll option 2', exact: true }).fill('Audit option two');
    await Promise.all([
      page.waitForURL(/\/pub\/thread\/\d+/),
      page.getByRole('button', { name: /post thread/i }).click(),
    ]);
    const pubPollId = Number(sqliteQuery(app, 'SELECT id FROM polls ORDER BY id DESC LIMIT 1;'));
    const optionOne = Number(sqliteQuery(app, `SELECT id FROM poll_options WHERE poll_id = ${pubPollId} ORDER BY id ASC LIMIT 1;`));
    const optionTwo = Number(sqliteQuery(app, `SELECT id FROM poll_options WHERE poll_id = ${pubPollId} ORDER BY id DESC LIMIT 1;`));
    const threadCsrf = await publicCsrf(page, app, page.url().replace(app.baseURL, ''));
    expect(pubPollId).toBeGreaterThan(0);
    expect(optionOne).toBeGreaterThan(0);
    expect(optionTwo).toBeGreaterThan(optionOne);

    await test.step('a voted option is persisted exactly once and duplicates are rejected', async () => {
      const voted = await page.request.post(`${app.baseURL}/vote`, {
        form: { _csrf: threadCsrf, option_id: String(optionOne) },
        maxRedirects: 0,
      });
      expect(voted.status()).toBe(303);
      expect(tableCount(app, 'poll_votes', `poll_id = ${pubPollId} AND option_id = ${optionOne}`)).toBe(1);

      const duplicate = await page.request.post(`${app.baseURL}/vote`, {
        form: { _csrf: threadCsrf, option_id: String(optionTwo) },
        maxRedirects: 0,
      });
      expect(duplicate.status()).toBe(400);
      expect(await expectSafeResponse(duplicate)).toContain('you have already voted');
      expect(tableCount(app, 'poll_votes', `poll_id = ${pubPollId}`)).toBe(1);
    });

    await test.step('zero and extreme option ids are rejected without votes', async () => {
      const zero = await page.request.post(`${app.baseURL}/vote`, {
        form: { _csrf: threadCsrf, option_id: '0' },
        maxRedirects: 0,
      });
      expect(zero.status()).toBe(400);
      expect(await expectSafeResponse(zero)).toContain('Invalid poll option.');

      const huge = await page.request.post(`${app.baseURL}/vote`, {
        form: { _csrf: threadCsrf, option_id: '9223372036854775807' },
        maxRedirects: 0,
      });
      expect(huge.status()).toBe(404);
      expect(await expectSafeResponse(huge)).toContain('Poll option not found.');
      expect(tableCount(app, 'poll_votes', `poll_id = ${pubPollId}`)).toBe(1);
    });

    await test.step('an option id from a locked board redirects to unlock and records no vote', async () => {
      const lockedBoard = uniqueShort('pollock', testInfo);
      app.createBoardCli({ short: lockedBoard, name: 'Locked Poll' });
      await page.goto(`${app.baseURL}/${lockedBoard}`);
      await openPostForm(page, lockedBoard);
      await page.locator('.poll-creator > summary').click();
      await page.locator('textarea[name="body"]').fill(marker('locked-poll-body'));
      await page.locator('input[name="poll_question"]').fill('Locked poll question');
      await page.getByRole('textbox', { name: 'poll option 1', exact: true }).fill('Locked option one');
      await page.getByRole('textbox', { name: 'poll option 2', exact: true }).fill('Locked option two');
      await Promise.all([
        page.waitForURL(new RegExp(`/${lockedBoard}/thread/\\d+`)),
        page.getByRole('button', { name: /post thread/i }).click(),
      ]);
      const lockedPollId = Number(sqliteQuery(app, 'SELECT id FROM polls ORDER BY id DESC LIMIT 1;'));
      const lockedOption = Number(sqliteQuery(app, `SELECT id FROM poll_options WHERE poll_id = ${lockedPollId} ORDER BY id ASC LIMIT 1;`));

      setBoardFixtureSettings(app, lockedBoard, {
        accessMode: 'post_password',
        accessPasswordHash: adminPasswordHash(app),
      });
      const response = await page.request.post(`${app.baseURL}/vote`, {
        form: { _csrf: threadCsrf, option_id: String(lockedOption) },
        maxRedirects: 0,
      });
      expect(response.status()).toBe(303);
      expect(response.headers()['location']).toContain(`/${lockedBoard}/unlock?return_to=`);
      expect(tableCount(app, 'poll_votes', `poll_id = ${lockedPollId}`)).toBe(0);
    });
  });

  test('script, event-handler, javascript:, and data: payloads render as inert escaped text', async ({ page, app }, testInfo) => {
    // Preconditions: public board; markers are unique to this test.
    actor('anonymous');
    const scriptMarker = marker('XSS_MARKER_script');
    const imageMarker = marker('XSS_MARKER_img');
    const jsMarker = marker('XSS_MARKER_js');
    const dataMarker = marker('XSS_MARKER_data');
    const svgMarker = marker('XSS_MARKER_svg');
    const idMarker = marker('XSS_MARKER_id');
    const subject = marker('xss-subject');
    const payload = [
      `<script>window.${scriptMarker}=1</script>`,
      `<img src=x onerror="window.${imageMarker}=2">`,
      `javascript:window.${jsMarker}=3`,
      `data:text/html,<script>window.${dataMarker}=4</script>`,
      `"><svg/onload=window.${svgMarker}=5>`,
      `${idMarker} <b id="${idMarker}">bold</b>`,
      "quote payload ' \" ` backtick",
    ].join('\n');

    await expectNoDialog(page, async () => {
      await createThread(page, app, 'pub', { subject, body: payload });
    });

    const postBody = page.locator('.post.op .post-body');
    await expect(postBody).toBeVisible();
    const rendered = await postBody.innerText();
    for (const raw of [`<script>`, `onerror`, `javascript:`, `data:text/html`, `<svg`, `<b id=`]) {
      expect(rendered, `escaped text should contain ${raw}`).toContain(raw);
    }
    expect(rendered).toContain(scriptMarker);
    expect(rendered).toContain("quote payload ' \" ` backtick");

    const executed = await page.evaluate(() => Object.keys(window).filter((key) => key.includes('XSS_MARKER')));
    expect(executed, 'no injected globals may exist').toEqual([]);
    expect(await page.locator('.post .post-body script').count()).toBe(0);
    expect(await page.locator('.post .post-body img[onerror]').count()).toBe(0);
    expect(await page.locator('.post .post-body svg').count()).toBe(0);
    expect(await page.locator('.post .post-body a[href^="javascript:" i]').count()).toBe(0);
    expect(await page.locator('.post .post-body a[href^="data:" i]').count()).toBe(0);
    expect(await page.locator(`#${idMarker}`).count()).toBe(0);

    await expectNoDialog(page, async () => {
      await page.reload({ waitUntil: 'domcontentloaded' });
    });
    await expect(page.locator('.post .post-body script')).toHaveCount(0);
  });

  test('a hostile upload filename is sanitized in the database and served with a safe image response', async ({ page, app }) => {
    // Preconditions: img board accepts images.
    actor('owner');
    const subject = marker('upload-subject');
    await createThread(page, app, 'img', { subject, body: marker('upload-body'), filePath: app.fixtures().oddNamePng });
    const postId = await postIdFrom(page);
    const storedName = sqliteQuery(app, `SELECT file_name FROM posts WHERE id = ${postId};`);
    const storedPath = sqliteQuery(app, `SELECT file_path FROM posts WHERE id = ${postId};`);
    const storedMime = sqliteQuery(app, `SELECT mime_type FROM posts WHERE id = ${postId};`);
    expect(storedName).not.toMatch(/["/\\:]/);
    expect(storedName).toContain('unicode-é');
    expect(storedPath).not.toContain('..');
    // The deterministic 1x1 PNG fixture benefits from the existing native WebP optimization.
    expect(storedMime).toBe('image/webp');
    expect(storedPath).toMatch(/\.webp$/);

    const mediaHref = await page.locator('.post.op .file-info a').first().getAttribute('href');
    expect(mediaHref).toBeTruthy();
    expect(mediaHref).toMatch(new RegExp(`^/boards/img/`));
    expect(mediaHref).not.toContain('..');
    expect(mediaHref).toMatch(/\.webp$/);
    const lastSegment = mediaHref!.split('/').pop() ?? '';
    expect(lastSegment).not.toMatch(/[\r\n"\\]/);

    const response = await page.request.get(`${app.baseURL}${mediaHref}`, { maxRedirects: 0 });
    expect(response.status()).toBe(200);
    const headers = response.headers();
    expect(headers['content-type']).toBe('image/webp');
    expect(headers['x-content-type-options']).toBe('nosniff');
    const output = await response.body();
    const originalSize = (await fsp.stat(app.fixtures().oddNamePng)).size;
    expect(output.subarray(0, 4).toString('ascii')).toBe('RIFF');
    expect(output.subarray(8, 12).toString('ascii')).toBe('WEBP');
    expect(output.length).toBeLessThan(originalSize);
    noteEvent('hostile-filename-optimization', `${originalSize} PNG bytes became ${output.length} WebP bytes`);
    const disposition = headers['content-disposition'];
    if (disposition !== undefined) {
      expect(disposition).toMatch(/^(inline|attachment); filename="[^"\r\n]*"$/);
      expect(disposition).not.toContain('..');
    }
  });

  test('public HTML, error pages, and the unauthenticated admin surface leak no hashes, secrets, tokens, paths, or SQL', async ({ page, browser, app }, testInfo) => {
    // Preconditions: authenticated admin only to capture the secrets that must
    // not appear on public pages; a thread is created so an ownership cookie and
    // deletion token exist.
    actor('admin');
    await adminLogin(page, app);
    const sessionValue = (await page.context().cookies(app.baseURL)).find((cookie) => cookie.name === 'chan_admin_session')!.value;
    const passwordHash = sqliteQuery(app, "SELECT password_hash FROM admin_users WHERE username = 'admin' LIMIT 1;");
    const threadId = await createThread(page, app, 'pub', { subject: marker('exposure'), body: 'exposure body' });
    const postId = await postIdFrom(page);
    const deletionToken = sqliteQuery(app, `SELECT deletion_token FROM posts WHERE id = ${postId};`);
    await expect.poll(async () => (await page.context().cookies(app.baseURL))
      .some((cookie) => cookie.name === 'rustchan_owned_posts'), {
      timeout: 5_000,
      message: 'ownership grant cookie becomes visible to the context',
    }).toBe(true);
    const ownedCookie = (await page.context().cookies(app.baseURL))
      .find((cookie) => cookie.name === 'rustchan_owned_posts')!.value;
    expect(sessionValue.length).toBeGreaterThan(8);
    expect(passwordHash.length).toBeGreaterThan(8);
    expect(deletionToken.length).toBeGreaterThan(8);
    expect(ownedCookie.length).toBeGreaterThan(8);

    const secrets: Array<[string, string]> = [
      ['admin password hash', passwordHash],
      ['admin session value', sessionValue],
      ['post deletion token', deletionToken],
      ['owned_posts cookie value', ownedCookie],
    ];
    const api = await request.newContext({ baseURL: app.baseURL });
    try {
      const scan = async (pathname: string, expectedStatus: number) => {
        const response = await api.get(pathname, { maxRedirects: 0 });
        expect(response.status(), `status for ${pathname}`).toBe(expectedStatus);
        const body = await expectSafeResponse(response);
        for (const [label, secret] of secrets) {
          expect(body, `${pathname} must not expose the ${label}`).not.toContain(secret);
        }
        expect(body).not.toContain('cookie_secret');
        expect(body).not.toMatch(/\/Users\/|rustchan-data|target\/debug/);
        expect(body).not.toMatch(/SELECT COUNT\(\*\)|SELECT \* FROM|INSERT INTO|UPDATE \w+ SET/i);
        return body;
      };

      await test.step('public and error surfaces stay clean', async () => {
        await scan('/', 200);
        await scan('/pub', 200);
        await scan(`/pub/thread/${threadId}`, 200);
        await scan('/banned', 200);
        await scan('/admin', 200);
        await scan('/admin/panel', 403);
        await scan('/definitely-not-a-route', 404);
      });

      await test.step('a CSRF denial error page stays clean', async () => {
        const denied = await api.post('/pub', {
          multipart: { submission_token: marker('exposure'), body: 'exposure csrf body' },
          maxRedirects: 0,
        });
        expect(denied.status()).toBe(403);
        const body = await expectSafeResponse(denied);
        for (const [label, secret] of secrets) {
          expect(body, `CSRF error page must not expose the ${label}`).not.toContain(secret);
        }
        expect(body).not.toContain('cookie_secret');
      });
    } finally {
      await api.dispose();
    }

    await test.step('session, CSRF, ownership, and unlock cookies carry the documented attributes over plain HTTP', async () => {
      const loginContext = await newAuditedContext(browser);
      const loginPage = await loginContext.newPage();
      try {
        await adminLogin(loginPage, app);
        // Cookie-store attributes are engine-neutral. WebKit deliberately
        // omits Set-Cookie from page response headers (verified against a raw
        // HTTP probe: WebKit `headersArray()` returns no set-cookie while the
        // context cookie jar receives the cookie), so the wire-level attribute
        // strings are asserted below through the Node-side request context,
        // which exposes Set-Cookie on every engine.
        const storeCookies = await loginContext.cookies(app.baseURL);
        const sessionStore = storeCookies.find((cookie) => cookie.name === 'chan_admin_session');
        expect(sessionStore, 'a browser login must store the admin session cookie').toBeTruthy();
        expect(sessionStore!.httpOnly).toBe(true);
        expect(sessionStore!.sameSite).toBe('Lax');
        expect(sessionStore!.secure).toBe(false);
        expect(sessionStore!.path).toBe('/');
        expect(
          sessionStore!.expires,
          'the session cookie must be persistent (a Max-Age was sent), not a browser-session cookie',
        ).toBeGreaterThan(Date.now() / 1000);
        const csrfStore = storeCookies.find((cookie) => cookie.name === 'csrf_token');
        expect(csrfStore, 'a browser login must keep a CSRF cookie').toBeTruthy();
        expect(csrfStore!.httpOnly).toBe(false);
        expect(csrfStore!.sameSite).toBe('Strict');
        expect(csrfStore!.secure).toBe(false);
      } finally {
        await loginContext.close();
      }

      const loginApi = await request.newContext({ baseURL: app.baseURL });
      try {
        const loginForm = await loginApi.get('/admin');
        const loginCsrf = extractCsrf(await loginForm.text());
        const loginResponse = await loginApi.post('/admin/login', {
          form: { _csrf: loginCsrf, username: ADMIN_USERNAME, password: ADMIN_PASSWORD },
          maxRedirects: 0,
        });
        expect([302, 303], 'request-API admin login should succeed').toContain(loginResponse.status());
        const setCookies = (await loginResponse.headersArray())
          .filter((header) => header.name.toLowerCase() === 'set-cookie')
          .map((header) => header.value);
        const sessionCookie = setCookies.find((value) => value.startsWith('chan_admin_session='));
        expect(sessionCookie, 'the login response must set the session cookie').toBeTruthy();
        expect(sessionCookie).toMatch(/HttpOnly/i);
        expect(sessionCookie).toMatch(/SameSite=Lax/i);
        expect(sessionCookie).not.toMatch(/;\s*Secure/i);
        expect(sessionCookie).toMatch(/Path=\//i);
        expect(sessionCookie).toMatch(/Max-Age=\d+/i);

        const csrfCookie = setCookies.find((value) => value.startsWith('csrf_token='));
        expect(csrfCookie, 'the login response must set the CSRF cookie').toBeTruthy();
        expect(csrfCookie).not.toMatch(/HttpOnly/i);
        expect(csrfCookie).toMatch(/SameSite=Strict/i);
        expect(csrfCookie).not.toMatch(/;\s*Secure/i);
      } finally {
        await loginApi.dispose();
      }

      const board = uniqueShort('attrs', testInfo);
      app.createBoardCli({ short: board, name: 'Cookie Attrs' });
      setBoardFixtureSettings(app, board, { accessMode: 'view_password', accessPasswordHash: adminPasswordHash(app) });
      const unlockApi = await request.newContext({ baseURL: app.baseURL });
      try {
        const unlockCsrf = await signedCsrf(unlockApi, `/${board}/unlock`);
        const unlocked = await unlockApi.post(`/${board}/unlock`, {
          form: { _csrf: unlockCsrf, password: ADMIN_PASSWORD, return_to: `/${board}` },
          maxRedirects: 0,
        });
        expect(unlocked.status()).toBe(303);
        const unlockCookie = setCookieValue(unlocked, `rustchan_board_access_${board}`);
        expect(unlockCookie).toBeTruthy();
        expect(unlockCookie).toMatch(/HttpOnly/i);
        expect(unlockCookie).toMatch(/SameSite=Lax/i);
        expect(unlockCookie).not.toMatch(/;\s*Secure/i);
        expect(unlockCookie).toMatch(/Path=\//i);
      } finally {
        await unlockApi.dispose();
      }

      const anonymousContext = await newAuditedContext(browser);
      const anonymous = await anonymousContext.newPage();
      try {
        await createThread(anonymous, app, 'pub', { subject: marker('attrs-thread'), body: marker('attrs-body') });
        // WebKit can surface a freshly-set HttpOnly grant a moment after the
        // redirect, so wait for the cookie store to expose it instead of
        // assuming synchronous visibility.
        await expect.poll(async () => (await anonymousContext.cookies(app.baseURL))
          .some((cookie) => cookie.name === 'rustchan_owned_posts'), {
          timeout: 5_000,
          message: 'ownership grant cookie becomes visible to the anonymous context',
        }).toBe(true);
        const cookies = await anonymousContext.cookies(app.baseURL);
        const owned = cookies.find((cookie) => cookie.name === 'rustchan_owned_posts');
        expect(owned).toBeTruthy();
        expect(owned!.httpOnly).toBe(true);
        expect(owned!.sameSite).toBe('Lax');
        expect(owned!.secure).toBe(false);

        const visitor = cookies.find((cookie) => cookie.name === 'rustchan_visitor_id');
        expect(visitor).toBeTruthy();
        expect(visitor!.httpOnly).toBe(false);
        expect(visitor!.sameSite).toBe('Lax');
        expect(visitor!.secure).toBe(false);
      } finally {
        await anonymousContext.close();
      }
    });
  });

  test('a banned actor cannot post, sees the ban page, files exactly one appeal, and posting resumes after the ban is lifted', async ({ page, browser }) => {
    // Preconditions: dedicated standalone instance so banning this visitor cannot
    // affect other tests. On loopback, identity_key() prefers rustchan_visitor_id,
    // so the ban is scoped to this browser context's visitor identity.
    actor('banned');
    const app = await createStandaloneApp({
      admin: true,
      boards: [{ short: 'banbg', name: 'Ban Audit' }],
    });
    const context = await newAuditedContext(browser);
    const poster = await context.newPage();
    try {
      await poster.goto(`${app.baseURL}/banbg`);
      const threadId = await createThread(poster, app, 'banbg', { subject: 'ban target', body: 'ban target body' });
      const targetPostId = await postIdOf(poster, '.post.op');
      const ipHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE id = ${targetPostId};`);
      expect(ipHash).toMatch(/^[0-9a-f]{64}$/);

      await adminLogin(page, app);
      const banCsrf = await adminCsrf(page, app);
      const banResponse = await page.request.post(`${app.baseURL}/admin/ban/add`, {
        form: { _csrf: banCsrf, ip_hash: ipHash, reason: 'audit ban reason', duration_hours: '0' },
        maxRedirects: 0,
      });
      expect(banResponse.status()).toBe(303);
      expect(tableCount(app, 'bans', `ip_hash = '${ipHash}'`)).toBe(1);

      await test.step('posting is denied with the ban page and adds no row', async () => {
        await poster.goto(`${app.baseURL}/banbg/thread/${threadId}`);
        const toggle = poster.locator('[data-action="toggle-post-form"]').first();
        if (await toggle.isVisible()) {
          await toggle.click();
        }
        const replyForm = poster.locator(`form[action="/banbg/thread/${threadId}"]`).first();
        await replyForm.locator('textarea[name="body"]').fill('banned reply attempt');
        const postsBefore = tableCount(app, 'posts');
        expectHttpError({
          method: 'POST',
          path: `/banbg/thread/${threadId}`,
          status: 403,
          reason: 'banned actors must receive the ban page instead of a stored post',
        });
        await replyForm.getByRole('button', { name: /post reply/i }).click();
        await expect(poster.locator('body')).toContainText('you are banned');
        await expect(poster.locator('body')).toContainText('audit ban reason');
        expect(tableCount(app, 'posts')).toBe(postsBefore);
      });

      await test.step('the ban page is reachable and the appeal is accepted exactly once', async () => {
        await poster.goto(`${app.baseURL}/banned`);
        await expect(poster.locator('body')).toContainText('you are banned');
        const appeal = poster.locator('form.appeal-form');
        await appeal.locator('textarea[name="reason"]').fill('please review this ban');
        const firstAppeal = poster.waitForResponse((response) => (
          new URL(response.url()).pathname === '/appeal' && response.request().method() === 'POST'
        ));
        await appeal.getByRole('button', { name: 'submit appeal' }).click();
        const firstResponse = await firstAppeal;
        expect(firstResponse.status()).toBe(200);
        await expect(poster.locator('body')).toContainText('Your appeal has been submitted');
        expect(tableCount(app, 'ban_appeals')).toBe(1);
        expect(tableCount(app, 'bans', `ip_hash = '${ipHash}'`)).toBe(1);

        await poster.goto(`${app.baseURL}/banned`);
        const secondAppeal = poster.locator('form.appeal-form');
        await secondAppeal.locator('textarea[name="reason"]').fill('second appeal attempt');
        const secondResponse = poster.waitForResponse((response) => (
          new URL(response.url()).pathname === '/appeal' && response.request().method() === 'POST'
        ));
        await secondAppeal.getByRole('button', { name: 'submit appeal' }).click();
        expect((await secondResponse).status()).toBe(200);
        await expect(poster.locator('body')).toContainText('already filed an appeal');
        expect(tableCount(app, 'ban_appeals')).toBe(1);
      });

      await test.step('lifting the ban through the real admin route restores posting', async () => {
        const banId = Number(sqliteQuery(app, `SELECT id FROM bans WHERE ip_hash = '${ipHash}' ORDER BY id DESC LIMIT 1;`));
        const removeResponse = await page.request.post(`${app.baseURL}/admin/ban/remove`, {
          form: { _csrf: banCsrf, ban_id: String(banId) },
          maxRedirects: 0,
        });
        expect(removeResponse.status()).toBe(303);
        expect(tableCount(app, 'bans')).toBe(0);

        const postsBefore = tableCount(app, 'posts', `thread_id = ${threadId}`);
        await createReply(poster, app, 'banbg', threadId, marker('reply-after-ban'));
        expect(tableCount(app, 'posts', `thread_id = ${threadId}`)).toBe(postsBefore + 1);
      });
    } finally {
      // Stop the default page's admin polling before disposing the standalone server.
      await page.goto('about:blank');
      await context.close();
      await app.dispose();
    }
  });

  test('a banned identity cannot file reports and ordinary reporting still works', async ({ page, browser }) => {
    // Preconditions: dedicated standalone instance. A ban is enforced on the
    // report route through the shared ban authorization helper, so the banned
    // actor receives the ban page and no report row is written. The first
    // report in this test proves the CSRF token used by the banned attempt is
    // valid on this route, so a CSRF failure cannot be mistaken for ban
    // enforcement.
    actor('banned');
    const app = await createStandaloneApp({
      admin: true,
      boards: [{ short: 'repbg', name: 'Report Audit' }],
    });
    const context = await newAuditedContext(browser);
    const actorPage = await context.newPage();
    const adminContext = await newAuditedContext(browser);
    const adminPage = await adminContext.newPage();
    try {
      await actorPage.goto(`${app.baseURL}/repbg`);
      const threadId = await createThread(actorPage, app, 'repbg', { subject: 'report target', body: 'report target body' });
      const opId = await postIdOf(actorPage, '.post.op');
      await createReply(actorPage, app, 'repbg', threadId, 'report target reply');
      const replyId = Number(sqliteQuery(
        app,
        `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 0 ORDER BY id ASC LIMIT 1;`,
      ));
      const ipHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE id = ${opId};`);
      expect(ipHash).toMatch(/^[0-9a-f]{64}$/);
      const csrf = await publicCsrf(actorPage, app, `/repbg/thread/${threadId}`);

      const fileReport = (postId: number, reason: string) => actorPage.request.post(`${app.baseURL}/report`, {
        form: {
          _csrf: csrf,
          post_id: String(postId),
          thread_id: String(threadId),
          board: 'repbg',
          reason,
        },
        maxRedirects: 0,
      });

      await test.step('the same token and route create a report while the actor is not banned', async () => {
        const baseline = await fileReport(opId, 'baseline report');
        expect(baseline.status(), 'ordinary reporting must succeed, proving CSRF is satisfied').toBe(303);
        expect(tableCount(app, 'reports')).toBe(1);
      });

      await test.step('the ban is installed through the real admin route', async () => {
        await adminLogin(adminPage, app);
        const banCsrf = await adminCsrf(adminPage, app);
        const banResponse = await adminPage.request.post(`${app.baseURL}/admin/ban/add`, {
          form: { _csrf: banCsrf, ip_hash: ipHash, reason: 'report gate audit', duration_hours: '0' },
          maxRedirects: 0,
        });
        expect(banResponse.status()).toBe(303);
        expect(tableCount(app, 'bans', `ip_hash = '${ipHash}'`)).toBe(1);
      });

      await test.step('a banned report is refused with the ban page and writes no row', async () => {
        const rowsBefore = tableCount(app, 'reports');
        const banned = await fileReport(replyId, 'banned actor report');
        const bannedBody = await expectSafeResponse(banned);
        expect(banned.status(), 'a banned identity must not be able to file reports').toBe(403);
        expect(bannedBody).toContain('you are banned');
        expect(bannedBody).toContain('report gate audit');
        expect(tableCount(app, 'reports'), 'the banned report must not create a row').toBe(rowsBefore);
      });

      await test.step('a banned request without CSRF still fails the CSRF gate, never reaching the ban check', async () => {
        const noCsrf = await actorPage.request.post(`${app.baseURL}/report`, {
          form: {
            _csrf: 'wrong-token',
            post_id: String(replyId),
            thread_id: String(threadId),
            board: 'repbg',
            reason: 'no csrf',
          },
          maxRedirects: 0,
        });
        const noCsrfBody = await expectSafeResponse(noCsrf);
        expect(noCsrf.status()).toBe(403);
        expect(noCsrfBody).toContain(CSRF_DENIED);
        expect(noCsrfBody, 'the CSRF gate must fail before the ban page is rendered').not.toContain('you are banned');
      });

      await test.step('lifting the ban restores reporting for the same actor and token', async () => {
        const banId = Number(sqliteQuery(app, `SELECT id FROM bans WHERE ip_hash = '${ipHash}' ORDER BY id DESC LIMIT 1;`));
        const banCsrf = await adminCsrf(adminPage, app);
        const removeResponse = await adminPage.request.post(`${app.baseURL}/admin/ban/remove`, {
          form: { _csrf: banCsrf, ban_id: String(banId) },
          maxRedirects: 0,
        });
        expect(removeResponse.status()).toBe(303);
        expect(tableCount(app, 'bans')).toBe(0);

        const rowsBefore = tableCount(app, 'reports');
        const afterUnban = await fileReport(replyId, 'post-unban report');
        expect(afterUnban.status()).toBe(303);
        expect(tableCount(app, 'reports')).toBe(rowsBefore + 1);
      });
    } finally {
      await adminContext.close();
      await context.close();
      await app.dispose();
    }
  });

  test('a banned identity cannot vote in a poll and ordinary voting still works', async ({ page, browser }) => {
    // Preconditions: dedicated standalone instance. Poll voting is persisted
    // thread participation, so it shares the ban gate with posting. The banned
    // attempt uses a CSRF token that the post-unban vote then proves valid.
    actor('banned');
    const app = await createStandaloneApp({
      admin: true,
      boards: [{ short: 'votebg', name: 'Vote Audit' }],
    });
    const context = await newAuditedContext(browser);
    const actorPage = await context.newPage();
    const adminContext = await newAuditedContext(browser);
    const adminPage = await adminContext.newPage();
    try {
      await actorPage.goto(`${app.baseURL}/votebg`);
      await openPostForm(actorPage, 'votebg');
      await actorPage.locator('.poll-creator > summary').click();
      await actorPage.locator('textarea[name="body"]').fill('vote audit poll body');
      await actorPage.locator('input[name="poll_question"]').fill('Vote audit question');
      await actorPage.getByRole('textbox', { name: 'poll option 1', exact: true }).fill('Vote audit option one');
      await actorPage.getByRole('textbox', { name: 'poll option 2', exact: true }).fill('Vote audit option two');
      await Promise.all([
        actorPage.waitForURL(/\/votebg\/thread\/\d+/),
        actorPage.getByRole('button', { name: /post thread/i }).click(),
      ]);
      const pollId = Number(sqliteQuery(app, 'SELECT id FROM polls ORDER BY id DESC LIMIT 1;'));
      const threadId = Number(sqliteQuery(app, `SELECT thread_id FROM polls WHERE id = ${pollId};`));
      const optionId = Number(sqliteQuery(app, `SELECT id FROM poll_options WHERE poll_id = ${pollId} ORDER BY id ASC LIMIT 1;`));
      const ipHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE thread_id = ${threadId} AND is_op = 1 LIMIT 1;`);
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM poll_votes WHERE poll_id = ${pollId};`)).toBe('0');
      const csrf = await publicCsrf(actorPage, app, `/votebg/thread/${threadId}`);

      await adminLogin(adminPage, app);
      const banCsrf = await adminCsrf(adminPage, app);
      const banResponse = await adminPage.request.post(`${app.baseURL}/admin/ban/add`, {
        form: { _csrf: banCsrf, ip_hash: ipHash, reason: 'vote gate audit', duration_hours: '0' },
        maxRedirects: 0,
      });
      expect(banResponse.status()).toBe(303);
      expect(tableCount(app, 'bans', `ip_hash = '${ipHash}'`)).toBe(1);

      const castVote = (voteCsrf: string) => actorPage.request.post(`${app.baseURL}/vote`, {
        form: { _csrf: voteCsrf, option_id: String(optionId) },
        maxRedirects: 0,
      });

      const banned = await castVote(csrf);
      const bannedBody = await expectSafeResponse(banned);
      expect(banned.status(), 'a banned identity must not be able to vote').toBe(403);
      expect(bannedBody).toContain('you are banned');
      expect(bannedBody).toContain('vote gate audit');
      expect(tableCount(app, 'poll_votes', `poll_id = ${pollId}`), 'the banned vote must not be recorded').toBe(0);

      await test.step('lifting the ban lets the same token cast the vote exactly once', async () => {
        const banId = Number(sqliteQuery(app, `SELECT id FROM bans WHERE ip_hash = '${ipHash}' ORDER BY id DESC LIMIT 1;`));
        const removeCsrf = await adminCsrf(adminPage, app);
        const removeResponse = await adminPage.request.post(`${app.baseURL}/admin/ban/remove`, {
          form: { _csrf: removeCsrf, ban_id: String(banId) },
          maxRedirects: 0,
        });
        expect(removeResponse.status()).toBe(303);

        const afterUnban = await castVote(csrf);
        expect(afterUnban.status()).toBe(303);
        expect(tableCount(app, 'poll_votes', `poll_id = ${pollId}`)).toBe(1);
        const duplicate = await castVote(csrf);
        expect(duplicate.status()).toBe(400);
        expect(tableCount(app, 'poll_votes', `poll_id = ${pollId}`)).toBe(1);
      });
    } finally {
      await adminContext.close();
      await context.close();
      await app.dispose();
    }
  });
});
