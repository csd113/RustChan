import type { Browser, BrowserContext, Page, TestInfo } from '@playwright/test';
import {
  boardId,
  expect,
  setSiteFixtureSettings,
  sqliteQuery,
  type RustChanServer,
} from './helpers';

export type BadgeSettings = {
  homepageNewThreadBadgesEnabled: boolean;
  homepageNewReplyBadgesEnabled: boolean;
  threadNewReplyBadgesEnabled: boolean;
};

export async function restartWithActivitySettings(
  app: RustChanServer,
  settings: BadgeSettings,
): Promise<void> {
  setSiteFixtureSettings(app, settings);
}

export async function seedReaderBaselines(
  page: Page,
  app: RustChanServer,
  entries: Array<[string, number]>,
): Promise<void> {
  const now = Math.floor(Date.now() / 1000);
  const boardMarkers: string[] = [];
  const threadMarkers: string[] = [];
  for (const [board, threadId] of entries) {
    const id = boardId(app, board);
    const createdAt = Number(sqliteQuery(app, `SELECT created_at FROM threads WHERE id = ${threadId};`));
    const replyCount = Number(sqliteQuery(app, `SELECT reply_count FROM threads WHERE id = ${threadId};`));
    if (!Number.isFinite(createdAt) || !Number.isFinite(replyCount)) {
      throw new Error(`thread ${threadId} baseline not found for /${board}/`);
    }
    boardMarkers.push(`${id}.${createdAt}.${threadId}.${now}`);
    threadMarkers.push(`${threadId}.${replyCount}.${now}`);
  }
  await page.context().addCookies([
    {
      name: 'rustchan_board_activity',
      value: `v1|${boardMarkers.join('|')}`,
      url: app.baseURL,
      httpOnly: true,
      sameSite: 'Lax',
    },
    {
      name: 'rustchan_thread_activity',
      value: `v1|${threadMarkers.join('|')}`,
      url: app.baseURL,
      httpOnly: true,
      sameSite: 'Lax',
    },
  ]);
}

export async function writerPage(
  browser: Browser,
  testInfo: TestInfo,
): Promise<{ context: BrowserContext; page: Page }> {
  const context = await browser.newContext({
    javaScriptEnabled: testInfo.project.name !== 'firefox-nojs',
  });
  return { context, page: await context.newPage() };
}

export function boardCard(page: Page, board: string) {
  return page.locator('.board-card').filter({
    has: page.locator(`a[href="/${board}/catalog"], a[href="/${board}"], a[href="/${board}/unlock"]`),
  }).first();
}

export async function expectBoardBadge(
  page: Page,
  board: string,
  badgeSelector: string,
  text: RegExp,
): Promise<void> {
  await expect(boardCard(page, board).locator(badgeSelector)).toContainText(text);
}

export async function expectBoardBadgeCleared(
  page: Page,
  board: string,
  badgeSelector: string,
): Promise<void> {
  await expect(boardCard(page, board).locator(badgeSelector)).toHaveCount(0);
}

export async function expectThreadSummaryBadge(
  page: Page,
  threadId: number,
  text: RegExp,
): Promise<void> {
  await expect(page.locator(`#t${threadId} .thread-summary-activity-badge`)).toContainText(text);
}

export async function expectThreadSummaryBadgeCleared(page: Page, threadId: number): Promise<void> {
  await expect(page.locator(`#t${threadId} .thread-summary-activity-badge`)).toHaveCount(0);
}

export async function expectCatalogBadge(page: Page, threadId: number, text: RegExp): Promise<void> {
  await expect(page.locator(`.catalog-item:has(a[href$="/thread/${threadId}"]) .catalog-activity-badge`)).toContainText(text);
}

export async function expectCatalogBadgeCleared(page: Page, threadId: number): Promise<void> {
  await expect(page.locator(`.catalog-item:has(a[href$="/thread/${threadId}"]) .catalog-activity-badge`)).toHaveCount(0);
}
