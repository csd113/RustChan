import {
  adminCsrf,
  adminLogin,
  createReply,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  publicCsrf,
  sqliteExec,
  sqliteQuery,
  test,
  uniqueShort,
} from './helpers';

test.describe('phase 2 moderation, report, and ban lifecycle', () => {
  test('JS report modal, duplicate reports, resolution, deletion, bans, appeals, and mod log are coherent', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'JS moderation lifecycle coverage runs on Chromium first');

    const board = uniqueShort('mod', testInfo);
    app.createBoardCli({ short: board, name: 'Moderation Phase 2' });
    const threadId = await createThread(page, app, board, {
      subject: 'moderation report target',
      body: 'OP remains visible while a reply is moderated',
    });
    await createReply(page, app, board, threadId, 'reply selected for report lifecycle');
    const replyPostId = Number(sqliteQuery(
      app,
      `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 0 ORDER BY id DESC LIMIT 1;`,
    ));
    const replyIpHash = sqliteQuery(app, `SELECT ip_hash FROM posts WHERE id = ${replyPostId};`);
    expect(replyIpHash).toMatch(/^[0-9a-f]{64}$/i);

    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    await expectSafePage(page);
    await expect(page.locator('body')).not.toContainText(replyIpHash);
    const reportButton = page.locator(`#p${replyPostId} .report-btn`);
    await reportButton.click();
    await expect(page.locator('#report-modal')).toBeVisible();
    await page.locator('#report-reason').fill('modal report reason '.repeat(20));
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
      page.locator('#report-submit-btn').click(),
    ]);
    await expect(page.locator('body')).toContainText(/reported|reply selected for report lifecycle/i);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM reports WHERE post_id = ${replyPostId} AND status = 'open';`)).toBe('1');
    expect(Number(sqliteQuery(app, `SELECT length(reason) FROM reports WHERE post_id = ${replyPostId};`))).toBeLessThanOrEqual(256);

    const duplicate = await page.request.post(`${app.baseURL}/report`, {
      form: {
        _csrf: await publicCsrf(page, app, `/${board}/thread/${threadId}`),
        post_id: String(replyPostId),
        thread_id: String(threadId),
        board,
        reason: 'duplicate report should not create another open row',
      },
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(duplicate.status());
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM reports WHERE post_id = ${replyPostId} AND status = 'open';`)).toBe('1');

    const loggedOutContext = await page.context().browser()!.newContext();
    const loggedOutBan = await loggedOutContext.request.post(`${app.baseURL}/admin/ban/add`, {
      form: {
        _csrf: 'missing-session',
        ip_hash: 'a'.repeat(64),
        reason: 'logged out moderation attempt',
        duration_hours: '1',
      },
      maxRedirects: 0,
    });
    expect(loggedOutBan.status()).toBe(403);
    await expectSafeResponse(loggedOutBan);
    await loggedOutContext.close();

    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=reports#reports`);
    await expect(page.locator('#reports')).toContainText('reply selected for report lifecycle');
    await expect(page.locator('#reports')).toContainText(replyIpHash.slice(0, 16));
    const reportId = sqliteQuery(app, `SELECT id FROM reports WHERE post_id = ${replyPostId} AND status = 'open' LIMIT 1;`);
    const csrf = await adminCsrf(page, app);
    const csrfDenied = await page.request.post(`${app.baseURL}/admin/report/resolve`, {
      form: { _csrf: 'invalid', report_id: reportId },
      maxRedirects: 0,
    });
    expect(csrfDenied.status()).toBe(403);
    await expectSafeResponse(csrfDenied);
    const resolve = await page.request.post(`${app.baseURL}/admin/report/resolve`, {
      form: { _csrf: csrf, report_id: reportId },
      maxRedirects: 0,
    });
    expect(resolve.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT status FROM reports WHERE id = ${reportId};`)).toBe('resolved');

    const deleteTargetThread = await createThread(page, app, board, {
      subject: 'delete report target',
      body: 'delete target op',
    });
    await createReply(page, app, board, deleteTargetThread, 'reply selected for admin delete');
    const deletePostId = sqliteQuery(
      app,
      `SELECT id FROM posts WHERE thread_id = ${deleteTargetThread} AND is_op = 0 ORDER BY id DESC LIMIT 1;`,
    );
    await adminLogin(page, app);
    const deleteCsrfDenied = await page.request.post(`${app.baseURL}/admin/post/delete`, {
      form: {
        _csrf: 'invalid',
        post_id: deletePostId,
        board,
      },
      maxRedirects: 0,
    });
    expect(deleteCsrfDenied.status()).toBe(403);
    await expectSafeResponse(deleteCsrfDenied);
    const deletePost = await page.request.post(`${app.baseURL}/admin/post/delete`, {
      form: {
        _csrf: await adminCsrf(page, app),
        post_id: deletePostId,
        board: `//evil.example/${board}`,
      },
      maxRedirects: 0,
    });
    expect(deletePost.status()).toBe(303);
    expect(deletePost.headers().location).toBe(`/${board}/thread/${deleteTargetThread}`);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM posts WHERE id = ${deletePostId};`)).toBe('0');

    await page.goto(`${app.baseURL}/admin/mod-log`);
    await expectSafePage(page, { allowAdminInternals: true });
    await expect(page.locator('body')).toContainText('resolve_report');
    await expect(page.locator('body')).toContainText('delete_post');
    await expect(page.locator('body')).not.toContainText('/Users/');

    const longReason = `phase 2 ban reason ${'R'.repeat(700)}`;
    const banCsrfDenied = await page.request.post(`${app.baseURL}/admin/ban/add`, {
      form: {
        _csrf: 'invalid',
        ip_hash: replyIpHash,
        reason: longReason,
        duration_hours: '1',
      },
      maxRedirects: 0,
    });
    expect(banCsrfDenied.status()).toBe(403);
    await expectSafeResponse(banCsrfDenied);
    const ban = await page.request.post(`${app.baseURL}/admin/ban/add`, {
      form: {
        _csrf: await adminCsrf(page, app),
        ip_hash: replyIpHash,
        reason: longReason,
        duration_hours: '1',
      },
      maxRedirects: 0,
    });
    expect(ban.status()).toBe(303);
    await page.goto(`${app.baseURL}/${board}`);
    const bannedPostCsrf = await postFormCsrf(page, board);
    const bannedPost = await page.request.post(`${app.baseURL}/${board}`, {
      multipart: {
        _csrf: bannedPostCsrf,
        submission_token: `banned-${Date.now()}`,
        subject: 'banned thread',
        body: 'posting should be denied while banned',
      },
      headers: publicSameOriginHeaders(app, `/${board}`),
      maxRedirects: 0,
    });
    expect(bannedPost.status()).toBe(403);
    const bannedBody = await expectSafeResponse(bannedPost);
    expect(bannedBody).toContain('phase 2 ban reason');
    expect(bannedBody).not.toContain('/Users/');

    await page.goto(`${app.baseURL}/banned?reason=${encodeURIComponent('phase 2 ban reason')}`);
    await page.locator('textarea[name="reason"]').fill('appeal message '.repeat(45));
    await Promise.all([
      page.waitForURL(/\/appeal|\/$/),
      page.getByRole('button', { name: /submit appeal/i }).click(),
    ]);
    await expect(page.locator('body')).toContainText(/appeal has been submitted|appeal submitted/i);
    await adminLogin(page, app);
    await page.goto(`${app.baseURL}/admin/panel?open=reports#appeals`);
    await expect(page.locator('body')).toContainText('appeal message');
    const appealId = sqliteQuery(app, `SELECT id FROM ban_appeals WHERE ip_hash = '${replyIpHash}' AND status = 'open' LIMIT 1;`);
    const acceptAppeal = await page.request.post(`${app.baseURL}/admin/appeal/accept`, {
      form: {
        _csrf: await adminCsrf(page, app),
        appeal_id: appealId,
        ip_hash: replyIpHash,
      },
      maxRedirects: 0,
    });
    expect(acceptAppeal.status()).toBe(303);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM bans WHERE ip_hash = '${replyIpHash}';`)).toBe('0');

    await page.goto(`${app.baseURL}/${board}`);
    const postAfterUnbanCsrf = await postFormCsrf(page, board);
    const postAfterUnban = await page.request.post(`${app.baseURL}/${board}`, {
      multipart: {
        _csrf: postAfterUnbanCsrf,
        submission_token: `unbanned-${Date.now()}`,
        subject: 'unbanned thread',
        body: 'posting should work after appeal acceptance',
      },
      headers: publicSameOriginHeaders(app, `/${board}`),
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(postAfterUnban.status());

    const expiredBan = await page.request.post(`${app.baseURL}/admin/ban/add`, {
      form: {
        _csrf: await adminCsrf(page, app),
        ip_hash: replyIpHash,
        reason: 'expired ban should not apply',
        duration_hours: '1',
      },
      maxRedirects: 0,
    });
    expect(expiredBan.status()).toBe(303);
    sqliteExec(app, `UPDATE bans SET expires_at = strftime('%s','now') - 10 WHERE ip_hash = '${replyIpHash}';`);
    await page.goto(`${app.baseURL}/${board}`);
    const expiredAllowedCsrf = await postFormCsrf(page, board);
    const expiredAllowed = await page.request.post(`${app.baseURL}/${board}`, {
      multipart: {
        _csrf: expiredAllowedCsrf,
        submission_token: `expired-ban-${Date.now()}`,
        subject: 'expired ban allowed',
        body: 'posting should work after ban expiry',
      },
      headers: publicSameOriginHeaders(app, `/${board}`),
      maxRedirects: 0,
    });
    expect([302, 303]).toContain(expiredAllowed.status());
  });

  test('no-JS report fallback posts through the plain form when JavaScript is disabled', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'firefox-nojs', 'plain report fallback is covered by the Firefox no-JS project');

    const board = uniqueShort('mnj', testInfo);
    app.createBoardCli({ short: board, name: 'Moderation No JS' });
    const threadId = await createThread(page, app, board, {
      subject: 'no js report target',
      body: 'reported without scripts',
    });
    const postId = sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${threadId} AND is_op = 1 LIMIT 1;`);
    await page.goto(`${app.baseURL}/${board}/thread/${threadId}`);
    const fallback = page.locator(`#p${postId} form.report-fallback-form`);
    await fallback.locator('summary').click();
    await fallback.locator('input[name="reason"]').fill('no js fallback reason');
    await Promise.all([
      page.waitForURL(new RegExp(`/${board}/thread/${threadId}`)),
      fallback.getByRole('button', { name: /submit report/i }).click(),
    ]);
    await expectSafePage(page);
    expect(sqliteQuery(app, `SELECT COUNT(*) FROM reports WHERE post_id = ${postId} AND status = 'open';`)).toBe('1');
  });

});

function publicSameOriginHeaders(app: Parameters<typeof adminCsrf>[1], pathPart: string): Record<string, string> {
  return {
    Origin: app.baseURL,
    Referer: `${app.baseURL}${pathPart}`,
  };
}

async function postFormCsrf(page: Parameters<typeof publicCsrf>[0], board: string): Promise<string> {
  const toggle = page.locator('.post-toggle-btn[data-action="toggle-post-form"], [data-action="toggle-post-form"]').first();
  if (await toggle.isVisible()) {
    await toggle.click();
  }
  const postInput = page.locator(`form[action="/${board}"] input[name="_csrf"]`).first();
  if (await postInput.count() > 0) {
    const value = await postInput.inputValue();
    if (value.length > 0) return value;
  }
  const values = await page.locator('input[name="_csrf"]').evaluateAll((inputs) =>
    inputs
      .map((input) => input instanceof HTMLInputElement ? input.value : '')
      .filter((value) => value.length > 0),
  );
  expect(values.length, `csrf token for /${board}/`).toBeGreaterThan(0);
  return values[0];
}
