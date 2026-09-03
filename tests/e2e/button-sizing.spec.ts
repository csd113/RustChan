import type { Locator, Page, TestInfo } from '@playwright/test';
import {
  ADMIN_PASSWORD,
  RustChanServer,
  adminPasswordHash,
  createReply,
  createThread,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  setThreadFixtureState,
  test,
  unlockBoard,
  uniqueShort,
} from './helpers';

type ControlMetrics = {
  name: string;
  width: number;
  height: number;
  top: number;
  bottom: number;
  left: number;
  right: number;
  borderTopWidth: string;
  backgroundColor: string;
  display: string;
};

const DESKTOP_MIN_ACTION_HEIGHT = 30;
const MOBILE_MIN_ACTION_HEIGHT = 38;

test.describe('public button and action-control sizing', () => {
  test('public action controls stay consistently sized across scripted and no-JS controls', async ({ page, app }, testInfo) => {
    const fixture = await seedPublicButtonSizingFixture(page, app, testInfo);
    await page.goto(`${app.baseURL}/`);
    await expectSafePage(page);
    await expectControlStable(page.locator('.home-btn'), 'home button');
    await expectControlStable(page.locator('#theme-picker-btn'), 'theme picker button');
    await expectControlStable(page.locator('.user-preferences-summary'), 'preferences button');

    await inspectPreferencesControls(page, testInfo);

    await page.goto(`${app.baseURL}/${fixture.board}`);
    await expectSafePage(page);
    await expectControlStable(page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first(), 'new thread toggle');
    await inspectThreadListControls(page, testInfo);

    await page.goto(`${app.baseURL}/${fixture.board}/catalog`);
    await expectSafePage(page);
    await inspectCatalogControls(page, testInfo);

    await page.goto(`${app.baseURL}/${fixture.board}/thread/${fixture.threadId}`);
    await expectSafePage(page);
    await inspectThreadPageControls(page, testInfo);
    await inspectReportFlow(page, testInfo);
    await inspectEditDeleteFlows(page, app.baseURL, fixture.board, testInfo);

    await page.goto(`${app.baseURL}/${fixture.board}/archive`);
    await expectSafePage(page);
    await expectControlStable(page.locator(`a[href="/${fixture.board}/thread/${fixture.archivedThreadId}"]`).first(), 'archive thread link');

    await page.goto(`${app.baseURL}/${fixture.passwordBoard}/unlock`);
    await expectSafePage(page);
    await expectControlStable(page.getByRole('button', { name: /unlock board/i }), 'password board submit');
    await unlockBoard(page, app, fixture.passwordBoard, ADMIN_PASSWORD);
    await expect(page).toHaveURL(new RegExp(`/${fixture.passwordBoard}`));
  });
});

async function seedPublicButtonSizingFixture(
  page: Page,
  app: RustChanServer,
  testInfo: TestInfo,
): Promise<{ board: string; passwordBoard: string; threadId: number; archivedThreadId: number }> {
  const board = uniqueShort('btn', testInfo);
  const passwordBoard = uniqueShort('pw', testInfo);
  app.createBoardCli({ short: board, name: 'Button Sizing', description: 'Public button sizing fixture' });
  app.createBoardCli({ short: passwordBoard, name: 'Password Buttons', description: 'Password prompt fixture' });
  setBoardFixtureSettings(app, board, {
    allowEditing: true,
    allowSelfDelete: true,
    allowImages: true,
    allowArchive: true,
    maxThreads: 10,
    maxArchivedThreads: 4,
  });
  setBoardFixtureSettings(app, passwordBoard, {
    accessMode: 'view_password',
    accessPasswordHash: adminPasswordHash(app),
  });

  const threadId = await createThread(page, app, board, {
    subject: 'post action controls',
    body: 'OP with media for button sizing',
    filePath: app.fixtures().tinyPng,
  });
  const opPostId = await firstPostId(page);
  await createReply(page, app, board, threadId, `quoting the OP >>${opPostId}`);
  await createReply(page, app, board, threadId, 'second reply with own edit and delete controls');

  const lockedThreadId = await createThread(page, app, board, {
    subject: 'locked public state',
    body: 'locked state remains visible',
  });
  setThreadFixtureState(app, lockedThreadId, { locked: true });

  const archivedThreadId = await createThread(page, app, board, {
    subject: 'archived public state',
    body: 'archived state remains visible',
  });
  setThreadFixtureState(app, archivedThreadId, { archived: true });

  await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
  await expectSafePage(page);
  return { board, passwordBoard, threadId, archivedThreadId };
}

