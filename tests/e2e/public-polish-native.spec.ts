import fs from 'node:fs/promises';
import path from 'node:path';
import { expectHttpError, type Page, type TestInfo } from './diagnostics';
import { createThread, expect, setBoardFixtureSettings, setThreadFixtureState, sqliteExec, sqliteQuery, test } from './helpers';

async function captureNativeError(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  const screenshots = path.resolve('output/playwright/public-polish');
  await fs.mkdir(screenshots, { recursive: true });
  await page.screenshot({ path: path.join(screenshots, `${process.env.PUBLIC_POLISH_STAGE ?? 'after'}-native-${name}-${testInfo.project.name}.png`), fullPage: true });
}

test.describe('public posting recovery with native forms', () => {
  test.use({ javaScriptEnabled: false });

  test('CAPTCHA rejection preserves poll choices and explains attachment reselection', async ({ page, app }, testInfo) => {
    setBoardFixtureSettings(app, 'img', { allowCaptcha: true, postCooldownSecs: 0 });
    await page.goto(`${app.baseURL}/img`);
    const form = page.locator('form[action="/img"]').first();
    await form.locator('[name="name"]').fill('匿名 <reader>');
    await form.locator('[name="subject"]').fill('A poll & a recoverable draft');
    await form.locator('[name="body"]').fill('>>123\n> A nested quote\nA draft that must survive rejection.');
    await form.locator('details.poll-creator > summary').click();
    await form.locator('[name="poll_question"]').fill('Which <choice> & why?');
    await form.locator('[name="poll_option"]').nth(0).fill('日本語 & <option>');
    await form.locator('[name="poll_option"]').nth(1).fill('Second choice');
    await form.locator('noscript details summary').click();
    await form.locator('[name="poll_option"]').nth(2).fill('Third choice beyond the initial two');
    await form.locator('[name="poll_duration_value"]').fill('3');
    await form.locator('[name="poll_duration_unit"]').selectOption('days');
    await form.locator('[name="file"]').setInputFiles(app.fixtures().tinyPng);
    await form.locator('[name="captcha_answer"]').fill('WRONG');
    const captcha = await form.locator('[name="captcha_id"]').inputValue();
    expectHttpError({ method: 'POST', path: '/img', status: 422, reason: 'invalid CAPTCHA intentionally exercises native recovery' });
    await form.getByRole('button', { name: 'post thread', exact: true }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/CAPTCHA verification failed/i);
    await page.waitForLoadState('load');
    await captureNativeError(page, testInfo, 'poll');
    await expect(form.locator('[name="name"]')).toHaveValue('匿名 <reader>');
    await expect(form.locator('[name="subject"]')).toHaveValue('A poll & a recoverable draft');
    await expect(form.locator('[name="body"]')).toHaveValue('>>123\n> A nested quote\nA draft that must survive rejection.');
    await expect(form.locator('[name="poll_question"]')).toHaveValue('Which <choice> & why?');
    await expect(form.locator('[name="poll_option"]').nth(0)).toHaveValue('日本語 & <option>');
    await expect(form.locator('[name="poll_option"]').nth(1)).toHaveValue('Second choice');
    await expect(form.locator('[name="poll_option"]').nth(2)).toHaveValue('Third choice beyond the initial two');
    await expect(form.locator('[name="poll_duration_value"]')).toHaveValue('3');
    await expect(form.locator('[name="poll_duration_unit"]')).toHaveValue('days');
    await expect(form.locator('details.poll-creator')).toHaveAttribute('open', '');
    await expect(form).toContainText(/choose your attachments again.*browser/i);
    await expect(form.locator('[name="file"]')).toHaveValue('');
    expect(await form.locator('[name="captcha_id"]').inputValue()).not.toBe(captcha);
    await expect(form.getByRole('button', { name: 'post thread', exact: true })).toBeEnabled();

    setBoardFixtureSettings(app, 'img', { allowCaptcha: false });
    await form.locator('[name="captcha_answer"]').fill('UNUSED');
    await form.locator('[name="file"]').setInputFiles(app.fixtures().tinyPng);
    await Promise.all([
      page.waitForURL(/\/img\/thread\/\d+/),
      form.getByRole('button', { name: 'post thread', exact: true }).click(),
    ]);
    await expect(page.locator('.poll-container')).toContainText('Third choice beyond the initial two');
    await expect(page.locator('.post-body').first()).toContainText('A draft that must survive rejection.');
  });

  test('a consumed CAPTCHA gives expired recovery and a fresh native challenge', async ({ page, app }) => {
    setBoardFixtureSettings(app, 'pub', { allowCaptcha: true, postCooldownSecs: 0 });
    await page.goto(`${app.baseURL}/pub`);
    const form = page.locator('form[action="/pub"]').first();
    await page.waitForLoadState('load');
    const challenge = await form.locator('[name="captcha_id"]').inputValue();
    // An already-consumed challenge follows the same Expired branch as a TTL expiry.
    // The separate Rust expiry test covers elapsed time; this needs no production clock hook.
    const consumed = await page.request.post(`${app.baseURL}/pub`, {
      multipart: {
        _csrf: await form.locator('[name="_csrf"]').inputValue(),
        submission_token: 'consume-native-challenge', body: 'Never accepted',
        captcha_id: challenge, captcha_answer: 'WRONG',
      },
      maxRedirects: 0,
    });
    expect(consumed.status()).toBe(422);
    expect(await consumed.text()).toContain('CAPTCHA verification failed');
    await form.locator('[name="body"]').fill('Draft survives expired challenge 日本語');
    await form.locator('.poll-creator > summary').click();
    await form.locator('[name="poll_question"]').fill('Retain this poll after expiry?');
    await form.locator('[name="poll_option"]').nth(0).fill('Yes');
    await form.locator('[name="poll_option"]').nth(1).fill('Absolutely');
    await form.locator('[name="captcha_answer"]').fill('WRONG');
    expectHttpError({ method: 'POST', path: '/pub', status: 422, reason: 'replayed consumed challenge exercises expired CAPTCHA UI' });
    await form.getByRole('button', { name: 'post thread', exact: true }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText('CAPTCHA expired. Request a new challenge and try again.');
    await expect(form.locator('[name="body"]')).toHaveValue('Draft survives expired challenge 日本語');
    await expect(form.locator('[name="poll_question"]')).toHaveValue('Retain this poll after expiry?');
    expect(await form.locator('[name="captcha_id"]').inputValue()).not.toBe(challenge);
    await expect(form.locator('.captcha-refresh-link')).toBeVisible();
    await expect(form.getByRole('button', { name: 'post thread', exact: true })).toBeEnabled();
    expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('0');
  });

  test('native cooldown explains the wait and retains reply body and sage', async ({ page, app }, testInfo) => {
    const threadId = await createThread(page, app, 'pub', { body: 'Recent opening post' });
    setBoardFixtureSettings(app, 'pub', { postCooldownSecs: 120 });
    await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
    const form = page.locator(`form[action="/pub/thread/${threadId}"]`).first();
    await form.locator('[name="body"]').fill('Recover this cooldown draft 日本語');
    await form.locator('[name="sage"]').check();
    expectHttpError({ method: 'POST', path: `/pub/thread/${threadId}`, status: 422, reason: 'real per-board posting cooldown rejects a second post' });
    const responsePromise = page.waitForResponse((response) => response.request().method() === 'POST' && new URL(response.url()).pathname === `/pub/thread/${threadId}`);
    await form.getByRole('button', { name: 'post reply', exact: true }).click();
    const response = await responsePromise;
    expect(response.status()).toBe(422);
    await testInfo.attach('cooldown-response.json', { contentType: 'application/json', body: Buffer.from(JSON.stringify({ status: response.status(), retryAfter: response.headers()['retry-after'] ?? null })) });
    await expect(page.locator('.post-error-banner').first()).toContainText(/Please wait [1-9]\d* more seconds? before posting again/);
    await expect(form.locator('[name="body"]')).toHaveValue('Recover this cooldown draft 日本語');
    await expect(form.locator('[name="sage"]')).toBeChecked();
    await expect(form.getByRole('button', { name: 'post reply', exact: true })).toBeEnabled();
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`)).toBe('1');
    // Age only this isolated fixture instead of changing server time or sleeping.
    sqliteExec(app, `UPDATE posts SET created_at = created_at - 121 WHERE thread_id = ${threadId};`);
    await Promise.all([
      page.waitForURL(new RegExp(`/pub/thread/${threadId}#p\\d+$`)),
      form.getByRole('button', { name: 'post reply', exact: true }).click(),
    ]);
    await expect(page.locator('.post.reply').last()).toContainText('Recover this cooldown draft 日本語');
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId};`)).toBe('2');
  });

  test('an invalid native CSRF form retains a copyable draft and directs a fresh form', async ({ page, app }) => {
    await page.goto(`${app.baseURL}/pub`);
    const form = page.locator('form[action="/pub"]').first();
    await form.locator('[name="body"]').fill('Draft from an older tab <日本語>');
    // Public signed tokens intentionally survive cookie rotation; corrupt the token instead.
    await form.locator('[name="_csrf"]').evaluate((input: HTMLInputElement) => { input.value = 'invalid-token'; });
    expectHttpError({ method: 'POST', path: '/pub', status: 403, reason: 'invalid native form token intentionally exercises CSRF denial' });
    await form.getByRole('button', { name: 'post thread', exact: true }).click();
    await expect(page.locator('#post-recovery-body')).toHaveValue('Draft from an older tab <日本語>');
    await expect(page.locator('.error-page')).toContainText('Reload the posting page for a fresh form after copying your draft.');
    expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('0');
  });

  test('an oversized native upload retains a copyable server draft and poll for a fresh retry', async ({ page, app }, testInfo) => {
    // Scope Rust tempfile storage to this isolated runtime for cleanup assertions.
    await app.stop();
    const temporaryUploads = path.join(app.rootDir, 'rejected-upload-temp');
    await fs.mkdir(temporaryUploads);
    app.envOverrides.TMPDIR = temporaryUploads;
    await app.start();
    setBoardFixtureSettings(app, 'img', {
      maxImageSizeMb: 1, maxVideoSizeMb: 1, maxAudioSizeMb: 1, maxPdfSizeMb: 1,
    });
    await page.goto(`${app.baseURL}/img`);
    const form = page.locator('form[action="/img"]').first();
    await form.locator('[name="subject"]').fill('Oversized upload draft');
    await form.locator('[name="body"]').fill('Keep this native draft 日本語');
    await form.locator('.poll-creator > summary').click();
    await form.locator('[name="poll_question"]').fill('Recover this poll?');
    await form.locator('[name="poll_option"]').nth(0).fill('Yes');
    await form.locator('[name="poll_option"]').nth(1).fill('Absolutely');
    await form.locator('[name="poll_duration_value"]').fill('15');
    await form.locator('[name="poll_duration_unit"]').selectOption('minutes');
    await form.locator('[name="file"]').setInputFiles(app.fixtures().oversized);
    expectHttpError({ method: 'POST', path: '/img', status: 413, reason: 'oversized upload exercises early parser rejection' });
    await form.getByRole('button', { name: 'post thread', exact: true }).click();
    const saved = page.locator('.error-page .post-form');
    await expect(page.locator('#post-recovery-body')).toHaveValue('Keep this native draft 日本語');
    await expect(saved).toContainText('Subject: Oversized upload draft');
    await expect(saved).toContainText('Poll: Recover this poll?');
    await expect(saved).toContainText('Option 1: Yes');
    await expect(saved).toContainText('Option 2: Absolutely');
    await expect(saved).toContainText('Duration: 15 minutes');
    await expect(page.locator('.error-page')).toContainText('some browsers clear it');
    await expect(page.locator('.error-page')).toContainText('Choose attachments again');
    await expect(page.locator('.error-page')).not.toContainText('Only controls received');
    await captureNativeError(page, testInfo, 'oversized');
    expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('0');
    expect(await fs.readdir(temporaryUploads)).toEqual([]);
    const bodyCopy = await page.locator('#post-recovery-body').inputValue();
    const copyControl = async (prefix: string) => (await saved.locator('p').filter({ hasText: new RegExp(`^${prefix}:`) }).innerText()).slice(prefix.length + 2);
    const subjectCopy = await copyControl('Subject');
    const questionCopy = await copyControl('Poll');
    const optionCopies = [await copyControl('Option 1'), await copyControl('Option 2')];
    // Firefox clears the original form after a rejected POST: copy into a fresh form.
    await page.goto(`${app.baseURL}/img`);
    await form.locator('[name="body"]').fill(bodyCopy);
    await form.locator('[name="subject"]').fill(subjectCopy);
    await form.locator('.poll-creator > summary').click();
    await form.locator('[name="poll_question"]').fill(questionCopy);
    await form.locator('[name="poll_option"]').nth(0).fill(optionCopies[0]);
    await form.locator('[name="poll_option"]').nth(1).fill(optionCopies[1]);
    await form.locator('[name="poll_duration_value"]').fill('15');
    await form.locator('[name="poll_duration_unit"]').selectOption('minutes');
    await expect(form.locator('[name="submission_token"]')).toHaveValue(/^[a-f0-9]{32}$/);
    await form.locator('[name="file"]').setInputFiles(app.fixtures().tinyPng);
    await Promise.all([
      page.waitForURL(/\/img\/thread\/\d+/),
      form.getByRole('button', { name: 'post thread', exact: true }).click(),
    ]);
    await expect(page.locator('.poll-container')).toContainText('Absolutely');
    expect(sqliteQuery(app, 'SELECT COUNT(*) FROM posts;')).toBe('1');
  });

  for (const state of ['locked', 'deleted'] as const) {
    test(`a stale native reply to a ${state} thread retains a copyable draft`, async ({ page, app }, testInfo) => {
      const threadId = await createThread(page, app, 'pub', { body: 'Opening post' });
      await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
      const form = page.locator(`form[action="/pub/thread/${threadId}"]`).first();
      await form.locator('[name="name"]').fill('Anonymous recovery');
      await form.locator('[name="body"]').fill('Do not lose <this> reply 日本語');
      await form.locator('[name="sage"]').check();
      if (state === 'locked') setThreadFixtureState(app, threadId, { locked: true });
      else sqliteExec(app, `DELETE FROM threads WHERE id = ${threadId};`);
      expectHttpError({ method: 'POST', path: `/pub/thread/${threadId}`, status: state === 'locked' ? 403 : 404, reason: `thread became ${state} after composer loaded` });
      await form.getByRole('button', { name: 'post reply', exact: true }).click();
      await expect(page.locator('#post-recovery-body')).toHaveValue('Do not lose <this> reply 日本語');
      await expect(page.locator('#post-recovery-body')).toHaveAttribute('readonly', '');
      await page.locator('#post-recovery-body').focus();
      await expect(page.locator('#post-recovery-body')).toBeFocused();
      await captureNativeError(page, testInfo, state);
      await expect(page.locator('.error-page')).toContainText('Anonymous recovery');
      await expect(page.locator('.error-page')).toContainText('Sage: yes');
      await expect(page.locator('.error-page')).toContainText(state === 'locked' ? 'This thread is locked.' : 'Thread not found.');
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE thread_id = ${threadId} AND is_op = 0;`)).toBe('0');
    });
  }

  test('rejected native reply retains sage and identifies missing attachments', async ({ page, app }) => {
    const threadId = await createThread(page, app, 'img', { subject: 'Reply recovery', body: 'Opening post' });
    setBoardFixtureSettings(app, 'img', { allowCaptcha: true, postCooldownSecs: 0 });
    await page.goto(`${app.baseURL}/img/thread/${threadId}`);
    const form = page.locator(`form[action="/img/thread/${threadId}"]`).first();
    await form.locator('[name="name"]').fill('Reply author');
    await form.locator('[name="body"]').fill('Recoverable reply 日本語');
    await form.locator('[name="sage"]').check();
    await form.locator('[name="file"]').setInputFiles(app.fixtures().tinyPng);
    await form.locator('[name="captcha_answer"]').fill('WRONG');
    expectHttpError({ method: 'POST', path: `/img/thread/${threadId}`, status: 422, reason: 'invalid CAPTCHA intentionally exercises native reply recovery' });
    await form.getByRole('button', { name: 'post reply', exact: true }).click();
    await expect(page.locator('.post-error-banner').first()).toContainText(/CAPTCHA verification failed/i);
    await expect(form.locator('[name="name"]')).toHaveValue('Reply author');
    await expect(form.locator('[name="body"]')).toHaveValue('Recoverable reply 日本語');
    await expect(form.locator('[name="sage"]')).toBeChecked();
    await expect(form).toContainText(/choose your attachments again.*browser/i);
    await expect(form.getByRole('button', { name: 'post reply', exact: true })).toBeEnabled();
  });
});
