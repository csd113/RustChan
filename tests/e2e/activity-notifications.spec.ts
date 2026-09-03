import { expect, test, type Browser, type BrowserContext, type Page, type TestInfo } from '@playwright/test';
import {
  createReplyViaRequest,
  createThreadViaRequest,
  expectSafePage,
  gotoAppPage,
  RustChanServer,
  setBoardFixtureSettings,
  setSiteFixtureSettings,
  uniqueShort,
} from './helpers';
import {
  type BadgeSettings,
  expectBoardBadge,
  expectBoardBadgeCleared,
  expectCatalogBadge,
  expectCatalogBadgeCleared,
  expectThreadSummaryBadge,
  expectThreadSummaryBadgeCleared,
  seedReaderBaselines,
  writerPage,
} from './activity-helpers';

test.describe('new activity notification clearing audit', () => {
  test.describe.configure({ timeout: 240_000 });

  test.describe('scripted projects', () => {
    test.skip(
      ({ browserName, isMobile }) => !(browserName === 'chromium' || (browserName === 'webkit' && isMobile)),
      'focused Chromium and mobile WebKit activity audit',
    );

  test('homepage, board, catalog, and thread badges clear after viewed in Chromium and mobile WebKit', async ({
    browser,
  }, testInfo) => {
    test.setTimeout(240_000);

    const settings = {
      homepageNewThreadBadgesEnabled: true,
      homepageNewReplyBadgesEnabled: true,
      threadNewReplyBadgesEnabled: true,
    };
    annotateActivityScenario(testInfo, 'all', settings);

    const boardA = uniqueShort('aa', testInfo);
    const boardB = uniqueShort('bb', testInfo);
    const app = await createActivityApp(settings, boardA, boardB);
    const reader = await projectPage(browser, testInfo);
    const page = reader.page;

    try {
      const threadA = await createThreadViaRequest(page, app, boardA, {
        subject: `baseline ${boardA}`,
        body: 'reader-visible baseline on the first audit board',
      });
      const threadB = await createThreadViaRequest(page, app, boardB, {
        subject: `baseline ${boardB}`,
        body: 'reader-visible baseline on the second audit board',
      });
      await seedReaderBaselines(page, app, [
        [boardA, threadA],
        [boardB, threadB],
      ]);

      const writer = await writerPage(browser, testInfo);
      try {
        await createReplyViaRequest(writer.page, app, boardA, threadA, `reply for ${boardA}`);
        await createThreadViaRequest(writer.page, app, boardA, {
          subject: `new thread ${boardA}`,
          body: 'new board thread from another browser context',
        });
        await createReplyViaRequest(writer.page, app, boardB, threadB, `reply for ${boardB}`);
        await createThreadViaRequest(writer.page, app, boardB, {
          subject: `new thread ${boardB}`,
          body: 'unrelated unread board activity',
        });
      } finally {
        await writer.context.close();
      }

      await gotoAppPage(page, app.baseURL);
      await expectBoardBadge(page, boardA, '.board-card-new-thread-badge', /1 New Threads/i);
      await expectBoardBadge(page, boardA, '.board-card-new-reply-badge', /1 New Replies/i);
      await expectBoardBadge(page, boardB, '.board-card-new-thread-badge', /1 New Threads/i);

      await gotoAppPage(page, `${app.baseURL}/${boardA}`);
      await expectThreadSummaryBadge(page, threadA, /1 New/i);
      await gotoAppPage(page, app.baseURL);
      await expectBoardBadgeCleared(page, boardA, '.board-card-new-thread-badge');
      await expectBoardBadgeCleared(page, boardA, '.board-card-new-reply-badge');
      await expectBoardBadge(page, boardB, '.board-card-new-thread-badge', /1 New Threads/i);
      await expectBoardBadge(page, boardB, '.board-card-new-reply-badge', /1 New Replies/i);

      await gotoAppPage(page, `${app.baseURL}/${boardB}/catalog`);
      await expectCatalogBadge(page, threadB, /1 New/i);
      await page.locator(`.catalog-item:has(a[href="/${boardB}/thread/${threadB}"]) a[href="/${boardB}/thread/${threadB}"]`).first().click();
      await expectSafePage(page);
      await expect(page.locator('#thread-posts')).toHaveAttribute('data-thread-id', String(threadB));

      await page.goBack({ waitUntil: 'domcontentloaded' });
      await expectCatalogBadgeCleared(page, threadB);
      await page.reload({ waitUntil: 'domcontentloaded' });
      await expectCatalogBadgeCleared(page, threadB);

      await page.goForward({ waitUntil: 'domcontentloaded' });
      await expectSafePage(page);
      await gotoAppPage(page, app.baseURL);
      await expectBoardBadgeCleared(page, boardB, '.board-card-new-reply-badge');
      await gotoAppPage(page, `${app.baseURL}/${boardB}/catalog`);
      await expectCatalogBadgeCleared(page, threadB);
      await gotoAppPage(page, `${app.baseURL}/${boardB}`);
      await expectThreadSummaryBadgeCleared(page, threadB);
    } finally {
      await reader.context.close();
      await stopActivityApp(testInfo, app);
    }
  });

  test('admin toggles keep homepage and thread badges independent', async ({ browser }, testInfo) => {
    test.setTimeout(240_000);

    const boards = ['home', 'thread', 'off'].map((label) => uniqueShort(label, testInfo));
    const app = await createActivityApp({
      homepageNewThreadBadgesEnabled: false,
      homepageNewReplyBadgesEnabled: false,
      threadNewReplyBadgesEnabled: false,
    }, ...boards);
    const reader = await projectPage(browser, testInfo);
    const page = reader.page;

    try {
      await runToggleScenario(page, browser, app, testInfo, 'home', boards[0], {
        homepageNewThreadBadgesEnabled: true,
        homepageNewReplyBadgesEnabled: false,
        threadNewReplyBadgesEnabled: false,
      }, {
        expectHomepageThreadBadge: true,
        expectHomepageReplyBadge: false,
        expectThreadBadge: false,
      });

      await runToggleScenario(page, browser, app, testInfo, 'thread', boards[1], {
        homepageNewThreadBadgesEnabled: false,
        homepageNewReplyBadgesEnabled: false,
        threadNewReplyBadgesEnabled: true,
      }, {
        expectHomepageThreadBadge: false,
        expectHomepageReplyBadge: false,
        expectThreadBadge: true,
      });

      await runToggleScenario(page, browser, app, testInfo, 'off', boards[2], {
        homepageNewThreadBadgesEnabled: false,
        homepageNewReplyBadgesEnabled: false,
        threadNewReplyBadgesEnabled: false,
      }, {
        expectHomepageThreadBadge: false,
        expectHomepageReplyBadge: false,
        expectThreadBadge: false,
      });
    } finally {
      await reader.context.close();
      await stopActivityApp(testInfo, app);
    }
  });
  });

  test.describe('Firefox no-JS project', () => {
    test.skip(
      ({ browserName, javaScriptEnabled }) => browserName !== 'firefox' || javaScriptEnabled,
      'Firefox no-JS activity smoke',
    );

  test('server-rendered read state clears without JavaScript', async ({ browser }, testInfo) => {
    test.setTimeout(150_000);

    const settings = {
      homepageNewThreadBadgesEnabled: true,
      homepageNewReplyBadgesEnabled: true,
      threadNewReplyBadgesEnabled: true,
    };
    annotateActivityScenario(testInfo, 'nojs', settings);
    const board = uniqueShort('nj', testInfo);
    const app = await createActivityApp(settings, board);
    const reader = await projectPage(browser, testInfo);
    const page = reader.page;

    try {
      const threadId = await createThreadViaRequest(page, app, board, {
        subject: 'no js baseline',
        body: 'baseline visible before unread activity',
      });
      await seedReaderBaselines(page, app, [[board, threadId]]);

      const writer = await browser.newContext({ javaScriptEnabled: false });
      try {
        const writerTab = await writer.newPage();
        await createReplyViaRequest(writerTab, app, board, threadId, 'no-js unread reply');
        await createThreadViaRequest(writerTab, app, board, {
          subject: 'no-js unread thread',
          body: 'server-rendered unread thread',
        });
      } finally {
        await writer.close();
      }

      await gotoAppPage(page, app.baseURL);
      await expectBoardBadge(page, board, '.board-card-new-thread-badge', /1 New Threads/i);
      await expectBoardBadge(page, board, '.board-card-new-reply-badge', /1 New Replies/i);

      await gotoAppPage(page, `${app.baseURL}/${board}`);
      await expectThreadSummaryBadge(page, threadId, /1 New/i);
      await page.locator(`#t${threadId} a[href="/${board}/thread/${threadId}"]`).first().click();
      await expectSafePage(page);
      await page.goBack({ waitUntil: 'domcontentloaded' });
      await expectThreadSummaryBadgeCleared(page, threadId);
      await page.goForward({ waitUntil: 'domcontentloaded' });
      await expectSafePage(page);
      await gotoAppPage(page, app.baseURL);
      await expectBoardBadgeCleared(page, board, '.board-card-new-thread-badge');
      await expectBoardBadgeCleared(page, board, '.board-card-new-reply-badge');
      await gotoAppPage(page, `${app.baseURL}/${board}`);
      await expectThreadSummaryBadgeCleared(page, threadId);
    } finally {
      await reader.context.close();
      await stopActivityApp(testInfo, app);
    }
  });
  });
});

