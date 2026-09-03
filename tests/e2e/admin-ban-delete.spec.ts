import type { Locator, Page, TestInfo } from '@playwright/test';
import {
  adminLogin,
  createReply,
  createThread,
  expect,
  expectSafePage,
  sqliteQuery,
  test,
  uniqueShort,
} from './helpers';

const DESKTOP_MIN_TARGET = 30;
const MOBILE_MIN_TARGET = 38;

test.describe('admin ban+delete confirmation flow', () => {
  test('styled modal handles cancel, validation, focus, and safe reply submission', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name === 'firefox-nojs', 'modal flow requires JavaScript');

    const board = uniqueShort('bnd', testInfo);
    app.createBoardCli({ short: board, name: 'Ban Delete Modal', description: 'ban delete fixture' });
    const dialogs: string[] = [];
    page.on('dialog', async (dialog) => {
      dialogs.push(dialog.message());
      await dialog.dismiss();
    });

    const threadId = await createThread(page, app, board, {
      subject: 'ban delete modal',
      body: 'OP stays in place while a reply is deleted',
    });
    await createReply(page, app, board, threadId, 'reply selected for ban and delete');
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    await expectSafePage(page, { allowAdminInternals: true });

    const reply = page.locator('.post.reply').first();
    const banForm = reply.locator('form[data-ban-delete-pid]').first();
    const banButton = banForm.getByRole('button', { name: /ban\+del/i });
    const postId = Number(await banForm.getAttribute('data-ban-delete-pid'));
    const ipHash = await banForm.locator('input[name="ip_hash"]').inputValue();
    expect(Number.isInteger(postId), 'reply post id should be available').toBe(true);
    expect(ipHash, 'reply IP hash should be available').toMatch(/^[0-9a-f]{64}$/i);

    await banButton.focus();
    await banButton.press('Enter');
    const modal = page.locator('#ban-delete-modal');
    await expect(modal).toBeVisible();
    await expect(page.getByRole('dialog', { name: /ban ip \+ delete post/i })).toBeVisible();
    expect(dialogs, 'ban+delete should not open browser prompt dialogs').toEqual([]);

    const reason = page.getByLabel('Ban reason');
    const duration = page.getByLabel('Duration in hours');
    const cancel = page.locator('#ban-delete-cancel');
    const submit = page.locator('#ban-delete-submit');
    await expect(reason).toBeFocused();
    await expect(page.locator('#ban-delete-post-label')).toHaveText(`No.${postId}`);
    await expectUsableTarget(reason, 'ban reason input', testInfo);
    await expectUsableTarget(duration, 'ban duration input', testInfo);
    await expectUsableTarget(cancel, 'ban delete cancel', testInfo);
    await expectUsableTarget(submit, 'ban delete destructive submit', testInfo);
    await expect(submit, 'submit should carry destructive styling').toHaveClass(/btn-danger/);
    await expectNoCoveredCenters(page.locator('#ban-delete-modal input, #ban-delete-modal button'), 'ban delete modal controls');
    await expectNoHorizontalOverflow(page, 'ban delete modal');

    await cancel.click();
    await expect(modal).toBeHidden();
    await expect(reply).toBeVisible();
    await expect(banButton).toBeFocused();

    await banButton.click();
    await expect(modal).toBeVisible();
    await duration.fill('-1');
    await submit.click();
    await expect(page.locator('#ban-delete-error')).toContainText(/duration must be 0 or a positive number/i);
    await expect(duration).toBeFocused();

    await reason.fill('modal ban delete reason');
    await duration.fill('2');
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}#p${postId}`)),
      submit.click(),
    ]);
    await expect(page.locator(`#p${postId}`)).toHaveCount(0);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE id = ${postId};`)).toBe('0');
    expect(sqliteQuery(app, `SELECT reason FROM bans WHERE ip_hash = '${ipHash}' ORDER BY id DESC LIMIT 1;`)).toBe('modal ban delete reason');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM bans WHERE ip_hash = '${ipHash}' AND expires_at IS NOT NULL;`)).toBe('1');
  });

  test('no-JS fallback still submits the original form with default moderation fields', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'firefox-nojs', 'no-JS fallback is covered by the Firefox no-JS project');

    const board = uniqueShort('bnj', testInfo);
    app.createBoardCli({ short: board, name: 'Ban Delete No JS', description: 'no js ban delete fixture' });
    const threadId = await createThreadNoJs(page, app.baseURL, board, {
      subject: 'ban delete no js',
      body: 'OP remains after reply deletion',
    });
    await createReplyNoJs(page, app.baseURL, board, threadId, 'reply selected for no-js ban delete');
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);

    const reply = page.locator('.post.reply').first();
    const banForm = reply.locator('form[data-ban-delete-pid]').first();
    const postId = Number(await banForm.getAttribute('data-ban-delete-pid'));
    const ipHash = await banForm.locator('input[name="ip_hash"]').inputValue();

    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}#p${postId}`)),
      banForm.getByRole('button', { name: /ban\+del/i }).click(),
    ]);
    await expectSafePage(page, { allowAdminInternals: true });
    await expect(page.locator(`#p${postId}`)).toHaveCount(0);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE id = ${postId};`)).toBe('0');
    expect(sqliteQuery(app, `SELECT reason FROM bans WHERE ip_hash = '${ipHash}' ORDER BY id DESC LIMIT 1;`)).toBe('Rule violation');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM bans WHERE ip_hash = '${ipHash}' AND expires_at IS NULL;`)).toBe('1');
  });
});