async function firstPostId(page: Page): Promise<number> {
  const id = await page.locator('.post').first().getAttribute('id');
  const postId = Number(id?.replace(/^p/, ''));
  expect(Number.isInteger(postId)).toBeTruthy();
  return postId;
}

async function inspectPreferencesControls(page: Page, testInfo: TestInfo): Promise<void> {
  await page.locator('.user-preferences-summary').click();
  if (isNoJs(testInfo)) {
    const preferences = page.locator('.user-preferences-noscript');
    await expect(preferences).toBeVisible();
    await expectControlStable(
      preferences.locator('button[name="theme"]').first(),
      'no-JS preferences theme button',
    );
    await expectControlStable(
      preferences.locator('button[name="show_activity_badges"]').first(),
      'no-JS preferences activity button',
    );
    await page.locator('.user-preferences-summary').click();
    return;
  }
  await expect(page.locator('.user-preferences-form')).toBeVisible();
  await expectControlStable(page.locator('.user-preferences-form select[name="theme"]'), 'preferences theme select');
  await expectControlStable(page.locator('.user-preferences-form input[type="checkbox"]').first(), 'preferences checkbox');
  if (isMobile(testInfo)) {
    await expectControlStable(page.locator('.user-preferences-mobile-close'), 'mobile preferences close');
  }
  await page.keyboard.press('Escape');
}

async function inspectThreadListControls(page: Page, testInfo: TestInfo): Promise<void> {
  await expectControlStable(page.locator('.board-nav-link[href$="/catalog"]').first(), 'catalog nav link');
  const paginationLink = page.locator('.pagination a').first();
  if (await paginationLink.count()) {
    await expectControlStable(paginationLink, 'pagination link');
  }
  await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first().click();
  const submit = page.locator('.post-form button[type="submit"]:visible').first();
  await expectControlStable(submit, 'new thread submit');
  await expectUsableActionHeight(submit, testInfo);
}

async function inspectCatalogControls(page: Page, testInfo: TestInfo): Promise<void> {
  if (isNoJs(testInfo)) {
    await expect(page.locator('.catalog-thread-menu-toggle').first()).toBeHidden();
    const actions = page.locator('.catalog-thread-fallback-actions').first();
    await expectControlStable(actions.locator('.catalog-thread-fallback-summary'), 'catalog fallback actions summary');
    await expectControlStable(actions.locator('.catalog-thread-fallback-submit').first(), 'catalog fallback action submit');
    await expectReportFallbackControls(actions, 'catalog');
    return;
  }

  await page.locator('.catalog-item').first().hover();
  const toggle = page.locator('.catalog-thread-menu-toggle').first();
  await expectControlStable(toggle, 'catalog thread menu toggle');
  await toggle.click();
  const menuItem = page.locator('.catalog-thread-menu-item').first();
  await expect(menuItem).toBeVisible();
  await expectControlStable(menuItem, 'catalog thread menu item');
  await expectNoAwkwardWrap(menuItem, 'catalog thread menu item');
}

