import type { Page, TestInfo } from '@playwright/test';
import { spawnSync } from 'node:child_process';
import fsp from 'node:fs/promises';
import path from 'node:path';
import { expectNoHorizontalOverflow } from './phase4-helpers';
import {
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  sqliteQuery,
  test,
  updateBoardSettings,
} from './helpers';

test.skip(process.env.RUSTCHAN_E2E_MEDIA_TOOLCHAIN !== '1', 'opt-in real media toolchain pass only');

test.describe('real media toolchain', () => {
  test('AV1 in WebM, Matroska and MP4 converts to verified VP9 WebM', async ({ page, app }) => {
    test.setTimeout(180_000);
    const ffmpeg = process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg';
    const ffprobe = process.env.RUSTCHAN_E2E_FFPROBE_PATH ?? 'ffprobe';
    const decoders = spawnSync(ffmpeg, ['-hide_banner', '-decoders'], { encoding: 'utf8' });
    test.skip(decoders.status !== 0 || !/\b(?:av1|libdav1d|libaom-av1)\b/.test(decoders.stdout ?? ''), 'installed FFmpeg lacks an AV1 decoder');
    test.skip(spawnSync(ffprobe, ['-version'], { stdio: 'ignore' }).status !== 0, 'optional FFprobe oracle is unavailable');
    const media = path.resolve(__dirname, '../fixtures/media');
    for (const ext of ['webm', 'mkv', 'mp4']) {
      const input = path.join(app.fixtureDir, `av1-input.${ext}`);
      const args = ['-hide_banner', '-loglevel', 'error', '-i', path.join(media, `av1.${ext}`)];
      // Add AAC only to MP4 to verify conversion to WebM-compatible Opus audio.
      if (ext === 'mp4') args.push('-i', path.join(media, 'tone.m4a'), '-map', '0:v:0', '-map', '1:a:0');
      // Harmless source metadata makes the result smaller under the existing size policy.
      args.push('-c', 'copy', '-metadata', `comment=${'fixture padding '.repeat(512)}`, '-y', input);
      const fixture = spawnSync(ffmpeg, args, { encoding: 'utf8', timeout: 30_000 });
      expect(fixture.status, fixture.stderr).toBe(0);
      await createThread(page, app, 'vid', {
        subject: `AV1 ${ext}`, body: 'AV1 container and codec conversion regression', filePath: input,
      });
      await expect.poll(async () => {
        await page.reload();
        await expectSafePage(page);
        return page.locator('.file-info a').first().getAttribute('href');
      }, { timeout: 90_000, intervals: [1_000, 2_000] }).toMatch(ext === 'webm' ? /\.vp9\.webm$/ : /\.webm$/);
      const href = await page.locator('.file-info a').first().getAttribute('href');
      const response = await page.request.get(`${app.baseURL}${href}`);
      expect(response.status()).toBe(200);
      expect(response.headers()['content-type']).toContain('video/webm');
      const output = path.join(app.fixtureDir, `verified-${ext}.webm`);
      await fsp.writeFile(output, await response.body());
      const probe = spawnSync(ffprobe, ['-v', 'error', '-show_streams', '-show_format', '-of', 'json', output], { encoding: 'utf8', timeout: 30_000 });
      expect(probe.status, probe.stderr).toBe(0);
      const info = JSON.parse(probe.stdout);
      const video = info.streams.find((stream: { codec_type: string }) => stream.codec_type === 'video');
      expect(video.codec_name).toBe('vp9');
      expect(video.pix_fmt).toBe('yuv420p');
      expect([video.width, video.height]).toEqual([64, 64]);
      expect(Number(info.format.duration)).toBeGreaterThan(0);
      expect(info.streams.filter((stream: { codec_type: string }) => stream.codec_type === 'audio').map((stream: { codec_name: string }) => stream.codec_name)).toEqual(ext === 'mp4' ? ['opus'] : []);
      expect(sqliteQuery(app, "SELECT mime_type || '|' || media_processing_state || '|' || COALESCE(media_processing_error, '') FROM posts ORDER BY id DESC LIMIT 1;")).toBe('video/webm||');
      expect(Number(sqliteQuery(app, 'SELECT file_size FROM posts ORDER BY id DESC LIMIT 1;'))).toBe((await fsp.stat(output)).size);
    }
  });

  test('uploads use real thumbnails, transcodes, waveform previews, and PDF fallback safely', async ({ page, app, javaScriptEnabled }, testInfo) => {
    test.setTimeout(180_000);
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
    if (javaScriptEnabled) await page.locator('.image-preview').click();
    await inspectMediaLayout(page, testInfo, 'image');

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
    if (javaScriptEnabled) await page.locator('.video-preview').click();
    await inspectMediaLayout(page, testInfo, 'video');

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
    await inspectMediaLayout(page, testInfo, 'audio');

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
    expect(pdfThumbContentType).toContain('image/webp');
    const pdfHref = await page.locator('.file-info a').first().getAttribute('href');
    const pdfResponse = await page.request.get(`${app.baseURL}${pdfHref}`);
    expect(pdfResponse.status()).toBe(200);
    expect(pdfResponse.headers()['content-type']).toContain('application/pdf');
    await expect(page.locator('iframe.media-expanded-pdf')).toHaveAttribute('data-src', pdfHref ?? '');
    await expectSafeResponse(pdfResponse);
    if (javaScriptEnabled) await page.locator('.pdf-preview').click();
    await inspectMediaLayout(page, testInfo, 'pdf');
  });
});

async function inspectMediaLayout(page: Page, testInfo: TestInfo, kind: string): Promise<void> {
  for (const width of [320, 360, 390, 430, 768, 1024, 1280, 1440]) {
    await page.setViewportSize({ width, height: 844 });
    await expectNoHorizontalOverflow(page, `${kind} at ${width}px`, 1);
    if (width === 320 || width === 1440) {
      await page.locator('.file-container').first().scrollIntoViewIfNeeded();
      await page.screenshot({ path: testInfo.outputPath(`${kind}-${width}.png`) });
    }
  }
}
