import { test, expect, createThread, sqliteQuery, isNoJsProject, } from './helpers';
import { expectNoHorizontalOverflow } from './phase4-helpers';

test.use({ javaScriptEnabled: false });

test('release: no-JS catalog actions remain inside cards and operate at desktop and phone widths', async ({ page, app }, testInfo) => {
  for (const subject of ['left catalog card', 'middle catalog card', 'right catalog card']) await createThread(page, app, 'pub', { subject, body: 'catalog action containment' });
  await page.goto(`${app.baseURL}/pub/catalog`);
  const card = page.locator('.catalog-item').filter({ hasText: 'left catalog card' });
  for (const width of [1280, 390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    await expectNoHorizontalOverflow(page, `no-JS catalog ${width}px`, 1);
    const outside = await page.locator('.catalog-item').evaluateAll(cards => cards.flatMap(card => {
      const box = card.getBoundingClientRect();
      return Array.from(card.querySelectorAll('button')).filter(button => {
        const rect = button.getBoundingClientRect();
        return rect.width > 0 && (rect.left < box.left || rect.right > box.right);
      }).map(button => button.textContent);
    }));
    expect(outside).toEqual([]);
    await card.getByRole('button', { name: 'Pin thread', exact: true }).click();
    await expect(card).toHaveAttribute('data-pinned', '1');
    await card.getByRole('button', { name: 'Unpin thread', exact: true }).click();
    await expect(card).toHaveAttribute('data-pinned', '0');
  }
  const report = card.locator('.report-fallback-form');
  await report.locator('summary').click();
  await report.getByLabel('reason').fill('narrow no-JS catalog report');
  await expectNoHorizontalOverflow(page, 'expanded no-JS report at 320px', 1);
  // Exercise native keyboard submission from the reason input as well as
  // the pointer-operated pin/hide controls above.
  await report.getByLabel('reason').press('Enter');
  await expect(page.locator('.post-success-banner')).toContainText(/report submitted/i);
  expect(sqliteQuery(app, 'SELECT COUNT(*) FROM reports;')).toBe('1');
  await page.goto(`${app.baseURL}/pub/catalog`);
  await card.getByRole('button', { name: 'Hide thread', exact: true }).click();
  await expect(card).toHaveCount(0);
  await page.goto(`${app.baseURL}/pub/hidden`);
  await card.getByRole('button', { name: 'Unhide thread', exact: true }).click();
  await expect(page).toHaveURL(/\/pub\/catalog$/);
  await expect(card).toBeVisible();
  if (isNoJsProject(testInfo)) await testInfo.attach('nojs-catalog-320.png', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' });
});
