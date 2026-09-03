import {
  adminLogin,
  createThread,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  test,
  uniqueShort,
} from './helpers';
import {
  expectFocusVisible,
  expectNamedInteractiveControls,
  expectNoCoveredCenters,
  expectNoHorizontalOverflow,
  expectSafeBody,
  expectUsableTarget,
  phase4SkipUnless,
  watchClientErrors,
} from './phase4-helpers';

test.describe('phase 4 accessibility basics and progressive enhancement', () => {
  test.beforeEach(async ({}, testInfo) => {
    phase4SkipUnless(
      testInfo,
      ['chromium', 'mobile-webkit', 'firefox-nojs'],
      'phase 4 accessibility coverage runs on Chromium, mobile WebKit, and no-JS Firefox',
    );
  });

  test('home, board, catalog, thread, posting, preferences, and admin forms expose keyboardable named controls', async ({ page, app }, testInfo) => {
    const assertNoClientErrors = watchClientErrors(page);
    const board = uniqueShort('a11y', testInfo);
    app.createBoardCli({ short: board, name: 'Accessibility Board', description: 'keyboard and label coverage' });
    setBoardFixtureSettings(app, board, {
      allowImages: true,
      allowEditing: true,
      allowSelfDelete: true,
      postCooldownSecs: 0,
    });
    const threadId = await createThread(page, app, board, {
      subject: 'accessibility thread',
      body: 'thread body for keyboard checks',
      filePath: app.fixtures().tinyPng,
    });

    await page.goto(app.baseURL);
    await expectSafePage(page);
    await expectNamedInteractiveControls(page, 'body', 'home page');
    await expectFocusVisible(page.getByRole('link', { name: /home/i }).first(), 'home link');
    await expectUsableTarget(page.locator(`.board-card-link[href="/${board}/catalog"], .board-card-link[href="/${board}"]`).first(), 'board card link', testInfo);

    await page.goto(`${app.baseURL}/${board}`);
    await expectSafeBody(page, 'board index');
    await expectNoHorizontalOverflow(page, 'board index');
    await expectNamedInteractiveControls(page, 'main', 'board index');
    const postToggle = page.locator('[data-action="toggle-post-form"]').first();
    await expectFocusVisible(postToggle, 'new thread toggle');
    await postToggle.focus();
    await page.keyboard.press('Enter');
    const threadForm = page.locator(`form[action="/${board}"]`).first();
    await expect(threadForm).toBeVisible();
    await expectUsableTarget(threadForm.getByLabel('subject'), 'thread subject field', testInfo);
    await expectUsableTarget(threadForm.getByLabel('body'), 'thread body field', testInfo);
    await expectUsableTarget(threadForm.getByLabel('upload'), 'thread upload field', testInfo);

    await page.goto(`${app.baseURL}/${board}/catalog`);
    await expectSafeBody(page, 'catalog');
    await expectNoHorizontalOverflow(page, 'catalog');
    await expectNamedInteractiveControls(page, 'main', 'catalog');

    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    await expectSafeBody(page, 'thread');
    await expectNoHorizontalOverflow(page, 'thread');
    await expectNamedInteractiveControls(page, 'main', 'thread page');
    const replyToggle = page.locator('[data-action="toggle-post-form"]').first();
    if (await replyToggle.isVisible()) {
      await replyToggle.click();
    }
    const replyForm = page.locator(`form[action="/${board}/thread/${threadId}"]`).first();
    await expectUsableTarget(replyForm.getByLabel('body'), 'reply body field', testInfo);
    await expectNoCoveredCenters(page.locator('.post-controls button:visible, .post-controls a:visible'), 'thread post controls');

    if (testInfo.project.name === 'mobile-webkit') {
      await expect(page.locator('.post-controls .report-btn').first()).toBeVisible();
      await expect(page.locator('.self-action-controls .edit-btn').first()).toBeVisible();
      await expect(page.locator('.self-action-controls .del-btn').first()).toBeVisible();
    }

    await page.goto(`${app.baseURL}/admin`);
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNamedInteractiveControls(page, '.admin-login-form', 'admin login form');
    await expectFocusVisible(page.getByLabel('Username'), 'admin username field');
    await expectFocusVisible(page.getByLabel('Password'), 'admin password field');

    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=site-settings#site-settings`);
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'admin settings');
    await expectFocusVisible(page.locator('select[name="default_theme"]').first(), 'admin default theme picker');
    await expectNamedInteractiveControls(page, '#site-settings', 'admin site settings');

    assertNoClientErrors();
  });

  test('report, edit, delete confirmation, preferences, and media controls close cleanly and restore focus', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name === 'firefox-nojs', 'scripted modal behavior is covered by no-JS fallback forms separately');

    const board = uniqueShort('dlg', testInfo);
    app.createBoardCli({ short: board, name: 'Dialog Board', description: 'dialog coverage' });
    setBoardFixtureSettings(app, board, {
      allowImages: true,
      allowEditing: true,
      allowSelfDelete: true,
      postCooldownSecs: 0,
    });
    const threadId = await createThread(page, app, board, {
      subject: 'dialog controls',
      body: 'owned post with media for dialog coverage',
      filePath: app.fixtures().tinyPng,
    });

    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    const reportTrigger = page.locator('.post-controls .report-btn').first();
    await reportTrigger.focus();
    await reportTrigger.click();
    await expect(page.locator('#report-modal')).toBeVisible();
    await expect(page.locator('#report-modal').getByLabel('reason')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(page.locator('#report-modal')).toBeHidden();
    await expect(reportTrigger).toBeFocused();

    const editTrigger = page.locator('.self-action-controls .edit-btn').first();
    await editTrigger.focus();
    await editTrigger.click();
    await expect(page.locator('#edit-modal.is-open')).toBeVisible();
    await expect(page.getByLabel('edit post body')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(page.locator('#edit-modal')).toBeHidden();
    await expect(editTrigger).toBeFocused();

    const deleteTrigger = page.locator('.self-action-controls .del-btn').first();
    await deleteTrigger.focus();
    await deleteTrigger.click();
    await expect(page.locator('#confirm-modal')).toBeVisible();
    await expect(page.locator('#confirm-modal-cancel')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(page.locator('#confirm-modal')).toBeHidden();
    await expect(deleteTrigger).toBeFocused();

    const preferences = page.locator('.user-preferences-panel > summary').first();
    await preferences.focus();
    await page.keyboard.press('Enter');
    await expect(page.locator('.user-preferences-panel[open]')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.locator('.user-preferences-panel[open]')).toHaveCount(0);

    const preview = page.locator('.media-preview').first();
    await preview.click();
    await expect(page.locator('.media-expanded-image').first()).toBeVisible();
    const closeMedia = page.locator('.media-close-btn').first();
    await expectUsableTarget(closeMedia, 'expanded media close button', testInfo);
    await closeMedia.click();
    await expect(preview).toBeVisible();
  });

  test('critical public flows remain usable without optional main JavaScript', async ({ page, context, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'broken optional-JS degradation is checked once in Chromium');

    const board = uniqueShort('pe', testInfo);
    app.createBoardCli({ short: board, name: 'Progressive Board', description: 'optional JS degradation' });
    await context.route('**/static/main.js*', (route) => route.abort());
    await page.goto(`${app.baseURL}/${board}`);
    await expectSafeBody(page, 'board without main.js');
    await page.locator('[data-action="toggle-post-form"]').first().click();
    await expect(page.locator('#post-form-wrap:target, #post-form-wrap').first()).toBeVisible();
    const threadForm = page.locator(`form[action="/${board}"]`).first();
    await expect(threadForm.getByLabel('subject')).toBeVisible();
    await threadForm.getByLabel('subject').fill('no optional js thread');
    await threadForm.getByLabel('body').fill('form opened through the anchor fallback');
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/\\d+`)),
      page.getByRole('button', { name: /post thread/i }).click(),
    ]);
    await expect(page.locator('body')).toContainText('form opened through the anchor fallback');
  });
});