function annotateActivityScenario(
  testInfo: TestInfo,
  label: string,
  settings: BadgeSettings,
): void {
  testInfo.annotations.push({
    type: 'activity-settings',
    description: `${label}: homepage_new_thread=${settings.homepageNewThreadBadgesEnabled}, homepage_new_reply=${settings.homepageNewReplyBadgesEnabled}, thread_new_reply=${settings.threadNewReplyBadgesEnabled}`,
  });
}

async function createActivityApp(settings: BadgeSettings, ...boards: string[]): Promise<RustChanServer> {
  const app = await RustChanServer.create();
  try {
    await app.initializeDefaultData();
    setSiteFixtureSettings(app, settings);
    createAuditBoards(app, ...boards);
    await app.start();
    return app;
  } catch (error) {
    await app.dispose().catch(() => undefined);
    throw error;
  }
}

function createAuditBoards(app: RustChanServer, ...boards: string[]): void {
  for (const board of boards) {
    app.createBoardCli({
      short: board,
      name: `Activity ${board}`,
      description: `Activity audit board ${board}`,
    });
    setBoardFixtureSettings(app, board, { postCooldownSecs: 0 });
  }
}

async function projectPage(
  browser: Browser,
  testInfo: TestInfo,
): Promise<{ context: BrowserContext; page: Page }> {
  const context = await browser.newContext({
    javaScriptEnabled: testInfo.project.name !== 'firefox-nojs',
  });
  return { context, page: await context.newPage() };
}

