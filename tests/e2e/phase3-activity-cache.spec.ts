import type { TestInfo } from '@playwright/test';
import {
  ADMIN_PASSWORD,
  adminPasswordHash,
  createReply,
  createThread,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  test,
  uniqueShort,
  unlockBoard,
  type RustChanServer,
} from './helpers';
import {
  boardCard,
  expectBoardBadge,
  expectBoardBadgeCleared,
  expectThreadSummaryBadge,
  expectThreadSummaryBadgeCleared,
  restartWithActivitySettings,
  seedReaderBaselines,
  writerPage,
} from './activity-helpers';

const BF_CACHE_PROJECTS = new Set(['chromium', 'mobile-webkit']);
const NO_JS_PROJECTS = new Set(['firefox-nojs']);

test.describe('phase 3 activity cache and browser restore behavior', () => {
  test('activity cookies survive restart and BFCache restore does not resurrect cleared badges', async ({
    page,
    browser,
    app,
  }, testInfo) => {
    test.skip(
      !BF_CACHE_PROJECTS.has(testInfo.project.name),
      'BFCache and restart activity coverage is sampled on Chromium and mobile WebKit',
    );

    await restartWithActivitySettings(app, {
      homepageNewThreadBadgesEnabled: true,
      homepageNewReplyBadgesEnabled: true,
      threadNewReplyBadgesEnabled: true,
    });
    const board = await createActivityBoard(app, testInfo, 'bf');
    const baselineThread = await createThread(page, app, board, {
      subject: 'phase 3 baseline',
      body: 'reader baseline before activity',
    });
    await seedReaderBaselines(page, app, [[board, baselineThread]]);

    const writer = await writerPage(browser, testInfo);
    try {
      await createReply(writer.page, app, board, baselineThread, 'phase 3 unread reply before restart');
      await createThread(writer.page, app, board, {
        subject: 'phase 3 unread thread before restart',
        body: 'unread thread survives restart in reader cookies',
      });
    } finally {
      await writer.context.close();
    }

    await page.goto(app.baseURL);
    await expectBoardBadge(page, board, '.board-card-new-thread-badge', /1 New Threads/i);
    await expectBoardBadge(page, board, '.board-card-new-reply-badge', /1 New Replies/i);

    await app.restart();
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expectBoardBadge(page, board, '.board-card-new-thread-badge', /1 New Threads/i);
    await expectBoardBadge(page, board, '.board-card-new-reply-badge', /1 New Replies/i);

    await page.goto(`${app.baseURL}/${board}`);
    await expectThreadSummaryBadge(page, baselineThread, /1 New/i);
    await page.goto(`${app.baseURL}/${board}/thread/${baselineThread}`);
    await expectSafePage(page);
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await expectThreadSummaryBadgeCleared(page, baselineThread);
    await page.goForward({ waitUntil: 'domcontentloaded' });
    await expectSafePage(page);

    await page.goto(app.baseURL);
    await expectBoardBadgeCleared(page, board, '.board-card-new-thread-badge');
    await expectBoardBadgeCleared(page, board, '.board-card-new-reply-badge');
    await app.restart();
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expectBoardBadgeCleared(page, board, '.board-card-new-thread-badge');
    await expectBoardBadgeCleared(page, board, '.board-card-new-reply-badge');
  });

  test('protected board activity cookies do not leak badges after the unlock cookie is removed', async ({
    page,
    browser,
    app,
  }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'protected activity leak audit runs once in Chromium');

    await restartWithActivitySettings(app, {
      homepageNewThreadBadgesEnabled: true,
      homepageNewReplyBadgesEnabled: true,
      threadNewReplyBadgesEnabled: true,
    });
    const board = await createProtectedActivityBoard(app, testInfo);

    const writer = await writerPage(browser, testInfo);
    let threadId = 0;
    try {
      await unlockBoard(writer.page, app, board, ADMIN_PASSWORD);
      threadId = await createThread(writer.page, app, board, {
        subject: 'protected baseline',
        body: 'protected baseline body',
      });
    } finally {
      await writer.context.close();
    }

    await unlockBoard(page, app, board, ADMIN_PASSWORD);
    await seedReaderBaselines(page, app, [[board, threadId]]);
    await page.context().clearCookies({ name: `rustchan_board_access_${board}` });

    const secondWriter = await writerPage(browser, testInfo);
    try {
      await unlockBoard(secondWriter.page, app, board, ADMIN_PASSWORD);
      await createReply(secondWriter.page, app, board, threadId, 'protected unread reply');
      await createThread(secondWriter.page, app, board, {
        subject: 'protected unread thread',
        body: 'protected unread thread body',
      });
    } finally {
      await secondWriter.context.close();
    }

    await page.goto(app.baseURL);
    const card = boardCard(page, board);
    await expect(card.locator('.new-activity-badge')).toHaveCount(0);
    await expect(card.locator('a.board-card-link')).toHaveAttribute('href', `/${board}/unlock`);
    await expect(page.locator('body')).not.toContainText('protected unread reply');
    await expect(page.locator('body')).not.toContainText('protected unread thread');
  });

  test('Firefox no-JS activity badges clear through plain navigation', async ({ page, browser, app }, testInfo) => {
    test.skip(!NO_JS_PROJECTS.has(testInfo.project.name), 'no-JS activity fallback is Firefox-specific signal');

    await restartWithActivitySettings(app, {
      homepageNewThreadBadgesEnabled: true,
      homepageNewReplyBadgesEnabled: true,
      threadNewReplyBadgesEnabled: true,
    });
    const board = await createActivityBoard(app, testInfo, 'nj');
    const threadId = await createThread(page, app, board, {
      subject: 'phase 3 no-js baseline',
      body: 'baseline before no-js unread activity',
    });
    await seedReaderBaselines(page, app, [[board, threadId]]);

    const writer = await browser.newContext({ javaScriptEnabled: false });
    try {
      const writerTab = await writer.newPage();
      await createReply(writerTab, app, board, threadId, 'phase 3 no-js unread reply');
    } finally {
      await writer.close();
    }

    await page.goto(`${app.baseURL}/${board}`);
    await expectThreadSummaryBadge(page, threadId, /1 New/i);
    await page.locator(`#t${threadId} a[href="/${board}/thread/${threadId}"]`).first().click();
    await expectSafePage(page);
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await expectThreadSummaryBadgeCleared(page, threadId);
  });
});

async function createActivityBoard(app: RustChanServer, testInfo: TestInfo, prefix: string): Promise<string> {
  const board = uniqueShort(prefix, testInfo);
  await app.stop();
  app.createBoardCli({
    short: board,
    name: `Phase 3 Activity ${board}`,
    description: `Phase 3 activity board ${board}`,
  });
  setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
  await app.start();
  return board;
}

async function createProtectedActivityBoard(app: RustChanServer, testInfo: TestInfo): Promise<string> {
  const board = uniqueShort('pa', testInfo);
  await app.stop();
  app.createBoardCli({
    short: board,
    name: `Phase 3 Protected ${board}`,
    description: 'Protected activity must not leak while locked',
  });
  setBoardFixtureSettings(app, board, {
    accessMode: 'view_password',
    accessPasswordHash: adminPasswordHash(app),
    postCooldownSecs: 0,
  });
  await app.start();
  return board;
}
