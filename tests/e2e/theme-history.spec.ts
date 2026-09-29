import { test, expect } from './helpers';
import { newAuditedContext } from './diagnostics';

test('history restoration revalidates theme state on pages without activity badges', async ({ page, app, javaScriptEnabled }) => {
  test.skip(!javaScriptEnabled, 'back-forward cache revalidation uses JavaScript');
  await page.goto(`${app.baseURL}/pub/search?q=theme`);
  await page.context().addCookies([{ name: 'rustchan_theme', value: 'blue-sky', url: app.baseURL }]);
  // Trigger the browser's persisted-page restoration contract directly, since
  // automation/browser versions differ in whether an actual entry uses BFCache.
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
});

test('a fresh browser context restores the cookie preference despite conflicting local storage', async ({ browser, page, app, javaScriptEnabled }) => {
  await page.goto(`${app.baseURL}/theme/blue-sky?return_to=/pub`);
  const state = await page.context().storageState();
  const fresh = await newAuditedContext(browser, { storageState: state, javaScriptEnabled });
  try {
    if (javaScriptEnabled) await fresh.addInitScript(origin => {
      if (location.origin === origin) localStorage.setItem('rustchan_theme', 'terminal');
    }, app.baseURL);
    const restored = await fresh.newPage();
    await restored.goto(`${app.baseURL}/pub`);
    await expect(restored.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
    await restored.goto(`${app.baseURL}/admin`);
    await expect(restored.locator('html')).toHaveAttribute('data-active-theme', 'blue-sky');
  } finally {
    await fresh.close();
  }
});
