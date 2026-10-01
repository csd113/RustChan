import fs from 'node:fs/promises';
import path from 'node:path';
import {
  createThreadViaRequest,
  createReplyViaRequest,
  expect,
  isNoJsProject,
  setThreadFixtureState,
  sqliteExec,
  sqliteQuery,
  test,
} from './helpers';

test('catalog last reply includes sage replies without changing bump order', async ({ page, app }, testInfo) => {
  test.skip(isNoJsProject(testInfo), 'Catalog sorting is an existing JavaScript enhancement with a native bump-order fallback');
  const old = await createThreadViaRequest(page, app, 'pub', { subject: 'Older discussion with sage reply' });
  const recent = await createThreadViaRequest(page, app, 'pub', { subject: 'Recently bumped discussion' });
  const now = Math.floor(Date.now() / 1000);
  setThreadFixtureState(app, old, { createdAt: now - 20, bumpedAt: now - 20 });
  setThreadFixtureState(app, recent, { createdAt: now - 10, bumpedAt: now - 10 });
  sqliteExec(app, `UPDATE posts SET created_at = ${now - 20} WHERE thread_id = ${old}; UPDATE posts SET created_at = ${now - 10} WHERE thread_id = ${recent};`);
  await page.goto(`${app.baseURL}/pub/thread/${old}`);
  await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator(`form[action="/pub/thread/${old}"]`);
  await form.locator('textarea[name="body"]').fill('Latest reply deliberately uses sage');
  await form.locator('input[name="sage"]').check();
  await form.locator('button[type="submit"]').click();
  await expect.poll(() => Number(sqliteQuery(app, `SELECT reply_count FROM threads WHERE id = ${old};`))).toBe(1);
  expect(Number(sqliteQuery(app, `SELECT bumped_at FROM threads WHERE id = ${old};`))).toBe(now - 20);
  await page.goto(`${app.baseURL}/pub/catalog`);
  const firstLink = page.locator('#catalog-grid .catalog-item').first().locator('.catalog-card-link').first();
  await page.locator('#catalog-sort').selectOption('bump');
  await expect(firstLink).toHaveAttribute('href', `/pub/thread/${recent}`);
  await page.locator('#catalog-sort').selectOption('last_reply');
  const directory = path.resolve(process.env.RUSTCHAN_AUDIT_OUTPUT ?? 'output/playwright/public-polish/catalog', 'screenshots');
  await fs.mkdir(directory, { recursive: true });
  await page.screenshot({ path: path.join(directory, `${testInfo.project.name}-catalog-last-reply.png`), fullPage: true });
  await expect(firstLink).toHaveAttribute('href', `/pub/thread/${old}`);
  await page.reload();
  await expect(page.locator('#catalog-sort')).toHaveValue('last_reply');
  await expect(firstLink).toHaveAttribute('href', `/pub/thread/${old}`);
  await page.locator('#catalog-sort').selectOption('replies');
  await expect(firstLink).toHaveAttribute('href', `/pub/thread/${old}`);
  await page.locator('#catalog-sort').selectOption('created');
  await expect(firstLink).toHaveAttribute('href', `/pub/thread/${recent}`);
});

test('search pagination preserves a Unicode query and stable results across browser Back', async ({ page, app }, testInfo) => {
  const query = 'café 漢字';
  const thread = await createThreadViaRequest(page, app, 'pub', { body: `${query} page marker 0` });
  for (let index = 1; index < 23; index += 1) {
    await createReplyViaRequest(page, app, 'pub', thread, `${query} page marker ${index}`);
  }
  await page.goto(`${app.baseURL}/pub/search`);
  await page.locator('#board-search-input').fill(query);
  await page.locator('.board-search-form button[type="submit"]').click();
  await expect(page.locator('.post')).toHaveCount(20);
  await expect(page.locator('.board-search-summary-results')).toHaveText('23 results');
  const firstPageIds = await page.locator('.post').evaluateAll(elements => elements.map(element => element.id));
  const expectedIds = sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${thread} ORDER BY created_at DESC, id DESC;`).split('\n').map(id => `p${id}`);
  expect(firstPageIds).toEqual(expectedIds.slice(0, 20));
  await page.locator('.pagination a').filter({ hasText: '[next]' }).click();
  await expect(page.locator('#board-search-input')).toHaveValue(query);
  const secondURL = new URL(page.url());
  expect(secondURL.searchParams.get('q')).toBe(query);
  expect(secondURL.searchParams.get('page')).toBe('2');
  await expect(page.locator('.post')).toHaveCount(3);
  const secondPageIds = await page.locator('.post').evaluateAll(elements => elements.map(element => element.id));
  expect(secondPageIds).toEqual(expectedIds.slice(20));
  expect(new Set([...firstPageIds, ...secondPageIds]).size).toBe(23);
  await page.locator('.post-num').first().click();
  await expect(page).toHaveURL(new RegExp(`/pub/thread/${thread}#${secondPageIds[0]}$`));
  await page.goBack();
  await expect(page.locator('#board-search-input')).toHaveValue(query);
  expect(new URL(page.url()).searchParams.get('page')).toBe('2');
  await expect(page.locator('.post')).toHaveCount(3);
  const directory = path.resolve(process.env.RUSTCHAN_AUDIT_OUTPUT ?? 'output/playwright/public-polish/catalog', 'screenshots');
  await fs.mkdir(directory, { recursive: true });
  await page.screenshot({ path: path.join(directory, `${testInfo.project.name}-search-page-restored.png`), fullPage: true });
});
