import fs from 'node:fs/promises';
import path from 'node:path';
import {
  createReplyViaRequest,
  createThreadViaRequest,
  expect,
  isNoJsProject,
  test,
} from './helpers';
import {
  BUILTIN_THEMES,
  expectFocusVisible,
  expectNoHorizontalOverflow,
  expectReadableContrast,
} from './phase4-helpers';

const evidenceDir = path.resolve('output/playwright/public-polish');

async function capture(page: import('./diagnostics').Page, label: string): Promise<void> {
  await fs.mkdir(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, `${process.env.RUSTCHAN_POLISH_PHASE ?? 'before'}-${test.info().project.name}-${label}.png`),
    fullPage: !label.includes('preferences') && !label.includes('report'),
  });
}

test('public compact theme and viewport audit', async ({ app, page }, testInfo) => {
  test.setTimeout(180_000);
  const threadId = await createThreadViaRequest(page, app, 'pub', {
    subject: 'Unicode café 日本語 — long_subject_'.repeat(3),
    body: 'A text-only thread.\n>quoted text\n>>1\nhttps://example.test/' + 'long_path_'.repeat(18),
  });
  for (let index = 0; index < 8; index += 1) {
    await createReplyViaRequest(page, app, 'pub', threadId,
      `Reply ${index}: café 日本語 👋\n>>1\n>nested quote\n${'dense but compact content '.repeat(12)}`);
  }
  for (const [slug, label] of BUILTIN_THEMES) {
    await page.goto(`${app.baseURL}/theme/${slug}?return_to=/pub/thread/${threadId}`);
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', slug);
    for (const viewport of [{ width: 1280, height: 800 }, { width: 320, height: 720 }, { width: 667, height: 320 }]) {
      await page.setViewportSize(viewport);
      await expectNoHorizontalOverflow(page, `${label} thread ${viewport.width}`);
      await expectReadableContrast(page, '.post-body', `${label} post text`, 4.2);
      await expectReadableContrast(page, '.post-time', `${label} metadata`, 3);
      await capture(page, `${slug}-thread-${viewport.width}`);
    }
    if (!isNoJsProject(testInfo)) {
      await page.setViewportSize({ width: 320, height: 720 });
      await page.locator('.report-btn').first().click();
      await expect(page.locator('#report-reason')).toBeFocused();
      await expectReadableContrast(page, '#report-reason', `${label} dialog input`, 3);
      await expectReadableContrast(page, '#report-submit-btn', `${label} dialog action`, 3);
      await expectFocusVisible(page.locator('#report-reason'), `${label} dialog focus`);
      await capture(page, `${slug}-report-320`);
      await page.keyboard.press('Escape');
      await page.locator('.user-preferences-summary').click();
      await capture(page, `${slug}-preferences-320`);
      await page.keyboard.press('Escape');
    }
  }
  await page.setViewportSize({ width: 320, height: 720 });
  for (const [name, route] of [['home', '/'], ['board', '/pub'], ['catalog', '/pub/catalog'], ['search', '/pub/search?q=Unicode'], ['archive', '/pub/archive']] as const) {
    await page.goto(app.baseURL + route);
    await expectNoHorizontalOverflow(page, `${name} 320px`);
    await capture(page, name);
  }
  await page.locator('.user-preferences-summary').click();
  await capture(page, 'preferences-320');
  if (isNoJsProject(testInfo)) {
    const terminal = page.locator('.user-preferences-noscript button[name="theme"][value="terminal"]');
    await terminal.click();
    await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'terminal');
  }
});

