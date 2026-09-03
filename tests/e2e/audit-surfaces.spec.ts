import type { Locator, Page } from '@playwright/test';
import {
  adminLogin,
  createReply,
  expect,
  expectSafePage,
  setSiteFixtureSettings,
  test,
} from './helpers';
import { expectNoHorizontalOverflow } from './phase4-helpers';

const AUDIT_PROJECTS = new Set(['chromium', 'webkit', 'firefox-nojs']);

test.beforeEach(async ({}, testInfo) => {
  test.skip(!AUDIT_PROJECTS.has(testInfo.project.name), 'focused audit browser matrix only');
});

test.describe('browser-reachable audit surfaces', () => {
  test('home, preferences, theme fallback, and narrow layout are reachable', async ({ page, app }, testInfo) => {
    await page.goto(app.baseURL);
    await expectSafePage(page);
    await expect(page.getByRole('link', { name: /home/i }).first()).toBeVisible();
    await expect(page.locator('main')).toContainText(/Boards|Public Board/i);
    await expect(page.locator('.board-card-link[href="/pub/catalog"], .board-card-link[href="/pub"]').first()).toBeVisible();
    for (const board of ['pub', 'img', 'vid', 'aud', 'txt']) {
      await expect(page.locator(`.board-card-link[href="/${board}/catalog"], .board-card-link[href="/${board}"]`).first()).toBeVisible();
    }
    await expect(page.getByRole('link', { name: /\/nsfw\//i }).first()).toBeVisible();
    await expect.poll(() => page.locator('.board-card').count(), {
      message: 'home should show at least the built-in boards',
    }).toBeGreaterThanOrEqual(6);
    await expect(page.locator('.user-preferences-form input[name="_csrf"]')).toHaveCount(1);
    await openDetailsIfClosed(page.locator('.user-preferences-panel').first());

    if (testInfo.project.name === 'firefox-nojs') {
      await expect(page.locator('html')).toHaveClass(/no-js/);
      const noJsPreferences = page.locator('.user-preferences-noscript');
      await expect(noJsPreferences).toBeVisible();
      await expect(noJsPreferences.locator('input[name="_csrf"]')).toHaveCount(5);
      await Promise.all([
        page.waitForResponse((response) => response.url() === `${app.baseURL}/preferences`
          && response.request().method() === 'POST'),
        noJsPreferences.locator('button[name="theme"][value="blue-sky"]').click(),
      ]);
      await expect(page).toHaveURL(/\/$/);
      await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
    } else {
      await expect(page.locator('html')).toHaveClass(/js/);
      await expect(page.locator('.user-preferences-form select[name="theme"]')).toBeVisible();
      await expect(page.locator('.user-preferences-form input[name="show_activity_badges"]')).toHaveCount(1);
      await page.locator('.user-preferences-form select[name="theme"]').selectOption('blue-sky');
      await expect(page.locator('html')).toHaveAttribute('data-theme', 'blue-sky');
    }

    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(app.baseURL);
    await expect(page.locator('.mobile-board-menu summary')).toBeVisible();
    await expectNoHorizontalOverflow(page, 'mobile home');
  });

  test('poll creation, voting, catalog controls, and hidden-thread views work through browser paths', async ({ page, app }, testInfo) => {
    const threadId = await createThreadViaBrowser(page, app.baseURL, 'pub', {
      subject: `audit poll ${testInfo.project.name}`,
      body: 'poll body for browser audit',
      poll: true,
    });
    await expect(page.locator('.poll-container')).toBeVisible();
    const firstPollOption = page.locator('.poll-vote-option input[name="option_id"]').first();
    await firstPollOption.check();
    await expect(firstPollOption).toBeChecked();
    const voteResponsePromise = page.waitForResponse((response) => response.url() === `${app.baseURL}/vote`
      && response.request().method() === 'POST');
    await page.getByRole('button', { name: /\[ cast vote \]/i }).click();
    const voteResponse = await voteResponsePromise;
    expect(voteResponse.status()).toBe(303);
    expect(voteResponse.headers().location).toBe(`/pub/thread/${threadId}#poll`);
    await expect(page.locator('.poll-results')).toBeVisible();

    await createThreadViaBrowser(page, app.baseURL, 'pub', {
      subject: `catalog hide ${testInfo.project.name}`,
      body: 'thread preference target',
    });
    await page.goto(`${app.baseURL}/pub/catalog`);
    await expect(page.locator('#catalog-sort')).toBeVisible();
    await expect(page.locator('#catalog-show-comment')).toBeVisible();

    if (testInfo.project.name === 'firefox-nojs') {
      await expect(page.locator('.catalog-thread-menu-toggle').first()).toBeHidden();
      await expect(page.locator('.catalog-thread-menu').first()).toBeHidden();
      await expect(page.locator('.catalog-thread-fallback-actions').first()).toBeVisible();
      const fallbackCount = await visibleFallbackAction(page.locator('.catalog-card-actions').first(), '/thread-preference');
      expect(
        fallbackCount,
        'Firefox/no-JS catalog cards should expose a visible pin/hide fallback action',
      ).toBeGreaterThan(0);
      return;
    }

    const targetCard = page.locator('.catalog-item').filter({ hasText: `catalog hide ${testInfo.project.name}` }).first();
    await targetCard.hover();
    await targetCard.locator('.catalog-thread-menu-toggle').click();
    await expect(targetCard.locator('.catalog-thread-menu')).toBeVisible();
    await Promise.all([
      page.waitForURL(/\/pub\/catalog/),
      targetCard.getByRole('button', { name: /hide thread/i }).click(),
    ]);
    await expect(page.locator('.board-nav-hidden')).toContainText('Hidden Threads: 1');

    await page.goto(`${app.baseURL}/pub/hidden`);
    await expect(page.locator('body')).toContainText(`catalog hide ${testInfo.project.name}`);
    const hiddenCard = page.locator('.catalog-item').filter({ hasText: `catalog hide ${testInfo.project.name}` }).first();
    await hiddenCard.hover();
    await hiddenCard.locator('.catalog-thread-menu-toggle').click();
    await Promise.all([
      page.waitForURL(/\/pub\/catalog/),
      hiddenCard.getByRole('button', { name: /unhide thread/i }).click(),
    ]);
    await expect(page.locator('body')).toContainText(`catalog hide ${testInfo.project.name}`);
  });

  test('public report controls work with JavaScript and expose a no-JS fallback', async ({ page, app }, testInfo) => {
    const threadId = await createThreadViaBrowser(page, app.baseURL, 'pub', {
      subject: `audit report ${testInfo.project.name}`,
      body: 'reportable OP',
    });
    await createReply(page, app, 'pub', threadId, 'reportable reply');
    await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
    const reply = page.locator('.post.reply').first();
    const postId = await postIdFrom(reply);

    if (testInfo.project.name === 'firefox-nojs') {
      await expect(reply.locator('.report-btn')).toBeHidden();
      await expect(reply.locator('.report-fallback-form')).toBeVisible();
      const fallbackCount = await visibleFallbackAction(reply.locator('.post-controls'), '/report');
      expect(
        fallbackCount,
        'Firefox/no-JS post controls should expose a visible report form or link',
      ).toBeGreaterThan(0);
      return;
    }

    await reply.locator('.report-btn').click();
    await expect(page.locator('#report-modal')).toBeVisible();
    await expect(page.locator('#report-info')).toContainText(`No.${postId}`);
    await page.locator('#report-reason').fill(`audit report ${testInfo.project.name}`);
    await Promise.all([
      page.waitForURL(new RegExp(`/pub/thread/${threadId}\\?reported=1#p${postId}`)),
      page.locator('#report-submit-btn').click(),
    ]);
    await expect(page.locator('.post-success-banner')).toContainText(/report submitted/i);
  });

  test('admin-authenticated and unauthenticated browser states expose expected security controls', async ({ page, app }) => {
    const unauthPanel = await page.request.get(`${app.baseURL}/admin/panel`, { maxRedirects: 0 });
    expect([302, 303, 403]).toContain(unauthPanel.status());

    await adminLogin(page, app);
    const panelResponse = await page.request.get(`${app.baseURL}/admin/panel`);
    expect(panelResponse.status()).toBe(200);
    expect(panelResponse.headers()['cache-control']).toContain('no-store');
    expect(panelResponse.headers()['x-frame-options']).toBe('SAMEORIGIN');
    expect(panelResponse.headers()['content-security-policy']).toContain("script-src 'self'");

    await page.goto(`${app.baseURL}/admin/panel`);
    await expectSafePage(page, { allowAdminInternals: true });
    for (const section of ['site health', 'boards', 'moderation', 'themes', 'full site backup', 'media settings', 'database maintenance']) {
      await expect(page.locator('.admin-dropdown > summary').filter({ hasText: section }).first()).toBeVisible();
    }
    const mutatingForms = page.locator('form[method="POST" i]');
    const formCount = await mutatingForms.count();
    expect(formCount).toBeGreaterThan(10);
    for (let index = 0; index < formCount; index += 1) {
      await expect(mutatingForms.nth(index).locator('input[name="_csrf"]')).toHaveCount(1);
    }
  });

  test('new activity badges are browser-visible and clear after visiting the target thread', async ({ page, browser, app }) => {
    setSiteFixtureSettings(app, {
      homepageNewThreadBadgesEnabled: true,
      homepageNewReplyBadgesEnabled: true,
      threadNewReplyBadgesEnabled: true,
    });
    await app.restart();

    const baselineThreadId = await createThreadViaBrowser(page, app.baseURL, 'pub', {
      subject: 'activity baseline thread',
      body: 'visited before a later reply arrives',
    });
    await page.goto(`${app.baseURL}/pub/thread/${baselineThreadId}`);
    await expectSafePage(page);
    await page.goto(`${app.baseURL}/pub/catalog`);
    await expectSafePage(page);

    const contextTwo = await browser.newContext({
      javaScriptEnabled: test.info().project.name !== 'firefox-nojs',
    });
    try {
      const other = await contextTwo.newPage();
      await createReplyViaBrowser(other, app.baseURL, 'pub', baselineThreadId, 'new reply from another browser context');
      const newThreadId = await createThreadViaBrowser(other, app.baseURL, 'pub', {
        subject: 'activity badge audit',
        body: 'new thread from another browser context',
      });

      await page.goto(app.baseURL);
      await expect(page.locator('.board-card-new-thread-badge, .board-card-new-reply-badge').first()).toBeVisible();
      await page.goto(`${app.baseURL}/pub/catalog`);
      await expect(page.locator('.catalog-activity-badge').first()).toBeVisible();
      await page.goto(`${app.baseURL}/pub/thread/${baselineThreadId}`);
      await expectSafePage(page);
      await page.goto(`${app.baseURL}/pub/catalog`);
      await expect(page.locator('.catalog-activity-badge')).toHaveCount(0);
      await page.goto(`${app.baseURL}/pub/thread/${newThreadId}`);
      await expectSafePage(page);
    } finally {
      await contextTwo.close();
    }
  });
});

async function createThreadViaBrowser(
  page: Page,
  baseURL: string,
  board: string,
  options: { subject: string; body: string; poll?: boolean },
): Promise<number> {
  await page.goto(`${baseURL}/${board}`);
  await expectSafePage(page);
  await openPostFormIfNeeded(page);
  const form = page.locator(`form[action="/${board}"]`).first();
  await form.locator('input[name="subject"]').fill(options.subject);
  await form.locator('textarea[name="body"]').fill(options.body);
  if (options.poll) {
    const poll = form.locator('details.poll-creator').first();
    if (!(await poll.evaluate((element) => (element as HTMLDetailsElement).open))) {
      await poll.locator('summary').click();
    }
    await form.locator('input[name="poll_question"]').fill('Audit poll question?');
    await form.locator('input[name="poll_option"]').nth(0).fill('First option');
    await form.locator('input[name="poll_option"]').nth(1).fill('Second option');
  }
  await Promise.all([
    page.waitForURL(new RegExp(`/${board}/thread/\\d+`)),
    form.getByRole('button', { name: /post thread/i }).click(),
  ]);
  await expectSafePage(page);
  return threadIdFromUrl(page.url());
}

async function createReplyViaBrowser(
  page: Page,
  baseURL: string,
  board: string,
  threadId: number,
  body: string,
): Promise<void> {
  await page.goto(`${baseURL}/${board}/thread/${threadId}`);
  await expectSafePage(page);
  await openPostFormIfNeeded(page);
  const form = page.locator(`form[action="/${board}/thread/${threadId}"]`).first();
  await form.locator('textarea[name="body"]').fill(body);
  await Promise.all([
    page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
    form.getByRole('button', { name: /post reply/i }).click(),
  ]);
  await expectSafePage(page);
}

async function openPostFormIfNeeded(page: Page): Promise<void> {
  const formWrap = page.locator('#post-form-wrap').first();
  if (await formWrap.isVisible()) {
    return;
  }
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  await expect(formWrap).toBeVisible();
}

async function openDetailsIfClosed(details: Locator): Promise<void> {
  if (!(await details.evaluate((element) => (element as HTMLDetailsElement).open))) {
    await details.locator('summary').click();
  }
}

function threadIdFromUrl(url: string): number {
  const match = url.match(/\/thread\/(\d+)/);
  if (!match) {
    throw new Error(`no thread id in ${url}`);
  }
  return Number(match[1]);
}

async function postIdFrom(post: Locator): Promise<number> {
  const id = await post.getAttribute('id');
  const numeric = Number(id?.replace(/^p/, ''));
  expect(Number.isInteger(numeric), 'post id should be present').toBe(true);
  return numeric;
}

async function visibleFallbackAction(scope: Locator, actionPath: string): Promise<number> {
  const formCount = await scope.locator(`form[action="${actionPath}"]:visible, form[action$="${actionPath}"]:visible`).count();
  const linkCount = await scope.locator(`a[href="${actionPath}"]:visible, a[href$="${actionPath}"]:visible`).count();
  return formCount + linkCount;
}
