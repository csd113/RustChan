import { test, expect, adminLogin, sqliteExec } from './helpers';
import { BUILTIN_THEMES } from './phase4-helpers';

const sections = ['control-center', 'site-settings', 'site-health', 'network-security', 'https', 'tor', 'access', 'timeouts', 'display', 'media', 'schedules', 'accounts', 'storage', 'logging', 'system', 'configuration-state', 'boards', 'moderation', 'appearance', 'backups', 'maintenance'];

for (const [theme] of BUILTIN_THEMES) {
test(`${theme}: 320px admin sections preserve native tables and keyboard-accessible local scrolling`, async ({ page, app }, testInfo) => {
  test.skip(theme !== 'forest' && testInfo.project.name !== 'chromium', 'all-theme coverage runs on Chromium');
  const now = Math.floor(Date.now() / 1000);
  sqliteExec(app, `
    INSERT INTO threads (board_id, subject, created_at, bumped_at) VALUES ((SELECT id FROM boards WHERE short_name='pub'), 'Wide table fixture', ${now}, ${now});
    INSERT INTO posts (thread_id, board_id, body, body_html, deletion_token, is_op, created_at) VALUES (last_insert_rowid(), (SELECT id FROM boards WHERE short_name='pub'), 'Long preview for the moderation table', 'Long preview for the moderation table', 'table-fixture', 1, ${now});
    INSERT INTO reports (post_id, thread_id, board_id, reason, reporter_hash, created_at) VALUES (last_insert_rowid(), (SELECT thread_id FROM posts WHERE id=last_insert_rowid()), (SELECT id FROM boards WHERE short_name='pub'), 'A sufficiently long report reason to exercise action columns', 'table-reporter', ${now});
  `);
  await page.setViewportSize({ width: 320, height: 844 });
  await adminLogin(page, app);
    await page.goto(`${app.baseURL}/theme/${theme}?return_to=/admin/panel`);
    for (const section of sections) {
      await page.goto(`${app.baseURL}/admin/panel?open=${section}#${section}`);
      const target = page.locator(`#${section}`);
      await expect(target).toBeVisible();
      expect(await page.evaluate(() => Math.max(
        document.documentElement.scrollWidth - document.documentElement.clientWidth,
        document.body.scrollWidth - document.body.clientWidth,
      )), `${theme}/${section}: page-local overflow`).toBeLessThanOrEqual(2);
      const wrappers = target.locator('.admin-table-wrap:visible');
      for (let i = 0; i < await wrappers.count(); i += 1) {
        const wrapper = wrappers.nth(i);
        const scrolls = await wrapper.evaluate(element => element.scrollWidth > element.clientWidth + 2);
        if (!scrolls) continue;
        await expect(wrapper).toHaveAttribute('tabindex', '0');
        await expect(wrapper).toHaveAttribute('role', 'region');
        await expect(wrapper).toHaveAttribute('aria-label', /\S/);
        const semantics = await wrapper.locator('table').evaluate(table => ({
          display: getComputedStyle(table).display,
          headers: table.querySelectorAll('th').length,
          sticky: [...table.querySelectorAll('th,td')].some(cell => getComputedStyle(cell).position === 'sticky'),
          touch: getComputedStyle(table.parentElement!).touchAction,
        }));
        expect(semantics.display).toBe('table');
        expect(semantics.headers).toBeGreaterThan(0);
        expect(semantics.sticky, 'no sticky cells overlap content').toBe(false);
        expect(semantics.touch, 'native touch panning is enabled').not.toBe('none');
        await wrapper.focus();
        await expect(wrapper).toBeFocused();
        const outline = await wrapper.evaluate(element => getComputedStyle(element).outlineWidth);
        expect(parseFloat(outline), 'scroll region has visible focus').toBeGreaterThanOrEqual(2);
        await page.keyboard.press('ArrowRight');
        await expect.poll(() => wrapper.evaluate(element => element.scrollLeft)).toBeGreaterThan(0);
        // Reach the far columns without moving the whole document.
        await wrapper.evaluate(element => { element.scrollLeft = element.scrollWidth; });
        const actions = wrapper.locator('button, a').filter({ visible: true });
        for (let j = 0; j < await actions.count(); j += 1) {
          await actions.nth(j).focus();
          await expect(actions.nth(j)).toBeFocused();
          const inside = await actions.nth(j).evaluate(element => {
            const box = element.getBoundingClientRect();
            const parent = element.closest('.admin-table-wrap')!.getBoundingClientRect();
            return box.left >= parent.left - 1 && box.right <= parent.right + 1;
          });
          expect(inside, 'focusing table actions scrolls them into reach').toBe(true);
        }
      }
    }
    await page.goto(`${app.baseURL}/admin/mod-log`);
    await expect(page.locator('.admin-table-wrap')).toHaveAttribute('aria-label', 'Moderation log');
});
}
