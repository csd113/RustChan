import { defineConfig, devices } from '@playwright/test';
import { rebrandedFirefoxExecutable } from './tests/e2e/firefox-rebrand';

// `RUSTCHAN_FIREFOX_REBRAND=1` opts into the documented macOS-only workaround
// for Playwright issue #42768 (see tests/e2e/firefox-rebrand.ts). Without it the
// Firefox projects use the stock launch path and fail loudly on affected hosts.
const firefoxExecutable = rebrandedFirefoxExecutable();
const firefoxLaunchOptions = firefoxExecutable
  ? { launchOptions: { executablePath: firefoxExecutable } }
  : {};

export default defineConfig({
  testDir: './tests/e2e',
  globalSetup: './tests/e2e/global-setup.ts',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 2,
  timeout: 90_000,
  expect: {
    timeout: 10_000,
  },
  outputDir: process.env.RUSTCHAN_AUDIT_OUTPUT ? `${process.env.RUSTCHAN_AUDIT_OUTPUT}/artifacts` : 'test-results/e2e/artifacts',
  reporter: [
    ['list'],
    ['html', { outputFolder: process.env.RUSTCHAN_AUDIT_OUTPUT ? `${process.env.RUSTCHAN_AUDIT_OUTPUT}/html` : 'playwright-report', open: 'never' }],
    ['json', { outputFile: process.env.RUSTCHAN_AUDIT_OUTPUT ? `${process.env.RUSTCHAN_AUDIT_OUTPUT}/results.json` : 'test-results/e2e/results.json' }],
  ],
  use: {
    actionTimeout: 15_000,
    navigationTimeout: 20_000,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
    {
      name: 'webkit',
      use: { ...devices['Desktop Safari'] },
    },
    {
      name: 'firefox',
      use: { ...devices['Desktop Firefox'], ...firefoxLaunchOptions },
    },
    {
      name: 'mobile-firefox',
      use: {
        browserName: 'firefox',
        viewport: { width: 390, height: 844 },
        deviceScaleFactor: 2,
        hasTouch: true,
        userAgent: 'Mozilla/5.0 (Android 14; Mobile; rv:120.0) Gecko/120.0 Firefox/120.0',
        ...firefoxLaunchOptions,
      },
    },
    {
      name: 'mobile-webkit',
      use: { ...devices['iPhone 13'] },
    },
    {
      name: 'firefox-nojs',
      use: {
        ...devices['Desktop Firefox'],
        javaScriptEnabled: false,
        ...firefoxLaunchOptions,
      },
    },
    {
      // Added by the 2026-09-29 deep audit. The configured `firefox-nojs`
      // project is the canonical no-JavaScript project, but the Playwright
      // Firefox build cannot create a profile on some macOS 27 hosts (upstream
      // Playwright issue #42768, TCC-protected app-data directory). The
      // documented opt-in workaround is RUSTCHAN_FIREFOX_REBRAND=1; without it,
      // `chromium-nojs` keeps the JavaScript-disabled contract covered on every
      // host. The `firefox-nojs` entry is intentionally left in place so the
      // recorded configuration contract is preserved and a blocked host stays
      // visible in run output.
      name: 'chromium-nojs',
      use: {
        ...devices['Desktop Chrome'],
        javaScriptEnabled: false,
      },
    },
  ],
});
