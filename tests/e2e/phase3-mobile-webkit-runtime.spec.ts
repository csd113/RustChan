import fsp from 'node:fs/promises';
import path from 'node:path';
import type { Page, TestInfo } from '@playwright/test';
import {
  ADMIN_PASSWORD,
  adminLogin,
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
import { expectNoHorizontalOverflow } from './phase4-helpers';

test.describe('phase 3 mobile WebKit runtime coverage', () => {
  test('login, protected unlock, posting, own-post controls, media viewer, admin settings, and long pages stay usable', async ({
    page,
    app,
  }, testInfo) => {
    test.skip(testInfo.project.name !== 'mobile-webkit', 'mobile WebKit is the focused mobile runtime');

    await page.setViewportSize({ width: 390, height: 844 });
    const board = await restartWithProtectedMobileBoard(app, testInfo);
    await unlockBoard(page, app, board, ADMIN_PASSWORD);

    const longImage = path.join(
      app.fixtureDir,
      `phase3-mobile-${'very-long-file-name-'.repeat(5)}asset.png`,
    );
    await fsp.copyFile(app.fixtures().tinyPng, longImage);

    const threadId = await createThread(page, app, board, {
      subject: `phase 3 mobile ${'title '.repeat(18)}`,
      body: `mobile long post ${'longunbrokenword'.repeat(18)}\n${'second line '.repeat(28)}`,
      filePath: longImage,
    });
    await expectSafePage(page);
    await expect(page.locator('.self-action-controls .edit-btn').first()).toBeVisible();
    await expect(page.locator('.self-action-controls .del-btn').first()).toBeVisible();
    await expect(page.locator('.image-preview').first()).toBeVisible();
    await expectNoHorizontalOverflow(page, 'mobile owned thread');

    await page.locator('.image-preview').first().click();
    await expect(page.locator('.media-expanded-image').first()).toBeVisible();
    await expect(page.locator('.media-close-btn').first()).toBeVisible();
    await page.locator('.media-close-btn').first().click();
    await expect(page.locator('.image-preview').first()).toBeVisible();

    for (let i = 0; i < 10; i += 1) {
      await createReply(
        page,
        app,
        board,
        threadId,
        `mobile reply ${i} ${'replylongword'.repeat(14)}`,
      );
    }
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}#bottom`);
    await expect(page.locator('[data-role="thread-reply-count"]').first()).toHaveText('10');
    await expect(page.locator('.thread-nav-bottom')).toBeVisible();
    await expectNoHorizontalOverflow(page, 'mobile long thread');
    await expectReachable(page, '.thread-nav-bottom a[href="#top"]');
    await expectReachable(page, '[data-action="toggle-post-form"]');

    await page.goto(`${app.baseURL}/admin`);
    await expectNoHorizontalOverflow(page, 'mobile admin login');
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel#site-settings`);
    const siteForm = page.locator('form[action="/admin/site/settings"]').first();
    await expect(siteForm.locator('input[name="site_name"]')).toBeVisible();
    await siteForm.locator('input[name="site_name"]').fill('Phase 3 Mobile WebKit');
    await expectReachable(page, 'form[action="/admin/site/settings"] button[type="submit"]');
    await expectNoHorizontalOverflow(page, 'mobile admin settings');
  });
});

async function restartWithProtectedMobileBoard(app: RustChanServer, testInfo: TestInfo): Promise<string> {
  const board = uniqueShort('mw', testInfo);
  await app.stop();
  app.createBoardCli({
    short: board,
    name: `Phase 3 Mobile ${'Board '.repeat(8)}${board}`,
    description: `Narrow layout protected board ${'description '.repeat(12)}`,
  });
  setBoardFixtureSettings(app, board, {
    accessMode: 'view_password',
    accessPasswordHash: adminPasswordHash(app),
    allowEditing: true,
    allowSelfDelete: true,
    postCooldownSecs: 0,
  });
  await app.start();
  return board;
}

async function expectReachable(page: Page, selector: string): Promise<void> {
  const locator = page.locator(selector).first();
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, `${selector} should have a bounding box`).toBeTruthy();
  const viewport = page.viewportSize();
  expect(viewport, 'viewport should be configured').toBeTruthy();
  expect(box!.x + box!.width).toBeGreaterThan(0);
  expect(box!.x).toBeLessThan(viewport!.width);
  expect(box!.y + box!.height).toBeGreaterThan(0);
}
