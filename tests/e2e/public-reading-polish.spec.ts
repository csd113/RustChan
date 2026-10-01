import fs from 'node:fs/promises';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { expectConsoleError, noteEvent } from './diagnostics';
import { BUILTIN_THEMES, expectReadableContrast } from './phase4-helpers';
import {
  createReplyViaRequest,
  createThread,
  createThreadViaRequest,
  expect,
  isNoJsProject,
  sqliteQuery,
  setBoardFixtureSettings,
  test,
  threadIdFromUrl,
  uniqueShort,
} from './helpers';

async function screenshot(page: Parameters<typeof createThreadViaRequest>[0], project: string, name: string): Promise<void> {
  const directory = path.resolve(process.env.RUSTCHAN_AUDIT_OUTPUT ?? 'output/playwright/public-polish/reading', 'screenshots');
  await fs.mkdir(directory, { recursive: true });
  await page.screenshot({ path: path.join(directory, `${project}-${name}.png`), fullPage: true });
}

function pcmWave(): Buffer {
  const sampleRate = 8_000;
  const samples = sampleRate * 3;
  const dataSize = samples * 2;
  const wave = Buffer.alloc(44 + dataSize);
  wave.write('RIFF', 0);
  wave.writeUInt32LE(36 + dataSize, 4);
  wave.write('WAVEfmt ', 8);
  wave.writeUInt32LE(16, 16);
  wave.writeUInt16LE(1, 20);
  wave.writeUInt16LE(1, 22);
  wave.writeUInt32LE(sampleRate, 24);
  wave.writeUInt32LE(sampleRate * 2, 28);
  wave.writeUInt16LE(2, 32);
  wave.writeUInt16LE(16, 34);
  wave.write('data', 36);
  wave.writeUInt32LE(dataSize, 40);
  for (let index = 0; index < samples; index += 1) {
    wave.writeInt16LE(Math.round(400 * Math.sin(index * 2 * Math.PI * 440 / sampleRate)), 44 + index * 2);
  }
  return wave;
}