async function inspectThreadPageControls(page: Page, testInfo: TestInfo): Promise<void> {
  await expectControlStable(page.locator('.thread-nav-btn[data-action="fetch-updates"]').first(), 'thread update button');
  await expectControlStable(page.locator('.autoupdate-label input[type="checkbox"]').first(), 'autoupdate checkbox');
  await expectControlStable(page.locator('.post-num').first(), 'quote/post number link');

  const imagePreview = page.locator('.media-preview').first();
  await expectControlStable(imagePreview, 'media preview control', { checkWrap: false });
  if (isNoJs(testInfo)) {
    await expect(imagePreview).toHaveAttribute('href', /\/boards\//);
  } else {
    await imagePreview.click();
    await expect(page.locator('.media-expanded-image, .media-expanded-video, .media-expanded-pdf').first()).toBeVisible();
    const closeButton = page.locator('.media-close-btn').first();
    if (await closeButton.isVisible()) {
      await expectControlStable(closeButton, 'media close button');
    }
  }

  await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').first().click();
  const submit = page.locator('.post-form button[type="submit"]:visible').first();
  await expectControlStable(submit, 'reply submit');
  await expectUsableActionHeight(submit, testInfo);

  const controls = page.locator('.reply .post-controls').filter({ has: page.locator('.edit-btn') }).first();
  await expect(controls).toBeVisible();
  const edit = controls.locator('.edit-btn').first();
  const del = controls.locator('.del-btn').first();
  if (isNoJs(testInfo)) {
    await expect(page.locator('.post-controls .report-btn').first()).toBeHidden();
    const report = controls.locator('.report-fallback-summary').first();
    const editMetrics = await metrics(edit, 'edit');
    const deleteMetrics = await metrics(del, 'delete');
    const reportMetrics = await metrics(report, 'report fallback');

    expectSimilarHeight(reportMetrics, editMetrics, 2);
    expectSimilarHeight(reportMetrics, deleteMetrics, 2);
    await expectUsableActionHeight(report, testInfo);
    await expectReportFallbackControls(controls, 'thread');
    return;
  }

  const report = controls.locator('.report-btn').first();
  const editMetrics = await metrics(edit, 'edit');
  const deleteMetrics = await metrics(del, 'delete');
  const reportMetrics = await metrics(report, 'report');

  expectSimilarHeight(reportMetrics, editMetrics, 2);
  expectSimilarHeight(reportMetrics, deleteMetrics, 2);
  if (!isMobile(testInfo)) {
    expectSameRow(reportMetrics, editMetrics, 5);
    expectSameRow(reportMetrics, deleteMetrics, 5);
  }
  expect(reportMetrics.width).toBeLessThanOrEqual(deleteMetrics.width + 16);
  expect(reportMetrics.width).toBeGreaterThanOrEqual(editMetrics.width - 8);
  expect(reportMetrics.borderTopWidth, 'report action should share post-action border chrome').toBe(editMetrics.borderTopWidth);
  expect(reportMetrics.backgroundColor, 'report action should share post-action background').toBe(editMetrics.backgroundColor);
  await expectUsableActionHeight(edit, testInfo);
  await expectUsableActionHeight(del, testInfo);
  await expectUsableActionHeight(report, testInfo);
}

async function inspectReportFlow(page: Page, testInfo: TestInfo): Promise<void> {
  if (isNoJs(testInfo)) {
    const fallback = page.locator('.post-controls .report-fallback-form').first();
    await openDetailsIfClosed(fallback.locator('.report-fallback-details').first());
    await expectControlStable(fallback.locator('.report-fallback-reason'), 'report fallback reason');
    await expectControlStable(fallback.locator('.report-fallback-submit'), 'report fallback submit');
    await expectUsableActionHeight(fallback.locator('.report-fallback-submit'), testInfo);
    return;
  }

  await page.locator('.post-controls .report-btn').first().click();
  await expect(page.locator('#report-modal')).toBeVisible();
  const cancel = page.locator('#report-modal .compress-cancel-btn');
  const submit = page.locator('#report-submit-btn');
  await expectControlStable(cancel, 'report modal cancel');
  await expectControlStable(submit, 'report modal submit');
  expectSimilarHeight(await metrics(cancel, 'report cancel'), await metrics(submit, 'report submit'), 2);
  await expectUsableActionHeight(submit, testInfo);
  await cancel.click();
  await expect(page.locator('#report-modal')).toBeHidden();
}

async function inspectEditDeleteFlows(page: Page, baseURL: string, board: string, testInfo: TestInfo): Promise<void> {
  const threadUrl = page.url();
  const edit = page.locator('.post-controls .edit-btn').first();
  if (isNoJs(testInfo)) {
    const editHref = await edit.getAttribute('href');
    expect(editHref).toBeTruthy();
    await page.goto(`${baseURL}${editHref}`);
    await expectSafePage(page);
    await expectControlStable(page.locator('.self-action-form button[type="submit"]'), 'edit fallback save');
    await expectControlStable(page.locator('.self-action-form .edit-btn'), 'edit fallback cancel');
  } else {
    await edit.click();
    await expect(page.locator('#edit-modal')).toBeVisible();
    await expectControlStable(page.locator('#edit-modal-form button[type="submit"]'), 'edit modal save');
    await expectControlStable(page.locator('#edit-modal-form [data-action="close-edit-modal"]'), 'edit modal cancel');
    await page.locator('#edit-modal-form [data-action="close-edit-modal"]').click();
    await expect(page.locator('#edit-modal')).toBeHidden();
  }

  await page.goto(threadUrl);
  const href = await page.locator('.post-controls .del-btn').first().getAttribute('href');
  expect(href).toBeTruthy();
  await page.goto(`${baseURL}${href}`);
  await expectSafePage(page);
  await expectControlStable(page.locator('.self-action-form button[type="submit"]'), 'delete confirmation submit');
  await expectControlStable(page.locator('.self-action-form .edit-btn'), 'delete confirmation cancel');
  await page.goto(`${baseURL}/${board}`);
}

async function expectReportFallbackControls(root: Locator, label: string): Promise<void> {
  const fallback = root.locator('.report-fallback-form').first();
  await expect(fallback).toBeVisible();
  await expectControlStable(fallback.locator('.report-fallback-summary'), `${label} report fallback summary`);
  await openDetailsIfClosed(fallback.locator('.report-fallback-details').first());
  await expectControlStable(fallback.locator('.report-fallback-reason'), `${label} report fallback reason`);
  await expectControlStable(fallback.locator('.report-fallback-submit'), `${label} report fallback submit`);
}

async function openDetailsIfClosed(details: Locator): Promise<void> {
  const open = await details.evaluate((element) => element instanceof HTMLDetailsElement && element.open);
  if (!open) {
    await details.locator('summary').click();
  }
}

async function expectControlStable(locator: Locator, name: string, options: { checkWrap?: boolean } = {}): Promise<void> {
  const box = await locator.boundingBox();
  expect(box, `${name} should be rendered`).not.toBeNull();
  expect(box!.width, `${name} should not collapse horizontally`).toBeGreaterThan(0);
  expect(box!.height, `${name} should not collapse vertically`).toBeGreaterThan(0);
  if (options.checkWrap !== false) {
    await expectNoAwkwardWrap(locator, name);
  }
}

async function expectNoAwkwardWrap(locator: Locator, name: string): Promise<void> {
  const wrapped = await locator.evaluate((element) => {
    if (element instanceof HTMLInputElement || element instanceof HTMLSelectElement) {
      return false;
    }
    const style = window.getComputedStyle(element);
    const lineHeight = Number.parseFloat(style.lineHeight);
    const fontSize = Number.parseFloat(style.fontSize);
    const usableLineHeight = Number.isFinite(lineHeight) ? lineHeight : fontSize * 1.2;
    return (
      element.scrollWidth > element.clientWidth + 1 ||
      element.scrollHeight > Math.ceil(usableLineHeight * 3.2)
    );
  });
  expect(wrapped, `${name} should not wrap into an oversized multi-line control`).toBeFalsy();
}

async function expectUsableActionHeight(locator: Locator, testInfo: TestInfo): Promise<void> {
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.height).toBeGreaterThanOrEqual(isMobile(testInfo) ? MOBILE_MIN_ACTION_HEIGHT : DESKTOP_MIN_ACTION_HEIGHT);
}

