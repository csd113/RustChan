import { defineConfig, devices } from '@playwright/test';

// Restart tests own disposable processes and have no dependency on the media audit harness.
export default defineConfig({
  testDir: './tests/e2e',
  testMatch: 'settings-restart.spec.ts',
  workers: 1,
  timeout: 120_000,
  expect: { timeout: 15_000 },
  outputDir: 'test-results/restart',
  use: { ...devices['Desktop Chrome'], trace: 'retain-on-failure' },
  projects: [
    { name: 'chromium' },
    { name: 'chromium-nojs', use: { javaScriptEnabled: false } },
  ],
});