test('valid H264 video plays and corrupt original media offers a recovery download', async ({ page, app }, testInfo) => {
  const ffmpeg = process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg';
  test.skip(spawnSync(ffmpeg, ['-version'], { stdio: 'ignore' }).status !== 0, 'The optional existing ffmpeg tool is required to generate a real video fixture');
  expectConsoleError({
    pattern: /^Button failed to load, iconName = [a-z-]+-placard, layoutTraits = \[MacOSLayoutTraits Inline\], src = blob:/,
    times: 'any',
    optional: true,
    reason: 'WebKit internal native-media-control icon logging, as covered by media.spec.ts',
  });
  const videoPath = path.join(app.fixtureDir, 'playable-color.mp4');
  const generated = spawnSync(ffmpeg, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=c=green:s=160x90:d=3', '-an', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', videoPath], { encoding: 'utf8' });
  expect(generated.status, generated.stderr).toBe(0);
  const validVideo = await fs.readFile(videoPath);
  await createThread(page, app, 'vid', { body: 'Real H264 playback and corrupt source recovery', filePath: videoPath });
  const href = await page.locator('#thread-posts .file-info a').first().getAttribute('href');
  expect(href).toMatch(/^\/boards\/vid\/.*\.mp4$/);
  expect((await page.request.get(`${app.baseURL}${href}`)).status()).toBe(200);
  const preview = page.locator('#thread-posts .video-preview').first();
  const video = page.locator('#thread-posts video.media-expanded-video').first();
  if (isNoJsProject(testInfo)) {
    // The existing native fallback opens the original in the browser's own
    // media document; the enhanced inline player stays hidden without scripts.
    await preview.click();
    await expect(page).toHaveURL(`${app.baseURL}${href}`);
    const nativeVideo = page.locator('video').first();
    await expect(nativeVideo).toBeVisible();
    await expect.poll(() => nativeVideo.evaluate(element => (element as HTMLVideoElement).currentTime)).toBeGreaterThan(0);
    expect(await nativeVideo.evaluate(element => (element as HTMLVideoElement).error)).toBeNull();
    await nativeVideo.evaluate(element => (element as HTMLVideoElement).pause());
    await screenshot(page, testInfo.project.name, 'video-native-played');
    return;
  }
  await preview.click();
  await expect(video).toBeVisible();
  await expect.poll(() => video.evaluate(element => (element as HTMLVideoElement).currentTime)).toBeGreaterThan(0);
  expect(await video.evaluate(element => (element as HTMLVideoElement).error)).toBeNull();
  await video.evaluate(element => (element as HTMLVideoElement).pause());
  await screenshot(page, testInfo.project.name, 'video-played');

  const storedVideo = path.join(app.dataDir, href!.replace(/^\/boards\//, 'boards/'));
  await fs.writeFile(storedVideo, 'synthetic corrupt MP4');
  await page.route('**/boards/**/*.mp4', route => route.continue());
  await page.reload();
  await video.evaluate((element, originalPath) => {
    element.querySelector('source')!.setAttribute('src', originalPath + '?audit=corrupt');
    (element as HTMLVideoElement).load();
  }, href!);
  await preview.click();
  await expect.poll(() => video.evaluate(element => {
    const player = element as HTMLVideoElement;
    return player.error !== null || player.networkState === HTMLMediaElement.NETWORK_NO_SOURCE;
  })).toBe(true);
  const error = page.locator('.media-playback-error');
  await expect(error).toContainText('Video could not be played');
  await expect(error.locator('a')).toHaveAttribute('href', href!);
  await screenshot(page, testInfo.project.name, 'video-corrupt');
  const [download] = await Promise.all([page.waitForEvent('download'), error.locator('a').click()]);
  expect(await download.failure()).toBeNull();
  const downloadedPath = await download.path();
  if (!downloadedPath) throw new Error('Original video download has no saved file');
  expect(await fs.readFile(downloadedPath, 'utf8')).toBe('synthetic corrupt MP4');

  await fs.writeFile(storedVideo, validVideo);
  await video.evaluate((element, originalPath) => {
    element.querySelector('source')!.setAttribute('src', originalPath + '?audit=recovered');
    (element as HTMLVideoElement).load();
  }, href!);
  await page.locator('#thread-posts .media-close-btn').first().press('Enter');
  await expect(preview).toBeFocused();
  await preview.click();
  await expect.poll(() => video.evaluate(element => (element as HTMLVideoElement).currentTime)).toBeGreaterThan(0);
  await expect(error).toBeHidden();
  await video.evaluate(element => (element as HTMLVideoElement).pause());
  await screenshot(page, testInfo.project.name, 'video-recovered');
});

test('valid WAV plays from native controls and corrupt media explains recovery', async ({ page, app }, testInfo) => {
  expectConsoleError({
    pattern: /^Button failed to load, iconName = [a-z-]+-placard, layoutTraits = \[MacOSLayoutTraits Inline\], src = blob:/,
    times: 'any',
    optional: true,
    reason: 'WebKit internal native-media-control icon logging, as covered by media.spec.ts',
  });
  const audioPath = path.join(app.fixtureDir, 'playable-tone.wav');
  await fs.writeFile(audioPath, pcmWave());
  await createThread(page, app, 'aud', { body: 'Real PCM WAV playback and graceful failure', filePath: audioPath });
  const audio = page.locator('#thread-posts audio').first();
  const original = page.locator('#thread-posts .file-info a').first();
  const href = await original.getAttribute('href');
  expect(href).toMatch(/^\/boards\/aud\/.*\.wav$/);
  expect((await page.request.get(`${app.baseURL}${href}`)).status()).toBe(200);
  await audio.scrollIntoViewIfNeeded();
  const bounds = await audio.boundingBox();
  if (!bounds) throw new Error('Native audio controls have no layout bounds');
  await audio.click({ position: { x: 25, y: bounds.height / 2 } });
  await expect.poll(() => audio.evaluate(element => (element as HTMLAudioElement).currentTime)).toBeGreaterThan(0);
  expect(await audio.evaluate(element => (element as HTMLAudioElement).error)).toBeNull();
  await audio.evaluate(element => (element as HTMLAudioElement).pause());
  await screenshot(page, testInfo.project.name, 'wav-played');
  if (isNoJsProject(testInfo)) {
    await expect(original).toBeVisible();
    return;
  }

  // Corrupt only this runtime's stored synthetic file. Disabling the browser
  // cache ensures the decoder sees the broken bytes rather than the good WAV.
  await fs.writeFile(path.join(app.dataDir, href!.replace(/^\/boards\//, 'boards/')), Buffer.from('synthetic corrupt WAV'));
  await page.route('**/boards/**/*.wav', route => route.continue());
  await page.reload();
  const corruptResponse = await page.request.get(`${app.baseURL}${href}`);
  expect(await corruptResponse.text()).toBe('synthetic corrupt WAV');
  await audio.evaluate(element => {
    const source = element.querySelector('source');
    if (!source) throw new Error('Audio source is missing');
    source.setAttribute('src', source.getAttribute('src') + '?audit=corrupt');
    (element as HTMLAudioElement).load();
  });
  noteEvent('corrupt-audio-source', JSON.stringify(await audio.evaluate(element => ({ source: element.querySelector('source')?.getAttribute('src'), currentSrc: (element as HTMLAudioElement).currentSrc }))));
  await audio.scrollIntoViewIfNeeded();
  await audio.click({ position: { x: 25, y: bounds.height / 2 } });
  await expect.poll(() => audio.evaluate(element => {
    const player = element as HTMLAudioElement;
    return player.error !== null || player.networkState === HTMLMediaElement.NETWORK_NO_SOURCE;
  })).toBe(true);
  noteEvent('corrupt-audio-error', JSON.stringify(await audio.evaluate(element => ({ errorCode: (element as HTMLAudioElement).error?.code ?? null, networkState: (element as HTMLAudioElement).networkState }))));
  await screenshot(page, testInfo.project.name, 'wav-corrupt');
  await expect(page.locator('.media-playback-error')).toContainText('Audio could not be played');
  await expect(page.locator('.media-playback-error a')).toHaveAttribute('href', href!);
  await expect(page.locator('.media-playback-error a')).toHaveText('Download the original file');
  if (testInfo.project.name === 'chromium') {
    const returnTo = new URL(page.url()).pathname;
    for (const [slug] of BUILTIN_THEMES) {
      await page.goto(`${app.baseURL}/theme/${slug}?return_to=${encodeURIComponent(returnTo)}`);
      await audio.evaluate((element, originalPath) => {
        element.querySelector('source')!.setAttribute('src', originalPath + '?audit=' + document.documentElement.dataset.activeTheme);
        (element as HTMLAudioElement).load();
      }, href!);
      await audio.click({ position: { x: 25, y: bounds.height / 2 } });
      await expect(page.locator('.media-playback-error')).toBeVisible();
      await expectReadableContrast(page, '.media-playback-error', `${slug} media failure`, 4.5);
      await expectReadableContrast(page, '.media-playback-error a', `${slug} media recovery action`, 4.5);
      await screenshot(page, testInfo.project.name, `wav-corrupt-${slug}`);
    }
  }
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.locator('.media-playback-error a').click(),
  ]);
  expect(await download.failure()).toBeNull();
  const downloadedPath = await download.path();
  if (!downloadedPath) throw new Error('Original media download has no saved file');
  expect(await fs.readFile(downloadedPath, 'utf8')).toBe('synthetic corrupt WAV');
  await fs.writeFile(path.join(app.dataDir, href!.replace(/^\/boards\//, 'boards/')), pcmWave());
  await audio.evaluate((element, originalPath) => {
    element.querySelector('source')!.setAttribute('src', originalPath + '?audit=recovered');
    (element as HTMLAudioElement).load();
  }, href!);
  await audio.click({ position: { x: 25, y: bounds.height / 2 } });
  await expect.poll(() => audio.evaluate(element => (element as HTMLAudioElement).currentTime)).toBeGreaterThan(0);
  await expect(page.locator('.media-playback-error')).toBeHidden();
  await audio.evaluate(element => (element as HTMLAudioElement).pause());
  await screenshot(page, testInfo.project.name, 'wav-recovered');
});

test('media failure before the deferred script loads still receives recovery feedback', async ({ page, app }, testInfo) => {
  test.skip(isNoJsProject(testInfo), 'This regression protects the deferred JavaScript enhancement; native media links remain covered separately');
  expectConsoleError({
    pattern: /^Button failed to load, iconName = [a-z-]+-placard, layoutTraits = \[MacOSLayoutTraits Inline\], src = blob:/,
    times: 'any',
    optional: true,
    reason: 'WebKit internal native-media-control icon logging, as covered by media.spec.ts',
  });
  const audioPath = path.join(app.fixtureDir, 'early-failure.wav');
  await fs.writeFile(audioPath, pcmWave());
  await createThread(page, app, 'aud', { body: 'Media failure while the application script is still loading', filePath: audioPath });
  const href = await page.locator('#thread-posts .file-info a').first().getAttribute('href');
  expect(href).toMatch(/^\/boards\/aud\/.*\.wav$/);
  await fs.writeFile(path.join(app.dataDir, href!.replace(/^\/boards\//, 'boards/')), 'synthetic corrupt WAV');
  let releaseScript: () => void = () => {};
  const scriptGate = new Promise<void>(resolve => { releaseScript = resolve; });
  await page.route('**/static/main.js*', async route => { await scriptGate; await route.continue(); });
  await page.route('**/boards/**/*.wav', route => route.continue());
  await page.reload({ waitUntil: 'commit' });
  const audio = page.locator('#thread-posts audio').first();
  try {
    await expect(audio).toBeVisible();
    await audio.evaluate((element, originalPath) => {
      element.querySelector('source')!.setAttribute('src', originalPath + '?audit=early-failure');
      (element as HTMLAudioElement).load();
    }, href!);
    const bounds = await audio.boundingBox();
    if (!bounds) throw new Error('Native audio controls have no layout bounds');
    await audio.click({ position: { x: 25, y: bounds.height / 2 } });
    await expect.poll(() => audio.evaluate(element => (element as HTMLAudioElement).networkState === HTMLMediaElement.NETWORK_NO_SOURCE)).toBe(true);
    expect(await audio.evaluate(element => (element as HTMLAudioElement).error)).toBeNull();
    await expect(audio).not.toHaveAttribute('data-playback-error-wired', '1');
  } finally {
    releaseScript();
  }
  await expect(audio).toHaveAttribute('data-playback-error-wired', '1');
  await expect(page.locator('.media-playback-error')).toContainText('Audio could not be played');
  await screenshot(page, testInfo.project.name, 'media-early-failure');
});

test('quote and backlink navigation preserves browser history and reduced motion', async ({ page, app }, testInfo) => {
  const thread = await createThreadViaRequest(page, app, 'pub', {
    subject: 'Dense Unicode discussion 日本語 café',
    body: 'Opening post\n' + 'compact reading context\n'.repeat(30),
  });
  const op = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${thread} AND is_op = 1;`));
  await createReplyViaRequest(page, app, 'pub', thread, `>>${op}\nNested quote\n>quoted text\n${'discussion line\n'.repeat(40)}`);
  const reply = Number(sqliteQuery(app, `SELECT MAX(id) FROM posts WHERE thread_id = ${thread};`));
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto(`${app.baseURL}/pub/thread/${thread}#p${reply}`);
  const quote = page.locator(`#p${reply} a.quotelink[data-pid="${op}"]`);
  await quote.scrollIntoViewIfNeeded();
  await screenshot(page, testInfo.project.name, 'quote-origin');
  await quote.focus();
  await quote.press('Enter');
  await expect(page).toHaveURL(new RegExp(`#p${op}$`));
  await expect.poll(() => page.locator(`#p${op}`).evaluate(el => el.getBoundingClientRect().top)).toBeGreaterThanOrEqual(0);
  await page.goBack();
  await expect(page).toHaveURL(new RegExp(`#p${reply}$`));
  await expect(page.locator(`#p${reply}`)).toBeInViewport();
  await quote.click();
  await expect(page).toHaveURL(new RegExp(`#p${op}$`));
  if (!isNoJsProject(testInfo)) {
    await page.locator(`#thread-posts #backrefs-${op} a[data-pid="${reply}"]`).click();
    await expect(page).toHaveURL(new RegExp(`#p${reply}$`));
    await expect(page.locator(`#p${reply}`)).toHaveClass(/post-highlighted/);
  }
  await screenshot(page, testInfo.project.name, 'quote-restored');
});

test('portrait image and audio attachments keep file links and reader context usable', async ({ page, app }, testInfo) => {
  // The maintained media.spec.ts documents this WebKit native-control icon
  // logging. Keep the same narrow engine-internal declaration here.
  expectConsoleError({
    pattern: /^Button failed to load, iconName = [a-z-]+-placard, layoutTraits = \[MacOSLayoutTraits Inline\], src = blob:/,
    times: 'any',
    optional: true,
    reason: 'WebKit internal native-media-control icon logging, as covered by media.spec.ts',
  });
  const board = uniqueShort('read', testInfo);
  app.createBoardCli({ short: board, name: 'Media reading' });
  setBoardFixtureSettings(app, board, { allowImages: true, allowAudio: true, allowVideo: false, allowPdf: false });
  const portrait = path.join(app.fixtureDir, `日本語-${'long-portrait-filename-'.repeat(5)}.png`);
  const fixturePage = await page.context().newPage();
  await fixturePage.setContent('<div style="width:180px;height:1400px;background:linear-gradient(#6b8e23,#2b4c7e)"></div>');
  await fixturePage.locator('div').screenshot({ path: portrait });
  await fixturePage.close();

  await page.goto(`${app.baseURL}/${board}`);
  if (!isNoJsProject(testInfo)) await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator(`form[action="/${board}"]`).first();
  await form.locator('textarea[name="body"]').fill('Image and audio discussion\nUnicode 日本語 café\nhttps://example.test/' + 'long-path-'.repeat(20));
  await form.locator('input[name="audio_file"]').setInputFiles(app.fixtures().fakeOgg);
  await form.locator('details.upload-secondary-toggle > summary').click();
  await form.locator('input[name="image_file"]').setInputFiles(portrait);
  await form.locator('button[type="submit"]').click();
  await page.waitForURL(new RegExp(`/${board}/thread/\\d+`));
  const post = page.locator('#thread-posts .post').first();
  await expect(post.locator('.image-audio-combo')).toHaveCount(1);
  await expect(post.locator('.audio-player-combo')).toBeVisible();
  const directImage = post.locator('.file-info a').first();
  await expect(directImage).toHaveAttribute('title', /^日本語-.*\.png$/);
  await expect(directImage).toContainText(/\.png$/);
  expect((await page.request.get(new URL(await directImage.getAttribute('href') ?? '', app.baseURL).href)).status()).toBe(200);
  await screenshot(page, testInfo.project.name, 'portrait-audio');
  if (!isNoJsProject(testInfo)) {
    const preview = post.locator('.image-preview');
    await preview.focus();
    const before = await post.locator('.file-info').first().evaluate(el => ({
      top: el.getBoundingClientRect().top,
      height: el.getBoundingClientRect().height,
    }));
    await preview.press('Enter');
    await expect(post.locator('.media-expanded-image')).toBeVisible();
    await screenshot(page, testInfo.project.name, 'portrait-expanded');
    noteEvent('audio-fixture-state', JSON.stringify(await post.locator('audio').evaluate((element) => {
      const audio = element as HTMLAudioElement;
      return { errorCode: audio.error?.code ?? null, readyState: audio.readyState, paused: audio.paused };
    })));
    const expandedMetadataHeight = await post.locator('.file-info').first().evaluate(el => el.getBoundingClientRect().height);
    await post.locator('.media-close-btn').press('Enter');
    await expect(preview).toBeFocused();
    const after = await post.locator('.file-info').first().evaluate(el => el.getBoundingClientRect().top);
    // Showing the close control can increase this metadata row's height.
    // Native scroll anchoring may adjust by that row, but must keep the reader
    // at the same attachment rather than move to another part of the thread.
    expect(Math.abs(after - before.top)).toBeLessThanOrEqual(Math.max(before.height, expandedMetadataHeight));
    await expect(preview).toBeInViewport();
    await expect(post.locator('.media-expanded-image')).toBeHidden();
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(1);
});

test('voting in a dense thread returns to the poll results', async ({ page, app }, testInfo) => {
  await page.goto(`${app.baseURL}/pub`);
  if (!isNoJsProject(testInfo)) await page.locator('[data-action="toggle-post-form"]').first().click();
  const form = page.locator('form[action="/pub"]').first();
  await form.locator('textarea[name="body"]').fill('Dense poll discussion\n' + 'reading context\n'.repeat(70));
  await form.locator('details.poll-creator > summary').click();
  await form.locator('input[name="poll_question"]').fill('Which compact layout reads best? 日本語');
  await form.locator('input[name="poll_option"]').nth(0).fill('Current style');
  await form.locator('input[name="poll_option"]').nth(1).fill('Small consistency fixes');
  await form.locator('button[type="submit"]').click();
  await page.waitForURL(/\/pub\/thread\/\d+/);
  const thread = threadIdFromUrl(page.url());
  await page.locator('.poll-vote-option input').first().check();
  await page.locator('.poll-vote-btn').click();
  await expect(page).toHaveURL(`${app.baseURL}/pub/thread/${thread}#poll`);
  await expect(page.locator('.poll-total')).toHaveText('1 total vote');
  await expect(page.locator('#poll')).toBeInViewport();
  await screenshot(page, testInfo.project.name, 'poll-results');
});

test('search post numbers and quotes open their thread context', async ({ page, app }, testInfo) => {
  const thread = await createThreadViaRequest(page, app, 'pub', { subject: 'Search context', body: 'Opening post outside matching results' });
  const op = Number(sqliteQuery(app, `SELECT id FROM posts WHERE thread_id = ${thread} AND is_op = 1;`));
  await createReplyViaRequest(page, app, 'pub', thread, `unique-search-needle >>${op}\n日本語 café`);
  const reply = Number(sqliteQuery(app, `SELECT MAX(id) FROM posts WHERE thread_id = ${thread};`));
  const searchURL = `${app.baseURL}/pub/search?q=unique-search-needle`;
  await page.goto(searchURL);
  await screenshot(page, testInfo.project.name, 'search-context');
  await page.locator(`#p${reply} .post-num`).click();
  await expect(page).toHaveURL(`${app.baseURL}/pub/thread/${thread}#p${reply}`);
  await page.goBack();
  await expect(page.locator('#board-search-input')).toHaveValue('unique-search-needle');
  if (['chromium', 'webkit', 'firefox'].includes(testInfo.project.name)) {
    await page.evaluate(() => {
      document.addEventListener('click', event => {
        if (!(event.target instanceof Element) || !event.target.closest('a.quotelink')) return;
        (window as Window & { auditQuoteClick?: unknown }).auditQuoteClick = {
          button: event.button, control: event.ctrlKey, meta: event.metaKey,
          shift: event.shiftKey, alt: event.altKey, defaultPrevented: event.defaultPrevented,
        };
      });
    });
    const [opened] = await Promise.all([
      page.context().waitForEvent('page'),
      page.locator(`#p${reply} a.quotelink`).click({ modifiers: ['ControlOrMeta'] }),
    ]).finally(async () => {
      noteEvent('modified-quote-click', JSON.stringify(await page.evaluate(() => (window as Window & { auditQuoteClick?: unknown }).auditQuoteClick)));
    });
    await expect(opened).toHaveURL(`${app.baseURL}/pub/thread/${thread}#p${op}`);
    await expect(page).toHaveURL(searchURL);
    await opened.close();
  }
  await page.locator(`#p${reply} a.quotelink`).click();
  await expect(page).toHaveURL(`${app.baseURL}/pub/thread/${thread}#p${op}`);
  await screenshot(page, testInfo.project.name, 'search-thread-context');
});
