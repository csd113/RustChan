import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import {
  adminCsrf,
  adminLogin,
  createThread,
  expect,
  expectSafePage,
  expectSafeResponse,
  test,
} from './helpers';

test.describe('phase 2 admin maintenance and backup progress', () => {
  test('DB check, repair status, progress polling, stale job protection, and concurrent maintenance blocking work through admin UI routes', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'maintenance progress coverage runs on Chromium first');

    await adminLogin(page, app);
    const loggedOut = await page.context().browser()!.newContext();
    const loggedOutPage = await loggedOut.newPage();
    const publicRepair = await loggedOutPage.request.get(`${app.baseURL}/admin/db/repair`, { maxRedirects: 0 });
    expect([302, 303, 403]).toContain(publicRepair.status());
    await loggedOut.close();

    await page.goto(`${app.baseURL}/admin/db/repair`);
    await expect(page.locator('body')).toContainText(/No maintenance rebuild is running/i);
    await expectSafePage(page);

    const check = await page.request.post(`${app.baseURL}/admin/db/check`, {
      form: { _csrf: await adminCsrf(page, app) },
      maxRedirects: 0,
    });
    expect(check.status()).toBe(200);
    const checkBody = await expectSafeResponse(check);
    expect(checkBody).toContain('Database health checks passed');
    expectNoHostPathLeak(checkBody, app);

    const protectedBackupRef = await createSavedFullBackup(page, app);
    await seedMaintenancePayload(app);
    await assertRepairBlockedDuringBackup(page, app, protectedBackupRef);

    const runningResponse = await page.request.post(`${app.baseURL}/admin/db/repair`, {
      form: { _csrf: await adminCsrf(page, app) },
      maxRedirects: 0,
    });
    expect(runningResponse.status()).toBe(200);
    const runningBody = await runningResponse.text();
    expect(runningBody).toContain('data-db-repair-progress');
    const jobId = Number(runningBody.match(/data-db-repair-job-id="(\d+)"/)?.[1]);
    expect(Number.isInteger(jobId) && jobId > 0).toBe(true);
    await waitForRepairRunning(page, app, jobId);

    const staleProgress = await page.request.get(`${app.baseURL}/admin/db/repair/progress?job_id=${jobId + 9999}`);
    expect(staleProgress.status()).toBe(200);
    const staleJson = await staleProgress.json();
    expect(staleJson.state).toBe('stale');
    expect(staleJson.done).toBe(true);

    const finalProgress = await waitForRepairDone(page, app, jobId);
    expect(['finished', 'failed', 'stale']).toContain(finalProgress.state);
    const status = await page.request.get(`${app.baseURL}/admin/db/repair/status?job_id=${jobId}`);
    expect(status.status()).toBe(200);
    const statusBody = await expectSafeResponse(status);
    expect(statusBody).toMatch(/Maintenance completed|Repair was not run|maintenance rebuild failed/i);
    expectNoHostPathLeak(statusBody.replace(/<strong>Backup path:<\/strong>[^<]+<code>[^<]+<\/code>/g, ''), app);

    const staleStatus = await page.request.get(`${app.baseURL}/admin/db/repair/status?job_id=${jobId + 9999}`);
    expect(staleStatus.status()).toBe(200);
    const staleStatusBody = await expectSafeResponse(staleStatus);
    expect(staleStatusBody).toContain('no longer the current status');
    expectNoHostPathLeak(staleStatusBody, app);
  });

  test('backup create, progress JSON, saved row, download, corrupt restore failure, cleanup, and delete are safe', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'backup UI/progress coverage runs on Chromium first');

    const sentinelThread = await createThread(page, app, 'pub', {
      subject: 'backup progress sentinel',
      body: 'live site should survive corrupt restore',
    });
    await adminLogin(page, app);
    const beforeRefs = await savedBackupRefs(app);
    const create = await page.request.post(`${app.baseURL}/admin/backup/create`, {
      form: {
        _csrf: await adminCsrf(page, app),
        storage_mode: 'split_zip',
        split_zip_part_size_gib: '4',
      },
      maxRedirects: 0,
    });
    expect(create.status()).toBe(303);
    const afterRefs = await savedBackupRefs(app);
    const createdRefs = afterRefs.filter((item) => !beforeRefs.includes(item));
    expect(createdRefs.length).toBe(1);
    const backupRef = createdRefs[0];

    const progress = await page.request.get(`${app.baseURL}/admin/backup/progress`);
    expect(progress.status()).toBe(200);
    const progressJson = await progress.json();
    expect(progressJson.phase).toBe(5);
    expect(progressJson.files_done).toBeGreaterThanOrEqual(0);

    await page.goto(`${app.baseURL}/admin/panel?open=full-backup-restore#full-backup-restore`);
    await expect(page.locator('#full-backup-restore')).toContainText(backupRef);
    await expectSafePage(page, { allowAdminInternals: true });

    const downloadPart = splitZipPartName(app, backupRef);
    const download = await page.request.get(`${app.baseURL}/admin/backup/download/full/${backupRef}?part=${downloadPart}`, {
      maxRedirects: 0,
    });
    expect(download.status()).toBe(200);
    expect(download.headers()['content-type']).toContain('application/zip');
    expect(download.headers()['content-disposition']).toContain('attachment');
    expect(Number(download.headers()['content-length'] ?? '0')).toBeGreaterThan(0);

    const corruptRestore = await page.request.post(`${app.baseURL}/admin/restore`, {
      multipart: {
        _csrf: await adminCsrf(page, app),
        backup_file: {
          name: 'corrupt.zip',
          mimeType: 'application/zip',
          buffer: Buffer.from('not a zip archive'),
        },
      },
      headers: {
        'X-Requested-With': 'XMLHttpRequest',
        ...adminSameOriginHeaders(app),
      },
      maxRedirects: 0,
    });
    expect([200, 400, 409, 422]).toContain(corruptRestore.status());
    const corruptBody = await expectSafeResponse(corruptRestore);
    expect(corruptBody).toMatch(/Invalid zip|restore/i);
    expectNoHostPathLeak(corruptBody, app);
    expect(await runtimeTmpFileCount(app)).toBe(0);

    await page.goto(`${app.baseURL}/pub/thread/${sentinelThread}`);
    await expect(page.locator('body')).toContainText('live site should survive corrupt restore');
    await expectSafePage(page);

    const deleteBackup = await page.request.post(`${app.baseURL}/admin/backup/delete`, {
      form: {
        _csrf: await adminCsrf(page, app),
        kind: 'full',
        filename: backupRef,
      },
      maxRedirects: 0,
    });
    expect(deleteBackup.status()).toBe(303);
    expect(fs.existsSync(path.join(app.dataDir, 'backups', backupRef))).toBe(false);
  });

  test('fault-injected repair failure and pre-repair backup failure remain manual e2e cases', async ({}, testInfo) => {
    test.skip(
      testInfo.project.name !== 'chromium',
      'manual note is attached to the Chromium phase-2 run only',
    );
    test.skip(true, 'normal e2e binary does not expose the cfg(test) pre-repair backup failure hook or a deterministic hard repair failure trigger');
  });
});

