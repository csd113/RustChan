/**
 * Playwright #42768: macOS 27 TCC denies Gecko's shared Firefox app-data root,
 * independently of the temporary -profile directory. Until upstream rebrands
 * its bundle, use the SAME browser revision with a separate app identity.
 * -app must precede Playwright's arguments, hence the generated launcher.
 * https://github.com/microsoft/playwright/issues/42768
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createHash } from 'node:crypto';
import { firefox } from '@playwright/test';

const workDir = path.resolve(__dirname, '../../output/playwright/firefox-rebranded');

export function needsFirefoxIdentityIsolation(platform: string, kernelRelease: string, ini: string): boolean {
  return platform === 'darwin' && Number(kernelRelease.split('.')[0]) >= 27
    && /^Name=Firefox$/m.test(ini) && /^Vendor=Mozilla$/m.test(ini);
}

export function rebrandApplicationIni(contents: string): string {
  if (!/^Name=Firefox$/m.test(contents) || !/^Vendor=Mozilla$/m.test(contents)) {
    throw new Error('Firefox identity changed; re-evaluate the #42768 launch accommodation.');
  }
  return contents.replace(/^Name=Firefox$/m, 'Name=PlaywrightFirefox')
    .replace(/^Vendor=Mozilla$/m, 'Vendor=Playwright');
}

function shellQuote(value: string): string {
  return "'" + value.replace(/'/g, "'\\''") + "'";
}

/** Automatic on affected OS/bundle identities; stock launch everywhere else. */
export function rebrandedFirefoxExecutable(): string | undefined {
  if (process.platform !== 'darwin' || Number(os.release().split('.')[0]) < 27) return undefined;
  const executable = firefox.executablePath();
  const sourceBundle = path.resolve(path.dirname(executable), '../..');
  const sourceIni = path.join(sourceBundle, 'Contents/Resources/application.ini');
  // Let Playwright itself report a missing browser with its install instructions.
  if (!fs.existsSync(sourceIni)) return undefined;
  const ini = fs.readFileSync(sourceIni, 'utf8');
  if (!needsFirefoxIdentityIsolation(process.platform, os.release(), ini)) return undefined;
  // An upgrade must explicitly revisit this temporary upstream accommodation.
  const version = require('@playwright/test/package.json').version as string;
  const [major, minor] = version.split('.').map(Number);
  if (major > 1 || minor >= 64) {
    throw new Error(`Playwright ${version} still has the shared Firefox identity. Re-evaluate #42768 before extending the accommodation.`);
  }
  const key = createHash('sha256').update(`${executable}:${fs.statSync(executable).mtimeMs}:${ini}:v2`).digest('hex').slice(0, 20);
  const cache = path.join(workDir, key);
  const bundle = path.join(cache, 'Nightly.app');
  const browserIni = path.join(bundle, 'Contents/Resources/browser/application.ini');
  const launcher = path.join(cache, 'firefox.sh');
  const patched = rebrandApplicationIni(ini);
  const script = `#!/bin/sh\nexec ${shellQuote(path.join(bundle, 'Contents/MacOS/firefox'))} -app ${shellQuote(browserIni)} "$@"\n`;
  if (fs.existsSync(launcher)) {
    if (fs.readFileSync(launcher, 'utf8') !== script || fs.readFileSync(browserIni, 'utf8') !== patched) {
      throw new Error('Cached Firefox launcher/identity mismatch; remove output/playwright/firefox-rebranded and rerun.');
    }
    return launcher;
  }
  fs.mkdirSync(workDir, { recursive: true });
  const staging = fs.mkdtempSync(path.join(workDir, '.building-'));
  try {
    fs.cpSync(sourceBundle, path.join(staging, 'Nightly.app'), { recursive: true });
    fs.writeFileSync(path.join(staging, 'Nightly.app/Contents/Resources/browser/application.ini'), patched);
    fs.writeFileSync(path.join(staging, 'firefox.sh'), script, { mode: 0o755 });
    try {
      fs.renameSync(staging, cache);
    } catch (error) {
      // Another test process can publish the same complete cache first.
      if (!fs.existsSync(launcher)) throw error;
    }
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }
  return launcher;
}
