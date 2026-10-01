import { test, expect, adminLogin, isNoJsProject, uniqueShort } from './helpers';

test('site-health polling stops at navigation start, before the next timer callback', async ({ page, app }, testInfo) => {
  test.skip(isNoJsProject(testInfo), 'polling requires JavaScript');
  await page.addInitScript(() => {
    // Retain real timers, but allow the test to run the next health deadline
    // exactly during beforeunload. Track clears as well so cancelled work does
    // not get resurrected. This works even while Chromium blocks protocol
    // evaluation behind a provisional navigation.
    const deadlines = new Map<number, () => void>();
    const timeout = window.setTimeout.bind(window);
    const interval = window.setInterval.bind(window);
    const clearTimeout = window.clearTimeout.bind(window);
    const clearInterval = window.clearInterval.bind(window);
    window.setTimeout = ((callback: TimerHandler, delay?: number, ...args: unknown[]) => {
      let id: number;
      id = timeout(() => {
        deadlines.delete(id);
        if (typeof callback === 'function') callback(...args);
      }, delay);
      if (delay === 5000 && typeof callback === 'function') deadlines.set(id, () => callback(...args));
      return id;
    }) as typeof window.setTimeout;
    window.setInterval = ((callback: TimerHandler, delay?: number, ...args: unknown[]) => {
      const id = interval(callback, delay, ...args);
      if (delay === 5000 && typeof callback === 'function') deadlines.set(id, () => callback(...args));
      return id;
    }) as typeof window.setInterval;
    window.clearTimeout = id => { deadlines.delete(id); clearTimeout(id); };
    window.clearInterval = id => { deadlines.delete(id); clearInterval(id); };
    (window as any).runHealthDeadline = () => {
      for (const callback of [...deadlines.values()]) callback();
    };
  });
  await adminLogin(page, app);
  await expect(page.locator('[data-admin-health-poll-status]')).toContainText('up to date');
  await page.evaluate(() => {
    window.name = '0';
    const originalFetch = window.fetch.bind(window);
    let navigating = false;
    window.fetch = (input, init) => {
      if (navigating && String(input).includes('/admin/site-health/jobs')) {
        window.name = String(Number(window.name) + 1);
      }
      return originalFetch(input, init);
    };
    // Registered after production lifecycle listeners: cancelled deadlines
    // must already have been removed before the old document can fetch again.
    window.addEventListener('beforeunload', () => {
      navigating = true;
      (window as any).runHealthDeadline();
    });
  });
  await page.locator('.admin-section-index a[href="#boards"]').click();
  const form = page.locator('form.admin-board-create-form');
  await form.locator('[name="short_name"]').fill(uniqueShort('nav', testInfo));
  await form.locator('[name="name"]').fill('Navigation lifecycle fixture');
  await Promise.all([
    page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/admin/board/create'),
    form.getByRole('button', { name: 'create' }).click(),
  ]);
  await expect(page.locator('[data-admin-health-poll-status]')).toContainText('up to date');
  expect(await page.evaluate(() => window.name), 'no polling from the unloading document').toBe('0');
});