async function createThreadNoJs(
  page: Page,
  baseURL: string,
  board: string,
  options: { subject: string; body: string },
): Promise<number> {
  await page.goto(`${baseURL}/${board}`);
  const form = page.locator(`form[action="/${board}"]`).first();
  await form.locator('input[name="subject"]').fill(options.subject);
  await form.locator('textarea[name="body"]').fill(options.body);
  await Promise.all([
    page.waitForURL(new RegExp(`/${board}/thread/\\d+`)),
    form.getByRole('button', { name: /post thread/i }).click(),
  ]);
  await expectSafePage(page);
  const threadId = Number(page.url().match(/\/thread\/(\d+)/)?.[1]);
  expect(Number.isInteger(threadId), 'thread id should be present in URL').toBe(true);
  return threadId;
}

async function createReplyNoJs(
  page: Page,
  baseURL: string,
  board: string,
  threadId: number,
  body: string,
): Promise<void> {
  await page.goto(`${baseURL}/${board}/thread/${threadId}`);
  const form = page.locator(`form[action="/${board}/thread/${threadId}"]`).first();
  await form.locator('textarea[name="body"]').fill(body);
  await Promise.all([
    page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
    form.getByRole('button', { name: /post reply/i }).click(),
  ]);
  await expectSafePage(page);
}

async function expectUsableTarget(locator: Locator, name: string, testInfo: TestInfo): Promise<void> {
  await expect(locator, `${name} should be visible`).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, `${name} should have layout bounds`).not.toBeNull();
  expect(box!.width, `${name} should not collapse horizontally`).toBeGreaterThan(0);
  expect(box!.height, `${name} should be tall enough to use`).toBeGreaterThanOrEqual(
    testInfo.project.name.includes('mobile') ? MOBILE_MIN_TARGET : DESKTOP_MIN_TARGET,
  );
  const awkward = await locator.evaluate((element) => {
    const style = window.getComputedStyle(element);
    return {
      horizontalOverflow: element.scrollWidth > element.clientWidth + 1,
      verticalOverflow: element.scrollHeight > element.clientHeight + 3 && style.whiteSpace === 'nowrap',
    };
  });
  expect(awkward.horizontalOverflow, `${name} text/content should not be clipped horizontally`).toBeFalsy();
  expect(awkward.verticalOverflow, `${name} text/content should not be clipped vertically`).toBeFalsy();
}

async function expectNoHorizontalOverflow(page: Page, name: string): Promise<void> {
  const overflow = await page.evaluate(() => {
    const doc = document.documentElement;
    const body = document.body;
    return Math.max(doc.scrollWidth - doc.clientWidth, body.scrollWidth - body.clientWidth);
  });
  expect(overflow, `${name} should not create horizontal page overflow`).toBeLessThanOrEqual(2);
}

async function expectNoCoveredCenters(locator: Locator, name: string): Promise<void> {
  const covered = await locator.evaluateAll((elements) => elements
    .filter((element) => {
      const rect = element.getBoundingClientRect();
      const style = window.getComputedStyle(element);
      return rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none';
    })
    .map((element, index) => {
      const rect = element.getBoundingClientRect();
      const x = rect.left + rect.width / 2;
      const y = rect.top + rect.height / 2;
      if (x < 0 || y < 0 || x > window.innerWidth || y > window.innerHeight) {
        return null;
      }
      const top = document.elementFromPoint(x, y);
      return top && (element === top || element.contains(top) || top.contains(element)) ? null : index + 1;
    })
    .filter((index): index is number => index !== null));
  expect(covered, `${name} should not be covered or overlapped at control centers`).toEqual([]);
}