async function metrics(locator: Locator, name: string): Promise<ControlMetrics> {
  return locator.evaluate((element, metricName) => {
    const rect = element.getBoundingClientRect();
    const style = window.getComputedStyle(element);
    return {
      name: metricName,
      width: rect.width,
      height: rect.height,
      top: rect.top,
      bottom: rect.bottom,
      left: rect.left,
      right: rect.right,
      borderTopWidth: style.borderTopWidth,
      backgroundColor: style.backgroundColor,
      display: style.display,
    };
  }, name);
}

function expectSimilarHeight(actual: ControlMetrics, expected: ControlMetrics, tolerance: number): void {
  expect(Math.abs(actual.height - expected.height), `${actual.name} height should match ${expected.name}`).toBeLessThanOrEqual(tolerance);
}

function expectSameRow(actual: ControlMetrics, expected: ControlMetrics, tolerance: number): void {
  expect(Math.abs(actual.top - expected.top), `${actual.name} top should align with ${expected.name}`).toBeLessThanOrEqual(tolerance);
  expect(Math.abs(actual.bottom - expected.bottom), `${actual.name} bottom should align with ${expected.name}`).toBeLessThanOrEqual(tolerance);
}

function isMobile(testInfo: TestInfo): boolean {
  return testInfo.project.name.includes('mobile');
}

function isNoJs(testInfo: TestInfo): boolean {
  return testInfo.project.name === 'firefox-nojs';
}
