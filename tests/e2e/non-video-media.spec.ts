import path from 'node:path';
import { expectConsoleError } from './diagnostics';
import { createThread, expect, expectSafePage, test } from './helpers';

// The default harness deliberately points FFmpeg at an unavailable executable.
test('common audio waveforms finish in the worker without FFmpeg', async ({ page, app }, testInfo) => {
  test.setTimeout(180_000);
  const media = path.resolve(__dirname, '../fixtures/media');
  const audioFiles = ['tone.mp3', 'tone.flac', 'tone.wav', 'tone.ogg', 'tone.aac', 'tone.m4a', 'tone-alac.m4a', 'tone.opus', 'speech-mode.opus', 'surround.opus', 'opus-surround-distinct.opus', 'opus-surround-padded-gain.opus', 'opus-surround-discrete.opus', 'audio.webm'];
  const declareNativeControlRender = () => {
    if (process.platform !== 'darwin'
        || testInfo.project.name !== 'mobile-webkit'
        || testInfo.project.use.deviceScaleFactor !== 3) return;
    // macOS WebKit's native controls request @3x PNG placards for this iPhone
    // profile, but its bundle ships only @1x/@2x. These native messages have
    // no script source URL; application console errors remain undeclared.
    // Register only one native control set for each test-owned thread render
    // or poll reload. A valid slower worker may require additional reloads.
    const port = new URL(app.baseURL).port;
    for (const [icon, perRender] of [
      ['invalid-placard', 1], ['pip-placard', 2], ['airplay-placard', 1],
    ] as const) {
      expectConsoleError({
        pattern: new RegExp(`^Button failed to load, iconName = ${icon}, layoutTraits = \\[MacOSLayoutTraits Inline\\], src = blob:http://127\\.0\\.0\\.1:${port}/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`),
        url: /^$/,
        times: perRender,
        optional: true,
        reason: 'macOS WebKit native media controls lack DPR3 PNG placard assets',
      });
    }
  };
  for (const file of audioFiles) {
    declareNativeControlRender();
    await createThread(page, app, 'aud', {
      subject: `Rust waveform ${file}`,
      body: 'common audio decoder and atomic waveform job',
      filePath: path.join(media, file),
    });
    await expect(page.locator('.audio-container')).toBeVisible();
    await expect.poll(async () => {
      declareNativeControlRender();
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