async function seedMaintenancePayload(app: { dataDir: string }): Promise<void> {
  const dir = path.join(app.dataDir, 'boards', 'pub', 'phase2-maintenance-payload');
  await fsp.mkdir(dir, { recursive: true });
  for (let i = 0; i < 96; i += 1) {
    const chunk = Buffer.alloc(512 * 1024);
    let state = 0x9e3779b9 ^ i;
    for (let offset = 0; offset < chunk.length; offset += 1) {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      chunk[offset] = state & 0xff;
    }
    await fsp.writeFile(path.join(dir, `payload-${i}.bin`), chunk);
  }
}

async function waitForRepairRunning(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
  jobId: number,
): Promise<void> {
  let last: { state: string; done: boolean } = { state: 'starting', done: false };
  await expect.poll(async () => {
    const response = await page.request.get(`${app.baseURL}/admin/db/repair/progress?job_id=${jobId}`);
    expect(response.status()).toBe(200);
    last = await response.json();
    return last.state === 'running' && !last.done ? 'running' : `${last.done ? 'done' : 'pending'}:${last.state}`;
  }, { timeout: 15_000, intervals: [100, 250, 500] }).toBe('running');
}

async function waitForRepairDone(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
  jobId: number,
): Promise<{ state: string; done: boolean }> {
  let last: { state: string; done: boolean } = { state: 'starting', done: false };
  await expect.poll(async () => {
    const response = await page.request.get(`${app.baseURL}/admin/db/repair/progress?job_id=${jobId}`);
    expect(response.status()).toBe(200);
    last = await response.json();
    return last.done;
  }, { timeout: 45_000, intervals: [250, 500, 1_000] }).toBe(true);
  return last;
}

