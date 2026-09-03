import type { Locator, Page, TestInfo } from '@playwright/test';
import {
  adminLogin,
  expect,
  expectSafePage,
  RustChanServer,
  sqliteExec,
  sqliteQuery,
  test,
} from './helpers';
import { BUILTIN_THEMES, expectReadableContrast } from './phase4-helpers';

test.describe('admin dashboard control center', () => {
  test('desktop dashboard prioritizes current issues and task-oriented workflows', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'dashboard desktop coverage runs on Chromium first');

    let liveLogRequests = 0;
    page.on('request', (request) => {
      if (new URL(request.url()).pathname === '/admin/log/live') liveLogRequests += 1;
    });
    const reason = seedOpenReport(app, testInfo);
    await adminLogin(page, app);
    await page.waitForTimeout(250);
    expect(liveLogRequests, 'a closed live-log disclosure should not poll').toBe(0);
    await page.goto(`${app.baseURL}/admin/panel?open=control-center#control-center`);
    await expectTargetBelowStickyHeader(page, '#control-center', 'direct Control Center navigation');

    const controlCenter = page.locator('#control-center');
    const disclosure = controlCenter.locator('details.admin-dropdown').first();
    await expect(disclosure).toHaveAttribute('open', '');
    await expect(page.getByRole('heading', { level: 2, name: /control center/i })).toBeVisible();

    for (const heading of [
      'site overview and health',
      'moderation and recent activity',
      'backups and recovery',
      'maintenance and background jobs',
      'network and Tor',
      'configuration shortcuts',
    ]) {
      await expect(controlCenter.getByRole('heading', { level: 3, name: new RegExp(heading, 'i') })).toBeVisible();
    }
    await expect(controlCenter.locator('.admin-control-group')).toHaveCount(6);

    await expect(dashboardStatus(page, 'jobs')).toHaveAttribute('data-dashboard-state', 'ok');
    await expect(dashboardStatus(page, 'jobs')).toContainText(/idle.*ready/i);
    await expect(dashboardStatus(page, 'tor')).toHaveAttribute('data-dashboard-state', 'disabled');
    await expect(dashboardStatus(page, 'media-tools')).toHaveAttribute('data-dashboard-state', 'informational');
    await expect(dashboardStatus(page, 'reports')).toHaveAttribute('data-dashboard-state', 'action-needed');
    await expect(controlCenter.locator('[data-dashboard-alert="reports"]')).toBeVisible();
    await expect(controlCenter.locator('[data-dashboard-alert="backups"]')).toBeVisible();
    await expect(controlCenter.locator('a[href="/admin/panel?open=full-backup-restore#full-backup-restore"]')).toHaveCount(1);
    await expect(page.locator('#reports')).toContainText(reason);

    await expect(controlCenter.locator('form[action="/admin/setup/reopen"]')).toHaveCount(0);
    await expect(page.locator('#database-maintenance form[action="/admin/setup/reopen"] input[name="_csrf"]')).toHaveCount(1);
    await expect(controlCenter.locator('a[href="/admin/panel?open=boards#boards"]').first()).toBeVisible();
    await expect(controlCenter.locator('a[href="/admin/panel?open=site-health#site-health"]').first()).toBeVisible();
    await expect(controlCenter.locator('a[href="/admin/mod-log"]').first()).toBeVisible();

    const systemDetails = controlCenter.locator('.admin-control-system-details');
    await systemDetails.locator(':scope > summary').focus();
    await page.keyboard.press('Enter');
    await expect(systemDetails).toHaveAttribute('open', '');
    await expect(systemDetails).toContainText(/database|media totals|diagnostics/i);

    await controlCenter.locator('a[href="/admin/panel?open=full-backup-restore#full-backup-restore"]').first().click();
    await expect(page.locator('#full-backup-restore > details')).toHaveAttribute('open', '');
    await expect(page).toHaveURL(/open=full-backup-restore#full-backup-restore$/);
    await expectTargetBelowStickyHeader(page, '#full-backup-restore', 'enhanced backup navigation');

    await controlCenter.locator('a[href="/admin/panel?open=live-log#live-log"]').first().click();
    await expect(page.locator('#live-log > details')).toHaveAttribute('open', '');
    await expect.poll(() => liveLogRequests).toBeGreaterThan(0);
    await page.locator('#live-log > details > summary').click();
    const requestsAfterClose = liveLogRequests;
    await page.waitForTimeout(2200);
    expect(liveLogRequests, 'closing live-log should stop polling').toBe(requestsAfterClose);
    await expectNoHorizontalOverflow(page, 'desktop dashboard');
    await expectSafePage(page, { allowAdminInternals: true });
  });

  test('mobile dashboard is visible, tappable, and contained at 390px and 320px', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'mobile-firefox', 'mobile dashboard coverage runs on mobile Firefox');

    await adminLogin(page, app);
    await expect(page.locator('#control-center > details')).toHaveAttribute('open', '');
    await expect(page.locator('#control-center .admin-control-group')).toHaveCount(6);
    await expect(page.locator('#control-center .admin-control-group').first()).toBeVisible();

    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 844 });
      await expectNoHorizontalOverflow(page, `${width}px dashboard`);
      const escaped = await page.locator('#control-center .admin-control-group').evaluateAll((groups) => {
        const viewportWidth = document.documentElement.clientWidth;
        return groups
          .map((group) => group.getBoundingClientRect())
          .filter((rect) => rect.left < -1 || rect.right > viewportWidth + 1)
          .map((rect) => ({ left: rect.left, right: rect.right, viewportWidth }));
      });
      expect(escaped, `${width}px task groups should stay within the viewport`).toEqual([]);
    }

    const actionHeights = await page.locator('#control-center .admin-control-action:visible').evaluateAll((actions) => (
      actions.map((action) => action.getBoundingClientRect().height)
    ));
    expect(actionHeights.length).toBeGreaterThan(0);
    expect(Math.min(...actionHeights), 'mobile Control Center actions should be at least 42px tall').toBeGreaterThanOrEqual(42);
  });

  test('no-JS dashboard opens by default and its links reveal protected destinations', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'firefox-nojs', 'no-JS dashboard coverage runs on the no-JS project');

    await adminLogin(page, app);
    await expect(page.locator('#control-center > details')).toHaveAttribute('open', '');

    const systemDetails = page.locator('#control-center .admin-control-system-details');
    await systemDetails.locator(':scope > summary').click();
    await expect(systemDetails).toHaveAttribute('open', '');

    await page.locator('#control-center a[href="/admin/panel?open=full-backup-restore#full-backup-restore"]').first().click();
    await expect(page).toHaveURL(/open=full-backup-restore#full-backup-restore$/);
    await expect(page.locator('#full-backup-restore > details')).toHaveAttribute('open', '');
    await expectTargetBelowStickyHeader(page, '#full-backup-restore', 'no-JS backup navigation');

    await page.goto(`${app.baseURL}/admin/panel`);
    await page.locator('#control-center a[href="/admin/panel?open=database-maintenance#database-maintenance"]').last().click();
    await expect(page.locator('#database-maintenance > details')).toHaveAttribute('open', '');
    await expect(page.locator('#database-maintenance form[action="/admin/setup/reopen"] input[name="_csrf"]')).toHaveCount(1);

    await page.goto(`${app.baseURL}/admin/panel`);
    await page.locator('#control-center a[href="/admin/panel?open=theme-catalog#theme-catalog"]').click();
    await expect(page.locator('#theme-catalog > details')).toHaveAttribute('open', '');

    await page.goto(`${app.baseURL}/admin/panel?open=live-log#live-log`);
    await expect(page.locator('#live-log > details')).toHaveAttribute('open', '');
    await expect(page.locator('#live-log a[href="/admin/log/live?bytes=65536"]')).toBeVisible();
    await expect(page.locator('[data-admin-live-log-controls]')).toBeHidden();
    await expect(page.locator('#admin-live-log-output')).toContainText(/JavaScript is available/i);

    await page.goto(`${app.baseURL}/admin/panel?open=site-health#site-health`);
    await page.locator('[data-admin-diagnostics] > summary').click();
    await expect(page.locator('[data-admin-diagnostics-text]')).toContainText(/RustChan version/i);
    await expect(page.locator('[data-admin-diagnostics-text]')).not.toContainText(/\/Users\//);
    await expectNoHorizontalOverflow(page, 'no-JS dashboard');
  });

  test('state pills retain readable text contrast across built-in themes', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'theme contrast coverage runs on Chromium first');

    await adminLogin(page, app);
    for (const [slug, label] of BUILTIN_THEMES) {
      await page.goto(`${app.baseURL}/theme/${slug}?return_to=/admin/panel`);
      await page.waitForURL(`${app.baseURL}/admin/panel`);
      await expect(page.locator('#control-center > details')).toHaveAttribute('open', '');
      for (const state of ['ok', 'informational', 'action-needed', 'disabled', 'unknown']) {
        const selector = `#control-center .admin-state-pill-${state}:visible`;
        if (await page.locator(selector).count()) {
          await expectReadableContrast(page, selector, `${label} ${state} status`, 4.5);
        }
      }
    }
  });

  test('unauthenticated admin panel requests do not render the dashboard', async ({ browser, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'unauthenticated dashboard coverage runs on Chromium first');

    const context = await browser.newContext();
    const page = await context.newPage();
    try {
      await page.goto(`${app.baseURL}/admin/panel`);
      await expect(page.locator('#control-center')).toHaveCount(0);
      await expect(page.locator('body')).toContainText(/not logged in|forbidden|admin login/i);
    } finally {
      await context.close();
    }
  });
});

