import {
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  test,
  updateBoardSettings,
} from './helpers';
import fsp from 'node:fs/promises';

test.describe('media upload handling', () => {
  test('image uploads render previews and disabled boards reject them safely', async ({ page, app }) => {
    const files = app.fixtures();
    const threadId = await createThread(page, app, 'img', {
      subject: 'image ok',
      body: 'image upload',
      filePath: files.tinyPng,
    });
    await expect(page.locator('.file-container')).toContainText('tiny.png');
    await expect(page.locator('[data-media-thumb="1"]').first()).toBeVisible();
    const mediaHref = await page.locator('.file-info a').first().getAttribute('href');
    expect(mediaHref).toMatch(/^\/boards\/img\//);
    const media = await page.request.get(`${app.baseURL}${mediaHref}`);
    expect(media.status()).toBe(200);

    await page.goto(`${app.baseURL}/txt`);
    await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').click();
    const form = page.locator('form[action="/txt"]').first();
    const csrf = await form.locator('input[name="_csrf"]').getAttribute('value');
    const disabledUpload = await page.request.post(`${app.baseURL}/txt`, {
      multipart: {
        _csrf: csrf ?? '',
        submission_token: `disabled-upload-${Date.now()}`,
        body: 'not allowed here',
        file: {
          name: 'tiny.png',
          mimeType: 'image/png',
          buffer: await fsp.readFile(files.tinyPng),
        },
      },
      maxRedirects: 0,
    });
    expect([400, 422]).toContain(disabledUpload.status());
    await expectSafeResponse(disabledUpload);
    await expectSafePage(page);

    await page.goto(`${app.baseURL}/img/thread/${threadId}`);
    await expectSafePage(page);
  });

  test('invalid, spoofed, oversized, and traversal-like media paths are rejected safely', async ({ page, app }, testInfo) => {
    const files = app.fixtures();
    const board = `lim${testInfo.workerIndex}${Date.now().toString(36).slice(-4)}`.slice(0, 8);
    app.createBoardCli({ short: board, name: 'Limits' });
    await updateBoardSettings(page, app, board, { maxImageSizeMb: 1 });

    for (const filePath of [files.spoofedPng, files.invalid]) {
      await page.goto(`${app.baseURL}/${board}`);
      await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').click();
      const form = page.locator(`form[action="/${board}"]`).first();
      await form.locator('textarea[name="body"]').fill('bad file');
      await form.locator('input[type="file"]').setInputFiles(filePath);
      await form.getByRole('button', { name: /post thread/i }).click();
      await expect(page.locator('.post-error-banner').first()).toContainText(/file type|not allowed|accepted/i);
      await expectSafePage(page);
    }

    await page.goto(`${app.baseURL}/${board}`);
    await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').click();
    await page.locator(`form[action="/${board}"] textarea[name="body"]`).fill('oversized');
    await page.locator(`form[action="/${board}"] input[type="file"]`).setInputFiles(files.oversized);
    await page.locator(`form[action="/${board}"] button[type="submit"]`).click();
    await expect(page.locator('.post-error-banner, body').first()).toContainText(/too large|maximum/i);
    await expectSafePage(page);

    const traversal = await page.request.get(`${app.baseURL}/boards/${board}/../../settings.toml`);
    expect([400, 403, 404]).toContain(traversal.status());
    await expectSafeResponse(traversal);
    const missing = await page.request.get(`${app.baseURL}/boards/${board}/missing.png`);
    expect(missing.status()).toBe(404);
    await expectSafeResponse(missing);
  });

  test('video, audio, PDF, and unusual filenames follow board media policy', async ({ page, app }, testInfo) => {
    const files = app.fixtures();
    const pdfBoard = `pdf${testInfo.workerIndex}${Date.now().toString(36).slice(-4)}`.slice(0, 8);
    app.createBoardCli({ short: pdfBoard, name: 'PDF Board' });
    await updateBoardSettings(page, app, pdfBoard, { allowPdf: true });

    await createThread(page, app, 'vid', { subject: 'video', body: 'video upload', filePath: files.fakeMp4 });
    await expect(page.locator('.video-container, .file-container')).toContainText('tiny.mp4');

    await createThread(page, app, 'aud', { subject: 'audio', body: 'audio upload', filePath: files.fakeOgg });
    await expect(page.locator('.audio-container, .file-container')).toContainText('tiny.ogg');

    await createThread(page, app, pdfBoard, { subject: 'pdf', body: 'pdf upload', filePath: files.tinyPdf });
    await expect(page.locator('.pdf-container, .file-container')).toContainText('tiny.pdf');

    await createThread(page, app, 'img', { subject: 'odd name', body: 'odd filename', filePath: files.oddNamePng });
    await expect(page.locator('.file-container')).toContainText('name with spaces');

    await page.goto(`${app.baseURL}/img`);
    await page.locator('.post-toggle-btn[data-action="toggle-post-form"]').click();
    await page.locator('form[action="/img"] textarea[name="body"]').fill('pdf rejected');
    await page.locator('form[action="/img"] input[type="file"]').setInputFiles(files.tinyPdf);
    await page.locator('form[action="/img"] button[type="submit"]').click();
    await expect(page.locator('.post-error-banner:not([hidden])').first()).toContainText(/pdf uploads are disabled/i);
  });
});
