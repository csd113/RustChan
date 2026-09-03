import { spawnSync } from 'node:child_process';
import {
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  test,
  updateBoardSettings,
} from './helpers';

test.skip(process.env.RUSTCHAN_E2E_MEDIA_TOOLCHAIN !== '1', 'opt-in real media toolchain pass only');

test.describe('real media toolchain', () => {
  test('uploads use real thumbnails, transcodes, waveform previews, and PDF fallback safely', async ({ page, app }, testInfo) => {
    const files = app.fixtures();

    await createThread(page, app, 'img', {
      subject: 'real image',
      body: 'thumbnail from real image',
      filePath: files.tinyPng,
    });
    await expect(page.locator('[data-media-thumb="1"]').first()).toBeVisible();
    const imageThumb = await page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    expect(imageThumb).toBeTruthy();
    const imageThumbResponse = await page.request.get(`${app.baseURL}${imageThumb}`);
    expect(imageThumbResponse.status()).toBe(200);
    expect(imageThumbResponse.headers()['content-type']).toContain('image/webp');

    await createThread(page, app, 'vid', {
      subject: 'real video',
      body: 'video upload with background transcode',
      filePath: files.fakeMp4,
    });
    await expect(page.locator('.video-container')).toContainText('tiny.mp4');
    const originalVideoHref = await page.locator('.file-info a').first().getAttribute('href');
    expect(originalVideoHref).toMatch(/\.mp4$/);
    const videoThumb = await page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    expect(videoThumb).toBeTruthy();
    const videoThumbResponse = await page.request.get(`${app.baseURL}${videoThumb}`);
    expect(videoThumbResponse.status()).toBe(200);
    expect(videoThumbResponse.headers()['content-type']).toContain('image/webp');

    await expect.poll(async () => {
      await page.reload();
      await expectSafePage(page);
      return page.locator('.file-info a').first().getAttribute('href');
    }, { timeout: 90_000, intervals: [1_000, 2_000, 5_000] }).toMatch(/\.webm$/);
    const transcodedVideoHref = await page.locator('.file-info a').first().getAttribute('href');
    const transcodedVideo = await page.request.get(`${app.baseURL}${transcodedVideoHref}`);
    expect(transcodedVideo.status()).toBe(200);
    expect(transcodedVideo.headers()['content-type']).toContain('video/webm');
    await expect(page.locator('video source')).toHaveAttribute('type', 'video/webm');
    const staleMp4 = await page.request.get(`${app.baseURL}${originalVideoHref}`, { maxRedirects: 0 });
    expect([301, 308]).toContain(staleMp4.status());

    await createThread(page, app, 'aud', {
      subject: 'real audio',
      body: 'audio upload with waveform',
      filePath: files.fakeOgg,
    });
    await expect(page.locator('.audio-container')).toContainText('tiny.ogg');
    await expect.poll(async () => {
      await page.reload();
      await expectSafePage(page);
      return page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    }, { timeout: 90_000, intervals: [1_000, 2_000, 5_000] }).toMatch(/\.png$/);
    const waveformThumb = await page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    const waveformResponse = await page.request.get(`${app.baseURL}${waveformThumb}`);
    expect(waveformResponse.status()).toBe(200);
    expect(waveformResponse.headers()['content-type']).toContain('image/png');

    const pdfBoard = `rpdf${testInfo.workerIndex}${Date.now().toString(36).slice(-3)}`.slice(0, 8);
    app.createBoardCli({ short: pdfBoard, name: 'Real PDF Board' });
    await updateBoardSettings(page, app, pdfBoard, { allowPdf: true });
    await createThread(page, app, pdfBoard, {
      subject: 'real pdf',
      body: 'pdf upload',
      filePath: files.tinyPdf,
    });
    await expect(page.locator('.pdf-container')).toContainText('tiny.pdf');
    const pdfThumb = await page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    expect(pdfThumb).toBeTruthy();
    const pdfThumbResponse = await page.request.get(`${app.baseURL}${pdfThumb}`);
    expect(pdfThumbResponse.status()).toBe(200);
    const pdfThumbContentType = pdfThumbResponse.headers()['content-type'];
    if (pdfRendererAvailable()) {
      expect(['image/webp', 'image/svg+xml']).toContain(pdfThumbContentType?.split(';')[0]);
    } else {
      expect(pdfThumbContentType).toContain('image/svg+xml');
    }
    const pdfHref = await page.locator('.file-info a').first().getAttribute('href');
    const pdfResponse = await page.request.get(`${app.baseURL}${pdfHref}`);
    expect(pdfResponse.status()).toBe(200);
    expect(pdfResponse.headers()['content-type']).toContain('application/pdf');
    await expect(page.locator('iframe.media-expanded-pdf')).toHaveAttribute('data-src', pdfHref ?? '');
    await expectSafeResponse(pdfResponse);
  });
});

function pdfRendererAvailable(): boolean {
  return ['pdftoppm', 'mutool', 'qlmanage'].some((tool) => {
    const result = spawnSync(tool, ['-h'], { stdio: 'ignore' });
    return !result.error;
  });
}