function dashboardStatus(page: Page, key: string): Locator {
  return page.locator(`#control-center [data-dashboard-status="${key}"]`).first();
}

function seedOpenReport(app: RustChanServer, testInfo: TestInfo): string {
  const boardId = Number(sqliteQuery(app, "SELECT id FROM boards WHERE short_name = 'pub' LIMIT 1;"));
  const stamp = `${Date.now()}-${testInfo.workerIndex}-${testInfo.repeatEachIndex}`;
  const reason = `dashboard report ${stamp}`;
  const now = Math.floor(Date.now() / 1000);
  sqliteExec(app, [
    `INSERT INTO threads (board_id, subject, created_at, bumped_at) VALUES (${boardId}, 'dashboard report', ${now}, ${now});`,
    `INSERT INTO posts (thread_id, board_id, body, body_html, deletion_token, is_op, created_at) VALUES (last_insert_rowid(), ${boardId}, 'reported dashboard post', 'reported dashboard post', 'dash-${stamp}', 1, ${now});`,
    `INSERT INTO reports (post_id, thread_id, board_id, reason, reporter_hash, created_at) VALUES (last_insert_rowid(), (SELECT thread_id FROM posts WHERE id = last_insert_rowid()), ${boardId}, ${sqlString(reason)}, 'dashboard-reporter', ${now});`,
  ].join('\n'));
  return reason;
}

function sqlString(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
}

async function expectNoHorizontalOverflow(page: Page, label: string): Promise<void> {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${label} should not overflow horizontally`).toBeLessThanOrEqual(1);
}

async function expectTargetBelowStickyHeader(page: Page, selector: string, label: string): Promise<void> {
  const positions = await page.locator(selector).evaluate((target) => ({
    headerBottom: document.querySelector('.site-header')?.getBoundingClientRect().bottom ?? 0,
    targetTop: target.getBoundingClientRect().top,
  }));
  expect(positions.targetTop, `${label} should clear the sticky header`).toBeGreaterThanOrEqual(
    positions.headerBottom - 1,
  );
}
