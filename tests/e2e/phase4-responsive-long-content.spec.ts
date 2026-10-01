import fs from 'node:fs';
import path from 'node:path';
import {
  adminLogin,
  createReply,
  createThread,
  expect,
  expectSafePage,
  setBoardFixtureSettings,
  sqliteExec,
  test,
  uniqueShort,
} from './helpers';
import {
  expectNamedInteractiveControls,
  expectNoCoveredCenters,
  expectNoHorizontalOverflow,
  expectSafeBody,
  phase4SkipUnless,
  watchClientErrors,
} from './phase4-helpers';

test.describe('phase 4 responsive and long-content stress', () => {
  test.beforeEach(async ({}, testInfo) => {
    phase4SkipUnless(
      testInfo,
      ['chromium', 'mobile-webkit'],
      'responsive long-content coverage runs on desktop Chromium and mobile WebKit',
    );
  });

  test('long board metadata, titles, posts, filenames, counters, and admin rows stay usable without overlap', async ({ page, app }, testInfo) => {
    const assertNoClientErrors = watchClientErrors(page);
    const board = uniqueShort('long', testInfo);
    const longToken = 'phase4longtoken'.repeat(12);
    app.createBoardCli({
      short: board,
      name: `Long Name ${longToken}`,
      description: `Description with URLs https://example.test/${longToken} and dense text ${'wide '.repeat(50)}`,
    });
    setBoardFixtureSettings(app, board, {
      name: `Long Board Name ${longToken}`,
      description: `Long board description ${longToken} ${'wrapping content '.repeat(40)}`,
      allowImages: true,
      allowArchive: true,
      postCooldownSecs: 0,
      maxThreads: 200,
      maxArchivedThreads: 200,
    });

    const longFile = path.join(app.fixtureDir, `${'very-long-upload-file-name-'.repeat(5)}phase4.png`);
    fs.copyFileSync(app.fixtures().tinyPng, longFile);

    const threadIds: number[] = [];
    for (let index = 0; index < 11; index += 1) {
      const threadId = await createThread(page, app, board, {
        subject: `Subject ${index} ${longToken}`,
        body: [
          `Long body ${index}`,
          'unbroken_' + 'x'.repeat(180),
          `quoted >>>/${board}/${index}`,
          'paragraph '.repeat(120),
        ].join('\n'),
        filePath: index === 0 ? longFile : undefined,
      });
      threadIds.push(threadId);
      if (index === 0) {
        for (let reply = 0; reply < 14; reply += 1) {
          await createReply(
            page,
            app,
            board,
            threadId,
            `deep reply ${reply} ${'replyword'.repeat(20)}\n>>${reply + 1}`,
          );
        }
      }
    }

    await page.goto(app.baseURL);
    await expectSafePage(page);
    await expectNoHorizontalOverflow(page, 'home with long board card');
    await expectNamedInteractiveControls(page, 'main', 'home with long board card');

    await page.goto(`${app.baseURL}/${board}`);
    await expectSafeBody(page, 'long board index');
    await expectNoHorizontalOverflow(page, 'long board index');
    await expect(page.locator('.pagination')).toContainText(/page 1 \/ 2/i);
    await expect(page.getByRole('link', { name: /\[next\]/i })).toBeVisible();
    await expectNoCoveredCenters(
      page.locator('.thread .subject a:visible, .thread .post-body a:visible, .thread .thread-footer a:visible, .pagination a:visible, .post-toggle-btn:visible'),
      'long board controls',
    );

    await page.goto(`${app.baseURL}/${board}?page=2`);
    await expectSafeBody(page, 'long board second page');
    await expectNoHorizontalOverflow(page, 'long board second page');
    await expect(page.locator('.pagination')).toContainText(/page 2 \/ 2/i);

    await page.goto(`${app.baseURL}/${board}/catalog`);
    await expectSafeBody(page, 'long catalog');
    await expectNoHorizontalOverflow(page, 'long catalog');
    await expectNoCoveredCenters(page.locator('.catalog-item a:visible, .catalog-thread-menu-toggle:visible'), 'catalog controls');

    await page.goto(`${app.baseURL}/${board}/thread/${threadIds[0]}`);
    await expectSafeBody(page, 'long thread');
    await expectNoHorizontalOverflow(page, 'long thread');
    await expect(page.locator('.post')).toHaveCount(15);
    await expect(page.locator('.file-info a').first()).toContainText(/very-long-upload-fil/);
    await expect(page.locator('.file-info a').first()).toHaveAttribute('href', /\.png$/);
    await expectNoCoveredCenters(page.locator('.thread-nav a:visible, .thread-nav button:visible, .post-controls button:visible'), 'long thread controls');

    const longReason = `phase4 reason ${longToken} ${'policy '.repeat(40)}`;
    sqliteExec(app, [
      "INSERT INTO bans (ip_hash, reason, expires_at)",
      `VALUES ('${'a'.repeat(64)}', '${longReason.replaceAll("'", "''")}', NULL);`,
      "INSERT INTO word_filters (pattern, replacement)",
      `VALUES ('${'needle'.repeat(20)}', '${'replacement'.repeat(18)}');`,
    ].join(' '));
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=reports#reports`);
    await expectSafePage(page, { allowAdminInternals: true });
    await expectNoHorizontalOverflow(page, 'admin moderation with long rows');
    await expectNoCoveredCenters(
      page.locator('#active-bans button:visible, #word-filters button:visible, #reports a:visible'),
      'admin moderation long-row controls',
    );

    assertNoClientErrors();
  });
});
