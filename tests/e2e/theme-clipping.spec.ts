import { test, expect, adminLogin } from './helpers';

test('custom theme creation and editing retain native submit buttons outside the decorative preview', async ({ page, app, javaScriptEnabled }) => {
  await adminLogin(page, app);
  await page.goto(`${app.baseURL}/admin/panel?open=theme-catalog#theme-catalog`);
  await page.locator('.theme-workbench-dropdown > summary').click();
  const form = page.locator('form[action="/admin/theme/create"]');
  const submit = page.getByRole('button', { name: 'create theme', exact: true });
  await expect(submit).toBeVisible();
  expect(await submit.evaluate((button: HTMLButtonElement) => button.form?.getAttribute('action'))).toBe('/admin/theme/create');
  await form.locator('input[name="display_name"]').fill('Browser created theme');
  await form.locator('input[name="slug"]').fill('browsercreated');
  await submit.click();
  const card = page.locator('#theme-browsercreated');
  await expect(card).toBeVisible();
  await expect(card.locator('summary').first()).toContainText('Browser created theme');
  if (!(await card.evaluate((el: HTMLDetailsElement) => el.open))) await card.locator('summary').first().click();
  const edit = card.locator('form[action="/admin/theme/update"]');
  const save = edit.getByRole('button', { name: 'save theme settings', exact: true });
  expect(await save.evaluate((button: HTMLButtonElement) => button.form?.getAttribute('action'))).toBe('/admin/theme/update');
  await edit.locator('input[name="display_name"]').fill('Browser edited theme');
  await save.click();
  await expect(card.locator('summary').first()).toContainText('Browser edited theme');
  if (!(await card.evaluate((el: HTMLDetailsElement) => el.open))) await card.locator('summary').first().click();
  await card.getByRole('button', { name: 'delete theme', exact: true }).click();
  if (javaScriptEnabled) await page.locator('#confirm-modal-continue').click();
  await expect(card).toHaveCount(0);
});

test('theme workshop controls and preview fit inside narrow admin cards', async ({ page, app }, testInfo) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await adminLogin(page, app);
  await page.goto(`${app.baseURL}/admin/panel?open=theme-catalog#theme-catalog`);
  const catalog = page.locator('#theme-catalog');
  for (const details of await catalog.locator('details').all()) {
    if (!(await details.evaluate((el: HTMLDetailsElement) => el.open))) await details.locator('summary').first().click();
  }
  for (const width of [320, 360, 390, 430, 768, 1024, 1280, 1440]) {
    await page.setViewportSize({ width, height: 844 });
    const clipping = await catalog.evaluate(section => {
      const rect = section.getBoundingClientRect();
      return { overflow: section.scrollWidth - section.clientWidth, elements: Array.from(section.querySelectorAll('*'))
        .filter(el => el.getBoundingClientRect().width > 0 && el.getBoundingClientRect().right > rect.right)
        .map(el => ({ tag: el.tagName, class: el.className, name: el.getAttribute('name'), width: el.getBoundingClientRect().width })) };
    });
    if (width === 320) {
      await catalog.locator('.theme-builder-preview-card').scrollIntoViewIfNeeded();
      await page.screenshot({ path: testInfo.outputPath('theme-preview-320.png') });
    }
    expect(clipping.overflow, JSON.stringify(clipping)).toBeLessThanOrEqual(1);
  }
});