async function assertRepairBlockedDuringBackup(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
  protectedBackupRef: string,
): Promise<void> {
  const createBackup = page.request.post(`${app.baseURL}/admin/backup/create`, {
    form: {
      _csrf: await adminCsrf(page, app),
      storage_mode: 'split_zip',
      split_zip_part_size_gib: '4',
    },
    maxRedirects: 0,
    timeout: 60_000,
  });
  await waitForBackupInProgress(page, app);
  const blockedRepair = await page.request.post(`${app.baseURL}/admin/db/repair`, {
    form: { _csrf: await adminCsrf(page, app) },
    maxRedirects: 0,
  });
  expect(blockedRepair.status()).toBe(409);
  const conflictBody = await expectSafeResponse(blockedRepair);
  expect(conflictBody).toContain('already running');
  expectNoHostPathLeak(conflictBody, app);

  const blockedDelete = await page.request.post(`${app.baseURL}/admin/backup/delete`, {
    form: {
      _csrf: await adminCsrf(page, app),
      kind: 'full',
      filename: protectedBackupRef,
    },
    maxRedirects: 0,
  });
  expect(blockedDelete.status()).toBe(409);
  const deleteConflictBody = await expectSafeResponse(blockedDelete);
  expect(deleteConflictBody).toContain('already running');
  expect(fs.existsSync(path.join(app.dataDir, 'backups', protectedBackupRef))).toBe(true);

  const blockedRestore = await page.request.post(`${app.baseURL}/admin/backup/restore-saved`, {
    form: {
      _csrf: await adminCsrf(page, app),
      filename: protectedBackupRef,
    },
    maxRedirects: 0,
  });
  expect(blockedRestore.status()).toBe(409);
  const restoreConflictBody = await expectSafeResponse(blockedRestore);
  expect(restoreConflictBody).toContain('already running');

  const created = await createBackup;
  expect(created.status()).toBe(303);
}

async function createSavedFullBackup(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
): Promise<string> {
  const beforeRefs = await savedBackupRefs(app);
  const response = await page.request.post(`${app.baseURL}/admin/backup/create`, {
    form: {
      _csrf: await adminCsrf(page, app),
      storage_mode: 'directory',
    },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  const createdRefs = (await savedBackupRefs(app)).filter((item) => !beforeRefs.includes(item));
  expect(createdRefs).toHaveLength(1);
  return createdRefs[0];
}

async function waitForBackupInProgress(
  page: Parameters<typeof adminCsrf>[0],
  app: Parameters<typeof adminCsrf>[1],
): Promise<void> {
  let lastPhase = 0;
  await expect.poll(async () => {
    const response = await page.request.get(`${app.baseURL}/admin/backup/progress`);
    expect(response.status()).toBe(200);
    const progress = await response.json() as { phase: number };
    lastPhase = progress.phase;
    return progress.phase > 0 && progress.phase < 5;
  }, {
    message: `backup should stay in progress for concurrency check; last phase ${lastPhase}`,
    timeout: 15_000,
    intervals: [100, 250, 500],
  }).toBe(true);
}

async function savedBackupRefs(app: { dataDir: string }): Promise<string[]> {
  const root = path.join(app.dataDir, 'backups');
  const entries = await fsp.readdir(root, { withFileTypes: true }).catch(() => []);
  return entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .filter((name) => name !== 'full' && name !== 'boards')
    .sort();
}

function splitZipPartName(app: { dataDir: string }, backupRef: string): string {
  const manifestPath = path.join(app.dataDir, 'backups', backupRef, 'manifest.json');
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
    parts?: Array<{ filename: string }>;
  };
  const part = manifest.parts?.[0]?.filename;
  expect(part).toMatch(/^parts\/part-\d{4}\.zip$/);
  return path.basename(part!);
}

function adminSameOriginHeaders(app: Parameters<typeof adminCsrf>[1]): Record<string, string> {
  return {
    Origin: app.baseURL,
    Referer: `${app.baseURL}/admin/panel`,
  };
}

async function runtimeTmpFileCount(app: { dataDir: string }): Promise<number> {
  const root = path.join(app.dataDir, 'runtime', 'tmp');
  let count = 0;
  async function walk(dir: string): Promise<void> {
    const entries = await fsp.readdir(dir, { withFileTypes: true }).catch(() => []);
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(fullPath);
      } else if (entry.isFile()) {
        count += 1;
      }
    }
  }
  await walk(root);
  return count;
}

function expectNoHostPathLeak(text: string, app: { dataDir: string }): void {
  expect(text).not.toContain('/Users/');
  expect(text).not.toContain(app.dataDir);
  expect(text).not.toContain('target/debug');
}
