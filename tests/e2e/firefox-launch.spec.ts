import { test, expect } from '@playwright/test';
import { needsFirefoxIdentityIsolation, rebrandApplicationIni } from './firefox-rebrand';

test('Firefox accommodation is scoped to the affected OS and original identity', async ({}, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'configuration check runs once');
  const ini = '[App]\nVendor=Mozilla\nName=Firefox\nVersion=153.0\n';
  expect(needsFirefoxIdentityIsolation('darwin', '27.0.0', ini)).toBe(true);
  expect(needsFirefoxIdentityIsolation('darwin', '26.0.0', ini)).toBe(false);
  expect(needsFirefoxIdentityIsolation('linux', '27.0.0', ini)).toBe(false);
  const isolated = rebrandApplicationIni(ini);
  expect(isolated).toContain('Version=153.0');
  expect(needsFirefoxIdentityIsolation('darwin', '27.0.0', isolated)).toBe(false);
  expect(() => rebrandApplicationIni(isolated)).toThrow(/identity changed/);
});

test('configured browser launches and respects JavaScript mode', async ({ page }, testInfo) => {
  await page.goto('data:text/html,<script>document.documentElement.dataset.js="on"</script><p>launch probe</p>');
  await expect(page.getByText('launch probe')).toBeVisible();
  if (testInfo.project.use.javaScriptEnabled === false) {
    await expect(page.locator('html')).not.toHaveAttribute('data-js', 'on');
  } else {
    await expect(page.locator('html')).toHaveAttribute('data-js', 'on');
  }
});
