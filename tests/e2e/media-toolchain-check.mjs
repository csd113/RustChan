import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const REQUIRED_ENCODERS = ['libwebp', 'libvpx-vp9', 'libopus'];

if (process.argv.includes('--self-test')) {
  runSelfTest();
} else {
  runRealCheck();
}

function runRealCheck() {
  const ffmpeg = process.env.RUSTCHAN_E2E_FFMPEG_PATH ?? 'ffmpeg';

  const version = run(ffmpeg, ['-version']);
  if (!version.ok) fail(`FFmpeg is required for npm run test:e2e:media but '${ffmpeg}' is not usable.\n${version.detail}`);

  const encoders = run(ffmpeg, ['-hide_banner', '-encoders']);
  if (!encoders.ok) fail(`Could not inspect FFmpeg encoders for '${ffmpeg}'.\n${encoders.detail}`);

  const missing = REQUIRED_ENCODERS.filter((encoder) => !encoders.stdout.includes(encoder));
  if (missing.length > 0) {
    fail(`FFmpeg is missing required encoder support for the media E2E pass: ${missing.join(', ')}.`);
  }


}

function runSelfTest() {
  const missing = checkToolchain('/definitely/not/rustchan-ffmpeg');
  assert(missing.some((message) => message.includes('not usable')), 'missing ffmpeg should be visible');

  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'rustchan-media-check-'));
  try {
    const broken = path.join(temp, 'ffmpeg-broken');
    fs.writeFileSync(broken, '#!/bin/sh\necho broken >&2\nexit 2\n');
    fs.chmodSync(broken, 0o755);
    const brokenErrors = checkToolchain(broken);
    assert(brokenErrors.some((message) => message.includes('not usable')), 'broken ffmpeg should be visible');

    const noCodecs = path.join(temp, 'ffmpeg-no-codecs');
    fs.writeFileSync(noCodecs, '#!/bin/sh\nif [ "$1" = "-version" ]; then exit 0; fi\nprintf "Encoders:\\n V..... png\\n"\n');
    fs.chmodSync(noCodecs, 0o755);
    const codecErrors = checkToolchain(noCodecs);
    assert(codecErrors.some((message) => message.includes('missing required encoder')), 'missing codecs should be visible');
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

function checkToolchain(ffmpeg) {
  const errors = [];
  if (!run(ffmpeg, ['-version']).ok) errors.push(`FFmpeg '${ffmpeg}' is not usable.`);
  const encoders = run(ffmpeg, ['-hide_banner', '-encoders']);
  if (encoders.ok) {
    const missing = REQUIRED_ENCODERS.filter((encoder) => !encoders.stdout.includes(encoder));
    if (missing.length > 0) errors.push(`FFmpeg is missing required encoder support: ${missing.join(', ')}.`);
  }
  return errors;
}

function run(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  return {
    ok: result.status === 0,
    stdout: result.stdout ?? '',
    detail: [result.error?.message, result.stdout, result.stderr].filter(Boolean).join('\n').trim(),
  };
}

function assert(condition, message) {
  if (!condition) fail(`media-toolchain-check self-test failed: ${message}`);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
