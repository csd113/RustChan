import { defineConfig, devices } from '@playwright/test';
import { rebrandedFirefoxExecutable } from './tests/e2e/firefox-rebrand';

// Automatic identity isolation for the macOS 27 / stock Gecko TCC defect.
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
      name: 'chromium-nojs',
      use: {
        ...devices['Desktop Chrome'],
        javaScriptEnabled: false,
      },
    },
  ],
});
