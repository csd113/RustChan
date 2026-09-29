import { test, expect, adminLogin, createThreadViaRequest, createReplyViaRequest, sqliteQuery } from './helpers';
import { expectNoHorizontalOverflow } from './phase4-helpers';

test.beforeEach(async ({ javaScriptEnabled }) => {
  test.skip(javaScriptEnabled, 'server-rendered enhancement boundaries');
});

test('no-JS thread refresh uses a normal link and does not offer automatic updates', async ({ page, app }) => {
  const id = await createThreadViaRequest(page, app, 'pub', { body: 'no-JS update baseline' });
  for (const width of [320, 360, 390, 430, 768, 1024, 1280, 1440]) {
    await page.setViewportSize({ width, height: 844 });
    await page.goto(`${app.baseURL}/pub/thread/${id}`);
    await expect(page.locator('[data-action="fetch-updates"]').first()).toBeHidden();
    await expect(page.locator('.autoupdate-label').first()).toBeHidden();
    await createReplyViaRequest(page, app, 'pub', id, `reply at ${width}px`);
    await page.getByRole('link', { name: '[ Update now ]', exact: true }).first().click();
    await expect(page.locator('.post.reply').last()).toContainText(`reply at ${width}px`);
    await expectNoHorizontalOverflow(page, `no-JS refresh ${width}px`);
  }
});

test('no-JS poll creation supports additional options without inert add buttons', async ({ page, app }, testInfo) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await page.goto(`${app.baseURL}/pub`);
  await page.locator('.poll-creator > summary').click();
  await expect(page.locator('.poll-add-btn')).toBeHidden();
  await page.getByText('More poll options', { exact: true }).click();
  await page.locator('textarea[name="body"]').fill('poll from no-JS UI');
  await page.locator('input[name="poll_question"]').fill('Choose a third option?');
  await page.getByRole('textbox', { name: 'poll option 1', exact: true }).fill('First');
  await page.getByRole('textbox', { name: 'poll option 2', exact: true }).fill('Second');
  await page.getByRole('textbox', { name: 'poll option 3', exact: true }).fill('Third');
  await expectNoHorizontalOverflow(page, 'no-JS extra poll options');
  await page.screenshot({ path: testInfo.outputPath('nojs-poll-320.png') });
  await page.getByRole('button', { name: /post thread/i }).click();
  await expect(page).toHaveURL(/\/pub\/thread\/\d+/);
  await expect(page.locator('.poll-container')).toContainText('Third');
});

test('no-JS poster IDs expose identity without advertising an unavailable highlight action', async ({ page, app }) => {
  const id = await createThreadViaRequest(page, app, 'pub', { body: 'poster identity' });
  await page.goto(`${app.baseURL}/pub/thread/${id}`);
  await expect(page.locator('.poster-id-btn').first()).toBeVisible();
  await expect(page.locator('.poster-id-btn').first()).toBeDisabled();
});

test('no-JS admin diagnostics and job counts do not advertise unavailable client actions', async ({ page, app }, testInfo) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await adminLogin(page, app);
  await page.goto(`${app.baseURL}/admin/panel?open=site-health#site-health`);
  const toggles = page.locator('[data-admin-health-toggle]');
  expect(await toggles.count()).toBeGreaterThan(0);
  for (const toggle of await toggles.all()) await expect(toggle).toBeDisabled();
  await page.locator('[data-admin-diagnostics] > summary').click();
  await expect(page.locator('[data-admin-diagnostics-copy]')).toBeHidden();
  await expect(page.locator('[data-admin-diagnostics-close]')).toBeHidden();
  await expect(page.locator('[data-admin-diagnostics-text]')).toBeVisible();
  await expectNoHorizontalOverflow(page, 'no-JS admin diagnostics');
  await page.screenshot({ path: testInfo.outputPath('nojs-diagnostics-320.png') });
  await page.locator('[data-admin-diagnostics] > summary').click();
  await expect(page.locator('[data-admin-diagnostics-text]')).toBeHidden();
});

test('no-JS admin IP reports submit through a validated native form', async ({ page, app }, testInfo) => {
  const id = await createThreadViaRequest(page, app, 'pub', { body: 'admin IP report target' });
  const hash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE thread_id=${id} LIMIT 1`);
  await page.setViewportSize({ width: 320, height: 844 });
  await adminLogin(page, app);
  await page.goto(`${app.baseURL}/admin/ip/${hash}`);
  const fallback = page.locator('form[action="/admin/ip/report"]').first();
  await expect(fallback).toBeVisible();
  await fallback.locator('input[name="reason"]').fill('No-JS admin report regression');
  await expectNoHorizontalOverflow(page, 'no-JS admin IP report');
  const section = page.locator('.admin-section');
  expect(await section.evaluate(el => el.scrollWidth - el.clientWidth), 'admin card content must fit without clipping').toBeLessThanOrEqual(1);
  const submit = fallback.getByRole('button', { name: 'report', exact: true });
  await expect(submit).toBeInViewport({ ratio: 1 });
  await page.screenshot({ path: testInfo.outputPath('nojs-ip-report-320.png') });
  await fallback.getByRole('button', { name: 'report', exact: true }).click();
  await expect(page).toHaveURL(/\/admin\//);
  expect(sqliteQuery(app, "SELECT COUNT(*) FROM reports WHERE reason LIKE 'No-JS admin report regression%'")).toBe('1');
});
