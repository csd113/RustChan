import path from 'node:path';
import { createThread, expect, expectSafePage, test } from './helpers';

// The default harness deliberately points FFmpeg at an unavailable executable.
test('common audio waveforms finish in the worker without FFmpeg', async ({ page, app }) => {
  test.setTimeout(180_000);
  const media = path.resolve(__dirname, '../fixtures/media');
  for (const file of ['tone.mp3', 'tone.flac', 'tone.wav', 'tone.ogg', 'tone.aac', 'tone.m4a', 'tone-alac.m4a', 'tone.opus', 'speech-mode.opus', 'audio.webm']) {
    await createThread(page, app, 'aud', {
      subject: `Rust waveform ${file}`,
      body: 'common audio decoder and atomic waveform job',
      filePath: path.join(media, file),
    });
    await expect(page.locator('.audio-container')).toBeVisible();
    await expect.poll(async () => {
      await page.reload();
      await expectSafePage(page);
      return page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    }, { timeout: 20_000, intervals: [100, 250, 500] }).toMatch(/\.png$/);
    const src = await page.locator('[data-media-thumb="1"]').first().getAttribute('src');
    const response = await page.request.get(`${app.baseURL}${src}`);
    expect(response.status()).toBe(200);
    expect(response.headers()['content-type']).toContain('image/png');
    const pixels = await response.body();
    expect(pixels.readUInt32BE(16)).toBe(250);
    expect(pixels.readUInt32BE(20)).toBe(125);
  }
});
