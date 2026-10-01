import base from '../../playwright.config';
import { defineConfig } from '@playwright/test';
import path from 'node:path';

export default defineConfig({
  ...base,
  testDir: '.',
  globalSetup: './global-setup.ts',
  testMatch: 'release-audit.spec.ts',
  grep: /natural multi-board/,
  workers: 1,
  outputDir: path.resolve(base.outputDir ?? 'test-results/native-cache'),
  reporter: [
    ['list'],
    ['html', { outputFolder: path.resolve(process.env.RUSTCHAN_AUDIT_OUTPUT ?? 'output/playwright/native-cache', 'html'), open: 'never' }],
    ['json', { outputFile: path.resolve(process.env.RUSTCHAN_AUDIT_OUTPUT ?? 'output/playwright/native-cache', 'results.json') }],
  ],
  use: { ...base.use, trace: 'off', video: 'off' },
  projects: base.projects?.filter(project => project.name !== 'firefox-nojs').map(project => ({
    ...project,
    use: {
      ...project.use,
      ...(project.name === 'chromium' ? { launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] } } : {}),
    },
  })),
});