test('mobile preferences initial focus and keyboard containment', async ({ app, page }, testInfo) => {
  test.skip(isNoJsProject(testInfo), 'no-JavaScript preferences remain native details forms');
  await page.setViewportSize({ width: 320, height: 720 });
  await page.goto(app.baseURL);
  const summary = page.locator('.user-preferences-summary');
  await summary.focus();
  await page.keyboard.press('Enter');
  const form = page.locator('.user-preferences-form');
  await expect(form).toBeVisible();
  await capture(page, 'preferences-keyboard');
  await expect(page.locator('.user-preferences-mobile-close')).toBeFocused();
  await expect(form).toHaveAttribute('role', 'dialog');
  await expect(form).toHaveAttribute('aria-modal', 'true');
  await page.locator('input[name="show_activity_badges"]').focus();
  await page.keyboard.press('Tab');
  await expect(page.locator('.user-preferences-mobile-close')).toBeFocused();
  await page.keyboard.press('Shift+Tab');
  await expect(page.locator('input[name="show_activity_badges"]')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(form).toBeHidden();
  await expect(summary).toBeFocused();
  await expectFocusVisible(summary, 'preferences restored focus');
  await capture(page, 'preferences-dismissed');
  await page.setViewportSize({ width: 667, height: 320 });
  await summary.click();
  const lastChoice = page.locator('input[name="show_activity_badges"]');
  await lastChoice.scrollIntoViewIfNeeded();
  await expect(lastChoice).toBeVisible();
  const bounds = await lastChoice.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(320);
  await capture(page, 'preferences-landscape');
  await page.keyboard.press('Escape');
  await expect(summary).toBeFocused();
  await page.setViewportSize({ width: 1280, height: 800 });
  await summary.click();
  await expect(form).not.toHaveAttribute('aria-modal', 'true');
  await page.setViewportSize({ width: 320, height: 720 });
  await expect(form).toHaveAttribute('aria-modal', 'true');
  await expect(page.locator('.user-preferences-mobile-close')).toBeFocused();
  await page.setViewportSize({ width: 1280, height: 800 });
  await expect(form).not.toHaveAttribute('aria-modal', 'true');
  await expect(summary).toBeFocused();
  await expect(page.locator('body')).not.toHaveClass(/user-preferences-mobile-open/);
  await page.keyboard.press('Escape');
});

test('report dialog stays reachable in short landscape and honors keyboard focus', async ({ app, page }, testInfo) => {
  test.skip(isNoJsProject(testInfo), 'no-JavaScript reports use inline native forms');
  const threadId = await createThreadViaRequest(page, app, 'pub');
  await page.setViewportSize({ width: 667, height: 320 });
  await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
  const trigger = page.locator('.report-btn').first();
  await trigger.click();
  await expect(page.locator('#report-reason')).toBeFocused();
  await capture(page, 'report-landscape');
  const submit = page.locator('#report-submit-btn');
  await submit.scrollIntoViewIfNeeded();
  const bounds = await submit.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.y).toBeGreaterThanOrEqual(0);
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(320);
  await submit.focus();
  await page.keyboard.press('Tab');
  await expect(page.locator('#report-reason')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(page.locator('#report-modal')).toBeHidden();
  await expect(trigger).toBeFocused();
});

test('reduced motion stops decorative blinking and posting scroll animation', async ({ app, page }, testInfo) => {
  test.skip(isNoJsProject(testInfo), 'scripted scrolling needs JavaScript');
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.setViewportSize({ width: 320, height: 720 });
  await page.goto(`${app.baseURL}/theme/terminal?return_to=/pub`);
  const animation = await page.locator('.site-header').evaluate((header) => getComputedStyle(header, '::before').animationName);
  expect(animation).toBe('none');
  await page.evaluate(() => {
    const original = Element.prototype.scrollIntoView;
    (window as unknown as { auditScrollBehavior: string[] }).auditScrollBehavior = [];
    Element.prototype.scrollIntoView = function(options) {
      if (options && typeof options === 'object') {
        (window as unknown as { auditScrollBehavior: string[] }).auditScrollBehavior.push(options.behavior ?? 'auto');
      }
      return original.call(this, options);
    };
  });
  await page.locator('[data-action="toggle-post-form"]').first().click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { auditScrollBehavior: string[] }).auditScrollBehavior.length)).toBeGreaterThan(0);
  expect(await page.evaluate(() => (window as unknown as { auditScrollBehavior: string[] }).auditScrollBehavior)).not.toContain('smooth');
});