async function stopActivityApp(testInfo: TestInfo, app: RustChanServer): Promise<void> {
  if (testInfo.status !== testInfo.expectedStatus) {
    await testInfo.attach('rustchan-server.log', {
      body: await app.logs(),
      contentType: 'text/plain',
    });
  }
  await app.dispose();
}

async function runToggleScenario(
  page: Page,
  browser: Browser,
  app: RustChanServer,
  testInfo: TestInfo,
  label: string,
  board: string,
  settings: BadgeSettings,
  expected: {
    expectHomepageThreadBadge: boolean;
    expectHomepageReplyBadge: boolean;
    expectThreadBadge: boolean;
  },
): Promise<void> {
  annotateActivityScenario(testInfo, label, settings);
  setSiteFixtureSettings(app, settings);
  const threadId = await createThreadViaRequest(page, app, board, {
    subject: `${label} baseline`,
    body: 'baseline before toggle assertion',
  });
  await seedReaderBaselines(page, app, [[board, threadId]]);

  const writer = await writerPage(browser, testInfo);
  try {
    await createReplyViaRequest(writer.page, app, board, threadId, `${label} unread reply`);
    await createThreadViaRequest(writer.page, app, board, {
      subject: `${label} unread thread`,
      body: 'toggle-specific unread thread',
    });
  } finally {
    await writer.context.close();
  }

  await gotoAppPage(page, app.baseURL);
  if (expected.expectHomepageThreadBadge) {
    await expectBoardBadge(page, board, '.board-card-new-thread-badge', /1 New Threads/i);
  } else {
    await expectBoardBadgeCleared(page, board, '.board-card-new-thread-badge');
  }
  if (expected.expectHomepageReplyBadge) {
    await expectBoardBadge(page, board, '.board-card-new-reply-badge', /1 New Replies/i);
  } else {
    await expectBoardBadgeCleared(page, board, '.board-card-new-reply-badge');
  }

  await gotoAppPage(page, `${app.baseURL}/${board}/catalog`);
  if (expected.expectThreadBadge) {
    await expectCatalogBadge(page, threadId, /1 New/i);
  } else {
    await expectCatalogBadgeCleared(page, threadId);
  }
}
