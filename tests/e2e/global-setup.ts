import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const repoRoot = path.resolve(__dirname, '../..');

export default async function globalSetup() {
  fs.mkdirSync(path.join(repoRoot, 'test-results/e2e'), { recursive: true });
  fs.mkdirSync(path.join(repoRoot, 'playwright-report'), { recursive: true });

  if (process.env.RUSTCHAN_E2E_SKIP_BUILD === '1' || process.env.RUSTCHAN_UPLOAD_BASE_URL) {
    return;
  }

  const result = spawnSync('cargo', ['build', '--bin', 'rustchan-cli'], {
    cwd: repoRoot,
    stdio: 'inherit',
    env: {
      ...process.env,
      CHAN_TOR_SUPPORT: '0',
      CHAN_AUTO_FULL_BACKUP_HOURS: '0',
      CHAN_AUTO_VACUUM_HOURS: '0',
      CHAN_WAL_CHECKPOINT_SECS: '0',
      CHAN_POLL_CLEANUP_HOURS: '0',
    },
  });

  if (result.status !== 0) {
    throw new Error(`cargo build --bin rustchan-cli failed with status ${result.status}`);
  }
}
