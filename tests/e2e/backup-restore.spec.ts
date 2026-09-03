import { test, expect, type Page, type TestInfo } from '@playwright/test';
import { spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import zlib from 'node:zlib';
import {
  ADMIN_PASSWORD,
  ADMIN_USERNAME,
  adminCsrf,
  adminLogin,
  adminPasswordHash,
  createReply,
  createThread,
  expectSafePage,
  expectSafeResponse,
  RustChanServer,
  sqliteExec,
  sqliteQuery,
  unlockBoard,
  updateBoardSettings,
} from './helpers';

const GIB = 1024 * 1024 * 1024;
const SPLIT_SIZE_OPTIONS_GIB = [1, 2, 4, 8, 16, 32, 64] as const;
const TOR_ENV = {
  CHAN_TOR_SUPPORT: '1',
  CHAN_TOR_BOOTSTRAP_TIMEOUT: '1',
};

type BackupStorageMode = 'directory' | 'split_zip';

type BackupFileEntry = {
  logical_path: string;
  runtime_logical_path?: string;
  board?: string;
  kind: string;
  size: number;
  sha256: string;
  zip_part?: string;
  zip_entry_path?: string;
};

type BackupManifest = {
  format: string;
  backup_id: string;
  scope: string;
  storage_mode: BackupStorageMode;
  included_boards: Array<{ short_name: string; name: string }>;
  includes: {
    database: boolean;
    settings: boolean;
    uploads: boolean;
    thumbnails: boolean;
    tor_keys: boolean;
    board_exports: boolean;
    file_inventory: boolean;
  };
  db_snapshot?: { path: string; size: number; sha256: string };
  files: BackupFileEntry[];
  parts: Array<{
    filename: string;
    part_index: number;
    total_parts: number;
    size: number;
    sha256: string;
    target_part_size: number;
    oversized: boolean;
  }>;
};

type BackupMetadata = {
  format: string;
  backup_id: string;
  scope: string;
  storage_mode: BackupStorageMode;
  created_at: number;
  completed_at?: number;
  total_size_bytes: number;
  verified: boolean;
  part_count: number;
  includes_tor_keys: boolean;
};

type BackupArtifact = {
  ref: string;
  root: string;
  metadata: BackupMetadata;
  manifest: BackupManifest;
};

type RepresentativeSite = {
  pubThread: number;
  imgThread: number;
  protectedThread: number;
  adminHash: string;
  extraUploadRelativePath: string;
  globalFaviconLogicalPath: string;
  globalFaviconRuntimePath: string;
  globalBannerLogicalPath: string;
  globalBannerRuntimePath: string;
  customThemeSlug: string;
};

test.describe('live backup and restore coverage', () => {
  test('manual directory full backups restore complete site state into a fresh restarted runtime', async ({ page }, testInfo) => {
    const apps: RustChanServer[] = [];
    try {
      const source = await startApp(testInfo, { env: { CHAN_AUTO_FULL_BACKUP_COPIES: '20' } });
      apps.push(source);
      const site = await seedRepresentativeSite(page, source);

      const backup = await createFullBackupViaAdmin(page, source, { storageMode: 'directory' });
      assertFullBackupArtifact(backup, {
        storageMode: 'directory',
        includesTorKeys: false,
        requiredBoards: ['pub', 'img', 'sec'],
      });
      assertRepresentativeAssetsInBackup(backup, site);
      assertChecksumsValid(backup.root);

      const restored = await startApp(testInfo, { defaultData: false, admin: true });
      apps.push(restored);
      await copyBackupToApp(backup, restored);
      await restoreSavedFullBackup(page, restored, backup.ref);
      await restored.restart();

      await assertRestoredSiteUsable(page, restored, site);
    } finally {
      await stopApps(testInfo, apps);
    }
  });

  test('manual split full backups cover every exposed split-size option and require complete parts for restore', async ({ page }, testInfo) => {
    test.setTimeout(180_000);
    const apps: RustChanServer[] = [];
    try {
      const source = await startApp(testInfo, { env: { CHAN_AUTO_FULL_BACKUP_COPIES: '30' } });
      apps.push(source);
      const site = await seedRepresentativeSite(page, source);
      await assertSplitSizeOptionsExposed(page, source);

      const splitBackups: BackupArtifact[] = [];
      for (const splitSizeGib of SPLIT_SIZE_OPTIONS_GIB) {
        const backup = await createFullBackupViaAdmin(page, source, {
          storageMode: 'split_zip',
          splitSizeGib,
        });
        assertFullBackupArtifact(backup, {
          storageMode: 'split_zip',
          splitSizeGib,
          includesTorKeys: false,
          requiredBoards: ['pub', 'img', 'sec'],
        });
        assertChecksumsValid(backup.root);
        assertSplitPartsContainDeclaredEntries(backup);
        splitBackups.push(backup);
      }

      const targetBackup = splitBackups[0];
      const missingPartRestore = await startApp(testInfo, { defaultData: false, admin: true });
      apps.push(missingPartRestore);
      await copyBackupToApp(targetBackup, missingPartRestore);
      const firstPart = targetBackup.manifest.parts[0];
      expect(firstPart, 'split backup should have at least one declared part').toBeTruthy();
      await fsp.rm(path.join(backupRoot(missingPartRestore, targetBackup.ref), firstPart.filename));

      const failedRestore = await restoreSavedFullBackup(page, missingPartRestore, targetBackup.ref, {
        expectSuccess: false,
      });
      expect(decodedLocation(failedRestore)).toMatch(/missing|not found|Inspect split ZIP part/i);
      await page.goto(missingPartRestore.baseURL);
      await expectSafePage(page);
      expect(sqliteQuery(missingPartRestore, "SELECT COUNT(*) FROM boards WHERE short_name = 'pub';")).toBe('0');

      await fsp.rm(backupRoot(missingPartRestore, targetBackup.ref), { recursive: true, force: true });
      await copyBackupToApp(targetBackup, missingPartRestore);
      await restoreSavedFullBackup(page, missingPartRestore, targetBackup.ref);
      await missingPartRestore.restart();

      await assertRestoredSiteUsable(page, missingPartRestore, site);
    } finally {
      await stopApps(testInfo, apps);
    }
  });

  test('saved board backups restore one board without replacing the rest of the site', async ({ page }, testInfo) => {
    const apps: RustChanServer[] = [];
    try {
      const app = await startApp(testInfo, { env: { CHAN_AUTO_FULL_BACKUP_COPIES: '20' } });
      apps.push(app);
      await seedRepresentativeSite(page, app);
      const otherThread = await createThread(page, app, 'img', {
        subject: 'board restore unaffected board',
        body: 'this image board content should remain after board restore',
      });

      const boardBackup = await createBoardBackupViaAdmin(page, app, 'pub');
      assertBoardBackupArtifact(boardBackup, 'pub');
      assertChecksumsValid(boardBackup.root);

      const afterBackupThread = await createThread(page, app, 'pub', {
        subject: 'board restore should remove this',
        body: 'created after board backup',
      });
      await restoreSavedBoardBackup(page, app, boardBackup.ref);
      await app.restart();

      await page.goto(`${app.baseURL}/pub/catalog`);
      await expect(page.locator('body')).toContainText('backup public thread');
      await expect(page.locator('body')).not.toContainText('board restore should remove this');
      await expectSafePage(page);
      expect(sqliteQuery(app, `SELECT COUNT(*) FROM threads WHERE id = ${afterBackupThread};`)).toBe('0');

      await page.goto(`${app.baseURL}/img/thread/${otherThread}`);
      await expect(page.locator('body')).toContainText('this image board content should remain after board restore');
      await expectSafePage(page);
    } finally {
      await stopApps(testInfo, apps);
    }
  });

  test('automated full backups write scheduled artifacts with the configured storage mode', async ({ page }, testInfo) => {
    test.setTimeout(150_000);
    const apps: RustChanServer[] = [];
    try {
      const app = await startApp(testInfo, { env: { CHAN_AUTO_FULL_BACKUP_COPIES: '5' } });
      apps.push(app);
      await seedRepresentativeSite(page, app);

      const sentinel = await createFullBackupViaAdmin(page, app, { storageMode: 'directory' });
      ageBackupForScheduler(sentinel.root, 3 * 3600);
      await updateAutomaticBackupSettings(page, app, {
        intervalHours: 1,
        copiesToKeep: 5,
        storageMode: 'split_zip',
        splitSizeGib: 1,
      });

      const scheduledRef = await waitForNewBackupRef(app, new Set([sentinel.ref]), 90_000);
      const scheduled = readBackupArtifact(app, scheduledRef);
      assertFullBackupArtifact(scheduled, {
        storageMode: 'split_zip',
        splitSizeGib: 1,
        includesTorKeys: false,
        requiredBoards: ['pub', 'img', 'sec'],
      });
      assertChecksumsValid(scheduled.root);
    } finally {
      await stopApps(testInfo, apps);
    }
  });

  test('Tor hidden service keys are included, excluded, and restored only when explicitly requested', async ({ page }, testInfo) => {
    test.setTimeout(180_000);
    const apps: RustChanServer[] = [];
    try {
      const source = await startApp(testInfo, {
        env: { ...TOR_ENV, CHAN_AUTO_FULL_BACKUP_COPIES: '20' },
      });
      apps.push(source);
      await seedRepresentativeSite(page, source);
      await writeTorKeys(source, 'source');

      await adminLogin(page, source);
      await page.goto(`${source.baseURL}/admin/panel?open=full-backup-restore#full-backup-restore`);
      await expect(page.locator('#full-backup-create-form input[name="include_tor_hidden_service_keys"]')).toHaveCount(1);

      const withoutTor = await createFullBackupViaAdmin(page, source, {
        storageMode: 'directory',
        includeTorKeys: false,
      });
      assertFullBackupArtifact(withoutTor, {
        storageMode: 'directory',
        includesTorKeys: false,
        requiredBoards: ['pub', 'img', 'sec'],
      });
      assertNoTorKeysInBackup(withoutTor);

      const withTor = await createFullBackupViaAdmin(page, source, {
        storageMode: 'directory',
        includeTorKeys: true,
      });
      assertFullBackupArtifact(withTor, {
        storageMode: 'directory',
        includesTorKeys: true,
        requiredBoards: ['pub', 'img', 'sec'],
      });
      assertTorKeysInBackup(withTor);
      assertChecksumsValid(withTor.root);

      const restored = await startApp(testInfo, {
        defaultData: false,
        admin: true,
        env: TOR_ENV,
      });
      apps.push(restored);
      await writeTorKeys(restored, 'current');
      const currentKeys = await readTorKeys(restored);
      await copyBackupToApp(withoutTor, restored);
      const noKeyFailure = await restoreSavedFullBackup(page, restored, withoutTor.ref, {
        restoreTorKeys: true,
        expectSuccess: false,
      });
      expect(decodedLocation(noKeyFailure)).toContain('does not include Tor hidden service keys');
      expect(await readTorKeys(restored)).toEqual(currentKeys);

      await copyBackupToApp(withTor, restored);
      await restoreSavedFullBackup(page, restored, withTor.ref);
      expect(await readTorKeys(restored)).toEqual(currentKeys);

      await restoreSavedFullBackup(page, restored, withTor.ref, { restoreTorKeys: true });
      const restoredKeys = await readTorKeys(restored);
      expect(restoredKeys).toEqual(await readTorKeys(source));
      assertTorKeyPathsStayInsideRuntime(restored);
    } finally {
      await stopApps(testInfo, apps);
    }
  });

  test('restore rejects invalid archives and leaves the live site usable', async ({ page }, testInfo) => {
    const apps: RustChanServer[] = [];
    try {
      const app = await startApp(testInfo);
      apps.push(app);
      const threadId = await createThread(page, app, 'pub', {
        subject: 'restore failure baseline',
        body: 'this should remain after failed restore',
      });
      await adminLogin(page, app);
      const csrf = await adminCsrf(page, app);
      const response = await page.request.post(`${app.baseURL}/admin/restore`, {
        multipart: {
          _csrf: csrf,
          backup_file: {
            name: 'not-a-backup.zip',
            mimeType: 'application/zip',
            buffer: Buffer.from('not a zip with ../traversal'),
          },
        },
        headers: { Origin: app.baseURL },
        maxRedirects: 0,
      });
      expect([200, 302, 303, 400]).toContain(response.status());
      if (response.status() === 200 || response.status() === 400) {
        await expectSafeResponse(response);
      }
      await page.goto(`${app.baseURL}/pub/thread/${threadId}`);
      await expect(page.locator('body')).toContainText('this should remain after failed restore');
      await expectSafePage(page);
    } finally {
      await stopApps(testInfo, apps);
    }
  });
});

async function startApp(
  testInfo: TestInfo,
  options: {
    defaultData?: boolean;
    admin?: boolean;
    env?: Record<string, string>;
  } = {},
): Promise<RustChanServer> {
  const app = await RustChanServer.create(undefined, { env: options.env ?? {} });
  try {
    if (options.defaultData !== false) {
      await app.initializeDefaultData();
    } else if (options.admin) {
      app.runCli(['admin', 'create-admin', ADMIN_USERNAME, ADMIN_PASSWORD]);
    }
    await app.start();
    testInfo.annotations.push({ type: 'rustchan-root', description: app.rootDir });
    return app;
  } catch (error) {
    await app.dispose().catch(() => undefined);
    throw error;
  }
}

async function stopApps(testInfo: TestInfo, apps: RustChanServer[]): Promise<void> {
  for (const app of apps.reverse()) {
    if (testInfo.status !== testInfo.expectedStatus) {
      await testInfo.attach(`rustchan-${app.port}.log`, {
        body: await app.logs(),
        contentType: 'text/plain',
      });
    }
    await app.dispose();
  }
}

async function seedRepresentativeSite(page: Page, app: RustChanServer): Promise<RepresentativeSite> {
  app.createBoardCli({ short: 'sec', name: 'Secure Board', description: 'Password-protected board' });
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  const siteSettings = await page.request.post(`${app.baseURL}/admin/site/settings`, {
    form: {
      _csrf: csrf,
      site_name: 'RustChan Backup E2E',
      site_subtitle: 'restored backup subtitle',
      default_theme: 'blue-sky',
      homepage_new_thread_badges_enabled: '1',
      homepage_new_reply_badges_enabled: '1',
      thread_new_reply_badges_enabled: '1',
      banner_rotation_interval_minutes: '5',
    },
    maxRedirects: 0,
  });
  expect(siteSettings.status()).toBe(303);
  await updateBoardSettings(page, app, 'sec', {
    name: 'Secure Board',
    description: 'Password-protected board',
    accessMode: 'view_password',
    accessPassword: 'secret-passphrase',
    allowImages: true,
  });

  const files = app.fixtures();
  const pubThread = await createThread(page, app, 'pub', {
    subject: 'backup public thread',
    body: 'public op body before backup',
  });
  await createReply(page, app, 'pub', pubThread, 'public reply before backup');
  const imgThread = await createThread(page, app, 'img', {
    subject: 'backup image thread',
    body: 'image op body before backup',
    filePath: files.tinyPng,
  });
  await createReply(page, app, 'img', imgThread, 'image reply before backup');
  const protectedThread = await createThread(page, app, 'sec', {
    subject: 'backup protected thread',
    body: 'protected board body before backup',
  });
  await createReply(page, app, 'sec', protectedThread, 'protected reply before backup');

  const extraUploadRelativePath = 'img/e2e-extra-split-payload.png';
  await writeDeterministicUpload(app, extraUploadRelativePath, 2 * 1024 * 1024);
  const siteAssets = await seedGlobalSiteAssets(page, app);
  const customThemeSlug = 'backup-custom';
  sqliteExec(app, [
    "INSERT INTO themes (slug, display_name, description, swatch_hex, enabled, sort_order, is_builtin, custom_css)",
    "VALUES ('backup-custom', 'Backup Custom Theme', 'theme restored by backup smoke', '#336699', 1, 9999, 0, 'body { --backup-theme-smoke: #336699; }')",
    "ON CONFLICT(slug) DO UPDATE SET display_name = excluded.display_name, custom_css = excluded.custom_css, enabled = 1;",
    "INSERT INTO site_settings (key, value) VALUES ('default_theme', 'backup-custom')",
    "ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
  ].join(' '));
  await expectMediaFilesExist(app);

  return {
    pubThread,
    imgThread,
    protectedThread,
    adminHash: adminPasswordHash(app),
    extraUploadRelativePath,
    globalFaviconLogicalPath: siteAssets.globalFaviconLogicalPath,
    globalFaviconRuntimePath: siteAssets.globalFaviconRuntimePath,
    globalBannerLogicalPath: siteAssets.globalBannerLogicalPath,
    globalBannerRuntimePath: siteAssets.globalBannerRuntimePath,
    customThemeSlug,
  };
}

async function seedGlobalSiteAssets(page: Page, app: RustChanServer): Promise<{
  globalFaviconLogicalPath: string;
  globalFaviconRuntimePath: string;
  globalBannerLogicalPath: string;
  globalBannerRuntimePath: string;
}> {
  const assetDir = path.join(app.rootDir, 'site-asset-fixtures');
  await fsp.mkdir(assetDir, { recursive: true });
  const faviconPath = path.join(assetDir, 'e2e-favicon.png');
  const bannerPath = path.join(assetDir, 'e2e-banner.png');
  await fsp.writeFile(faviconPath, pngRgba(512, 512, (x, y) => [x % 256, y % 256, 96, 255]));
  await fsp.writeFile(bannerPath, pngRgba(468, 60, (x, y) => [40 + (x % 90), 80 + (y % 90), 150, 255]));
  const sameOriginHeaders = {
    Origin: app.baseURL,
    Referer: `${app.baseURL}/admin/panel`,
  };

  const faviconResponse = await page.request.post(`${app.baseURL}/admin/site/favicon`, {
    multipart: {
      _csrf: await adminCsrf(page, app),
      favicon: {
        name: 'e2e-favicon.png',
        mimeType: 'image/png',
        buffer: await fsp.readFile(faviconPath),
      },
    },
    headers: sameOriginHeaders,
    maxRedirects: 0,
  });
  expect(faviconResponse.status()).toBe(303);
  const globalFaviconRuntimePath = 'favicon-32x32.png';
  expect(fs.existsSync(path.join(app.dataDir, 'runtime', 'favicon', globalFaviconRuntimePath))).toBe(true);

  const bannerResponse = await page.request.post(`${app.baseURL}/admin/site/banner`, {
    multipart: {
      _csrf: await adminCsrf(page, app),
      target_type: 'none',
      enabled: '1',
      show_on_index: '1',
      show_on_catalog: '1',
      banner: {
        name: 'e2e-banner.png',
        mimeType: 'image/png',
        buffer: await fsp.readFile(bannerPath),
      },
    },
    headers: sameOriginHeaders,
    maxRedirects: 0,
  });
  expect(bannerResponse.status()).toBe(303);
  const bannerStorageKey = sqliteQuery(
    app,
    "SELECT storage_key FROM banner_assets WHERE scope_type = 'global' ORDER BY id DESC LIMIT 1;",
  ).trim();
  expect(bannerStorageKey).toMatch(/^[0-9a-f]{32}$/);
  const globalBannerRuntimePath = `global/${bannerStorageKey}.webp`;
  expect(fs.existsSync(path.join(app.dataDir, 'runtime', 'banner', globalBannerRuntimePath))).toBe(true);

  return {
    globalFaviconLogicalPath: `site-assets/favicon/${globalFaviconRuntimePath}`,
    globalFaviconRuntimePath,
    globalBannerLogicalPath: `site-assets/banner/${globalBannerRuntimePath}`,
    globalBannerRuntimePath,
  };
}

async function writeDeterministicUpload(app: RustChanServer, relativePath: string, bytes: number): Promise<void> {
  const target = path.join(app.dataDir, 'boards', relativePath);
  await fsp.mkdir(path.dirname(target), { recursive: true });
  const buf = Buffer.alloc(bytes);
  for (let index = 0; index < buf.length; index += 1) {
    buf[index] = (index * 31 + 17) % 256;
  }
  await fsp.writeFile(target, buf);
}

async function expectMediaFilesExist(app: RustChanServer): Promise<void> {
  await expect.poll(() => {
    const count = Number(sqliteQuery(app, 'SELECT COUNT(*) FROM posts WHERE file_path IS NOT NULL;'));
    return count;
  }, { timeout: 10_000 }).toBeGreaterThan(0);

  const paths = sqliteQuery(
    app,
    "SELECT file_path FROM posts WHERE file_path IS NOT NULL UNION SELECT thumb_path FROM posts WHERE thumb_path IS NOT NULL;",
  ).split('\n').filter(Boolean);
  expect(paths.length).toBeGreaterThan(0);
  for (const rel of paths) {
    expect(fs.existsSync(path.join(app.dataDir, 'boards', rel)), `restored media path ${rel}`).toBe(true);
  }
}

async function assertRestoredSiteUsable(page: Page, app: RustChanServer, site: RepresentativeSite): Promise<void> {
  expect(sqliteQuery(app, "SELECT value FROM site_settings WHERE key = 'site_name';")).toBe('RustChan Backup E2E');
  expect(sqliteQuery(app, "SELECT value FROM site_settings WHERE key = 'site_subtitle';")).toBe('restored backup subtitle');
  expect(sqliteQuery(app, "SELECT access_mode FROM boards WHERE short_name = 'sec';")).toBe('view_password');
  expect(sqliteQuery(app, "SELECT COUNT(*) FROM boards WHERE short_name IN ('pub', 'img', 'sec');")).toBe('3');
  expect(adminPasswordHash(app)).toBe(site.adminHash);
  await expectMediaFilesExist(app);
  expect(fs.existsSync(path.join(app.dataDir, 'boards', site.extraUploadRelativePath))).toBe(true);
  expect(fs.existsSync(path.join(app.dataDir, 'runtime', 'favicon', site.globalFaviconRuntimePath))).toBe(true);
  expect(fs.existsSync(path.join(app.dataDir, 'runtime', 'banner', site.globalBannerRuntimePath))).toBe(true);
  expect(sqliteQuery(app, `SELECT display_name FROM themes WHERE slug = '${site.customThemeSlug}';`)).toBe('Backup Custom Theme');

  await page.context().clearCookies();
  await page.goto(app.baseURL);
  await expect(page.locator('body')).toContainText('RustChan Backup E2E');
  await expect(page.locator('body')).toContainText('restored backup subtitle');
  await expect(page.locator('html')).toHaveAttribute('data-active-theme', site.customThemeSlug);
  await expectSafePage(page);

  const customThemeCss = await page.request.get(`${app.baseURL}/theme-css/${site.customThemeSlug}`);
  expect(customThemeCss.status()).toBe(200);
  expect(await customThemeCss.text()).toContain('--backup-theme-smoke');

  await page.goto(`${app.baseURL}/pub/thread/${site.pubThread}`);
  await expect(page.locator('body')).toContainText('backup public thread');
  await expect(page.locator('body')).toContainText('public reply before backup');
  await expectSafePage(page);

  await page.goto(`${app.baseURL}/img/thread/${site.imgThread}`);
  await expect(page.locator('body')).toContainText('backup image thread');
  await expect(page.locator('body')).toContainText('image reply before backup');
  expect(await page.locator('a[href*="/img/"], img').count()).toBeGreaterThan(0);
  await expectSafePage(page);

  await page.context().clearCookies();
  await page.goto(`${app.baseURL}/sec`);
  await expect(page.locator('body')).toContainText(/password|unlock/i);
  await unlockBoard(page, app, 'sec', 'secret-passphrase');
  await page.goto(`${app.baseURL}/sec/thread/${site.protectedThread}`);
  await expect(page.locator('body')).toContainText('backup protected thread');
  await expect(page.locator('body')).toContainText('protected reply before backup');
  await expectSafePage(page);

  await adminLogin(page, app);
  await expect(page.locator('body')).toContainText('RustChan Backup E2E');
}

async function assertSplitSizeOptionsExposed(page: Page, app: RustChanServer): Promise<void> {
  await adminLogin(page, app);
  await page.goto(`${app.baseURL}/admin/panel?open=full-backup-restore#full-backup-restore`);
  const expected = SPLIT_SIZE_OPTIONS_GIB.map(String);
  const manual = await page
    .locator('#full-backup-create-form select[name="split_zip_part_size_gib"] option')
    .evaluateAll((options) => options.map((option) => (option as HTMLOptionElement).value));
  const automatic = await page
    .locator('form.full-backup-settings-form select[name="auto_full_backup_split_zip_part_size_gib"] option')
    .evaluateAll((options) => options.map((option) => (option as HTMLOptionElement).value));
  expect(manual).toEqual(expected);
  expect(automatic).toEqual(expected);
}

async function createFullBackupViaAdmin(
  page: Page,
  app: RustChanServer,
  options: {
    storageMode: BackupStorageMode;
    splitSizeGib?: number;
    includeTorKeys?: boolean;
  },
): Promise<BackupArtifact> {
  await adminLogin(page, app);
  const before = new Set(await listBackupRefs(app));
  const csrf = await adminCsrf(page, app);
  const form: Record<string, string> = {
    _csrf: csrf,
    storage_mode: options.storageMode,
  };
  if (options.storageMode === 'split_zip') {
    form.split_zip_part_size_gib = String(options.splitSizeGib ?? 4);
  }
  if (options.includeTorKeys) {
    form.include_tor_hidden_service_keys = '1';
  }
  const response = await page.request.post(`${app.baseURL}/admin/backup/create`, {
    form,
    headers: { Origin: app.baseURL },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  const ref = await waitForNewBackupRef(app, before, 10_000);
  return readBackupArtifact(app, ref);
}

async function createBoardBackupViaAdmin(page: Page, app: RustChanServer, board: string): Promise<BackupArtifact> {
  await adminLogin(page, app);
  const before = new Set(await listBackupRefs(app));
  const csrf = await adminCsrf(page, app);
  const response = await page.request.post(`${app.baseURL}/admin/board/backup/create`, {
    form: {
      _csrf: csrf,
      board_short: board,
    },
    headers: { Origin: app.baseURL },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  const ref = await waitForNewBackupRef(app, before, 10_000);
  return readBackupArtifact(app, ref);
}

async function updateAutomaticBackupSettings(
  page: Page,
  app: RustChanServer,
  settings: {
    intervalHours: number;
    copiesToKeep: number;
    storageMode: BackupStorageMode;
    splitSizeGib: number;
    includeTorKeys?: boolean;
  },
): Promise<void> {
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  const response = await page.request.post(`${app.baseURL}/admin/backup/settings`, {
    form: {
      _csrf: csrf,
      auto_full_backup_interval_hours: String(settings.intervalHours),
      auto_full_backup_copies_to_keep: String(settings.copiesToKeep),
      auto_full_backup_storage_mode: settings.storageMode,
      auto_full_backup_split_zip_part_size_gib: String(settings.splitSizeGib),
      ...(settings.includeTorKeys ? { auto_full_backup_include_tor_hidden_service_keys: '1' } : {}),
    },
    headers: { Origin: app.baseURL },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
}

async function restoreSavedFullBackup(
  page: Page,
  app: RustChanServer,
  backupRef: string,
  options: { restoreTorKeys?: boolean; expectSuccess?: boolean } = {},
): Promise<string> {
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  const response = await page.request.post(`${app.baseURL}/admin/backup/restore-saved`, {
    form: {
      _csrf: csrf,
      filename: backupRef,
      ...(options.restoreTorKeys ? { restore_tor_hidden_service_keys: '1' } : {}),
    },
    headers: { Origin: app.baseURL },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  const location = response.headers().location ?? '';
  if (options.expectSuccess === false) {
    expect(location).toContain('restore_error=');
    return location;
  }
  expect(location).toContain('restored=1');
  return location;
}

async function restoreSavedBoardBackup(page: Page, app: RustChanServer, backupRef: string): Promise<void> {
  await adminLogin(page, app);
  const csrf = await adminCsrf(page, app);
  const response = await page.request.post(`${app.baseURL}/admin/board/backup/restore-saved`, {
    form: {
      _csrf: csrf,
      filename: backupRef,
    },
    headers: { Origin: app.baseURL },
    maxRedirects: 0,
  });
  expect(response.status()).toBe(303);
  expect(response.headers().location ?? '').toContain('open=board-backup-restore');
}

async function listBackupRefs(app: RustChanServer): Promise<string[]> {
  const root = path.join(app.dataDir, 'backups');
  const entries = await fsp.readdir(root, { withFileTypes: true }).catch(() => []);
  return entries
    .filter((entry) => entry.isDirectory() && fs.existsSync(path.join(root, entry.name, 'backup.json')))
    .map((entry) => entry.name)
    .sort();
}

async function waitForNewBackupRef(app: RustChanServer, before: Set<string>, timeout: number): Promise<string> {
  let latest: string | undefined;
  await expect.poll(async () => {
    const refs = await listBackupRefs(app);
    const created = refs.filter((ref) => !before.has(ref));
    latest = created.sort().at(-1);
    return created.length;
  }, { timeout, intervals: [250, 500, 1_000, 2_000] }).toBeGreaterThan(0);
  if (!latest) {
    throw new Error('new backup was not found');
  }
  return latest;
}

function backupRoot(app: RustChanServer, backupRef: string): string {
  return path.join(app.dataDir, 'backups', backupRef);
}

function readBackupArtifact(app: RustChanServer, backupRef: string): BackupArtifact {
  const root = backupRoot(app, backupRef);
  return {
    ref: backupRef,
    root,
    metadata: readJson<BackupMetadata>(path.join(root, 'backup.json')),
    manifest: readJson<BackupManifest>(path.join(root, 'manifest.json')),
  };
}

async function copyBackupToApp(backup: BackupArtifact, app: RustChanServer): Promise<void> {
  const destination = backupRoot(app, backup.ref);
  await fsp.rm(destination, { recursive: true, force: true });
  await fsp.mkdir(path.dirname(destination), { recursive: true });
  await fsp.cp(backup.root, destination, { recursive: true });
}

function assertFullBackupArtifact(
  backup: BackupArtifact,
  expected: {
    storageMode: BackupStorageMode;
    splitSizeGib?: number;
    includesTorKeys: boolean;
    requiredBoards: string[];
  },
): void {
  expect(backup.metadata.format).toBe('rustchan-backup-v4');
  expect(backup.metadata.backup_id).toBe(backup.ref);
  expect(backup.metadata.scope).toBe('full_site');
  expect(backup.metadata.storage_mode).toBe(expected.storageMode);
  expect(backup.metadata.verified).toBe(true);
  expect(backup.metadata.includes_tor_keys).toBe(expected.includesTorKeys);
  expect(backup.manifest.format).toBe('rustchan-backup-v4');
  expect(backup.manifest.backup_id).toBe(backup.ref);
  expect(backup.manifest.scope).toBe('full_site');
  expect(backup.manifest.storage_mode).toBe(expected.storageMode);
  expect(backup.manifest.includes.database).toBe(true);
  expect(backup.manifest.includes.settings).toBe(true);
  expect(backup.manifest.includes.uploads).toBe(true);
  expect(backup.manifest.includes.thumbnails).toBe(true);
  expect(backup.manifest.includes.board_exports).toBe(true);
  expect(backup.manifest.includes.tor_keys).toBe(expected.includesTorKeys);
  expect(backup.manifest.db_snapshot?.path).toBe('db/rustchan.sqlite3');
  for (const board of expected.requiredBoards) {
    expect(backup.manifest.included_boards.some((entry) => entry.short_name === board), `board /${board}/ in backup`).toBe(true);
  }
  assertManifestFilesPresentOrZipped(backup);
  if (expected.storageMode === 'directory') {
    expect(backup.manifest.parts).toHaveLength(0);
    expect(backup.metadata.part_count).toBe(0);
  } else {
    expect(backup.manifest.parts.length).toBeGreaterThan(0);
    expect(backup.metadata.part_count).toBe(backup.manifest.parts.length);
    for (const part of backup.manifest.parts) {
      expect(part.target_part_size).toBe((expected.splitSizeGib ?? 4) * GIB);
      expect(fs.existsSync(path.join(backup.root, part.filename))).toBe(true);
      expect(sha256File(path.join(backup.root, part.filename))).toBe(part.sha256);
    }
  }
}

function assertRepresentativeAssetsInBackup(backup: BackupArtifact, site: RepresentativeSite): void {
  const settings = backup.manifest.files.find((entry) => entry.logical_path === 'config/settings.toml');
  expect(settings?.kind).toBe('settings');
  for (const [logicalPath, kind] of [
    [site.globalFaviconLogicalPath, 'favicon'],
    [site.globalBannerLogicalPath, 'banner'],
  ] as const) {
    const entry = backup.manifest.files.find((candidate) => candidate.logical_path === logicalPath);
    expect(entry, `${logicalPath} should be included in full backup`).toBeTruthy();
    expect(entry?.kind).toBe(kind);
    if (backup.manifest.storage_mode === 'directory') {
      expect(fs.existsSync(path.join(backup.root, logicalPath)), `${logicalPath} payload`).toBe(true);
    }
  }
  expect(backup.manifest.files.some((entry) => entry.logical_path.startsWith('boards/img/media/src/')), 'uploaded media should be inventoried').toBe(true);
}

function assertBoardBackupArtifact(backup: BackupArtifact, board: string): void {
  expect(backup.metadata.format).toBe('rustchan-backup-v4');
  expect(backup.metadata.scope).toBe('board');
  expect(backup.metadata.storage_mode).toBe('directory');
  expect(backup.metadata.includes_tor_keys).toBe(false);
  expect(backup.manifest.scope).toBe('board');
  expect(backup.manifest.storage_mode).toBe('directory');
  expect(backup.manifest.includes.database).toBe(false);
  expect(backup.manifest.includes.uploads).toBe(true);
  expect(backup.manifest.included_boards.map((entry) => entry.short_name)).toEqual([board]);
  expect(backup.manifest.files.every((entry) => entry.board === board || entry.logical_path.includes(`/${board}/`))).toBe(true);
  assertManifestFilesPresentOrZipped(backup);
}

function assertManifestFilesPresentOrZipped(backup: BackupArtifact): void {
  for (const entry of backup.manifest.files) {
    expect(entry.logical_path).not.toContain('..');
    expect(entry.logical_path.startsWith('/')).toBe(false);
    if (backup.manifest.storage_mode === 'directory') {
      const filePath = path.join(backup.root, entry.logical_path);
      expect(fs.existsSync(filePath), `backup file ${entry.logical_path}`).toBe(true);
      expect(sha256File(filePath), `sha256 for ${entry.logical_path}`).toBe(entry.sha256);
      expect(entry.zip_part).toBeUndefined();
    } else {
      expect(entry.zip_part, `zip part for ${entry.logical_path}`).toMatch(/^parts\/part-\d{4}\.zip$/);
      expect(entry.zip_entry_path).toBe(entry.logical_path);
    }
  }
}

function assertSplitPartsContainDeclaredEntries(backup: BackupArtifact): void {
  const entriesByPart = new Map<string, string[]>();
  for (const entry of backup.manifest.files) {
    const part = entry.zip_part;
    expect(part).toBeTruthy();
    const list = entriesByPart.get(part!) ?? [];
    list.push(entry.zip_entry_path ?? entry.logical_path);
    entriesByPart.set(part!, list);
  }
  for (const part of backup.manifest.parts) {
    const zipPath = path.join(backup.root, part.filename);
    const zipEntries = zipEntryNames(zipPath);
    for (const expectedEntry of entriesByPart.get(part.filename) ?? []) {
      expect(zipEntries, `${part.filename} contains ${expectedEntry}`).toContain(expectedEntry);
    }
  }
}

function assertChecksumsValid(root: string): void {
  const checksumsPath = path.join(root, 'checksums.sha256');
  expect(fs.existsSync(checksumsPath)).toBe(true);
  const lines = fs.readFileSync(checksumsPath, 'utf8').split(/\r?\n/).filter(Boolean);
  expect(lines.length).toBeGreaterThanOrEqual(3);
  for (const line of lines) {
    const match = line.match(/^([a-f0-9]{64})  (.+)$/);
    expect(match, `checksum line ${line}`).toBeTruthy();
    const [, expectedHash, relativePath] = match!;
    expect(relativePath).not.toBe('checksums.sha256');
    const filePath = path.join(root, relativePath);
    expect(fs.existsSync(filePath), `checksum target ${relativePath}`).toBe(true);
    expect(sha256File(filePath), `checksum for ${relativePath}`).toBe(expectedHash);
  }
}

function pngRgba(width: number, height: number, pixel: (x: number, y: number) => [number, number, number, number]): Buffer {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y += 1) {
    const rowStart = y * (width * 4 + 1);
    raw[rowStart] = 0;
    for (let x = 0; x < width; x += 1) {
      const [r, g, b, a] = pixel(x, y);
      const offset = rowStart + 1 + x * 4;
      raw[offset] = r;
      raw[offset + 1] = g;
      raw[offset + 2] = b;
      raw[offset + 3] = a;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  return Buffer.concat([
    Buffer.from('\x89PNG\r\n\x1a\n', 'binary'),
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', zlib.deflateSync(raw)),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

function pngChunk(type: string, data: Buffer): Buffer {
  const typeBuffer = Buffer.from(type, 'ascii');
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuffer, data])), 0);
  return Buffer.concat([length, typeBuffer, data, crc]);
}

function crc32(buffer: Buffer): number {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function assertNoTorKeysInBackup(backup: BackupArtifact): void {
  expect(backup.manifest.files.some((entry) => entry.kind === 'tor_key')).toBe(false);
  expect(fs.existsSync(path.join(backup.root, 'tor-keys'))).toBe(false);
}

function assertTorKeysInBackup(backup: BackupArtifact): void {
  const torEntries = backup.manifest.files.filter((entry) => entry.kind === 'tor_key');
  expect(torEntries.length).toBeGreaterThanOrEqual(2);
  for (const entry of torEntries) {
    expect(entry.logical_path).toMatch(/^tor-keys\//);
    expect(entry.logical_path).not.toContain('..');
  }
}

function ageBackupForScheduler(root: string, ageSeconds: number): void {
  const now = Math.floor(Date.now() / 1000);
  const createdAt = now - ageSeconds - 60;
  const completedAt = now - ageSeconds;
  const manifestPath = path.join(root, 'manifest.json');
  const metadataPath = path.join(root, 'backup.json');
  const manifest = readJson<BackupManifest & { created_at: number; completed_at?: number }>(manifestPath);
  const metadata = readJson<BackupMetadata>(metadataPath);
  manifest.created_at = createdAt;
  manifest.completed_at = completedAt;
  metadata.created_at = createdAt;
  metadata.completed_at = completedAt;
  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  fs.writeFileSync(metadataPath, `${JSON.stringify(metadata, null, 2)}\n`);
}

async function writeTorKeys(app: RustChanServer, label: string): Promise<void> {
  const dir = torKeyDir(app);
  await fsp.mkdir(dir, { recursive: true });
  await fsp.writeFile(path.join(dir, 'hs_ed25519_secret_key'), `${label}-secret-key\n`, { mode: 0o600 });
  await fsp.writeFile(path.join(dir, 'hs_ed25519_public_key'), `${label}-public-key\n`, { mode: 0o600 });
  await fsp.mkdir(path.join(dir, 'nested'), { recursive: true });
  await fsp.writeFile(path.join(dir, 'nested', 'authorized_clients'), `${label}-client\n`, { mode: 0o600 });
}

async function readTorKeys(app: RustChanServer): Promise<Record<string, string>> {
  const dir = torKeyDir(app);
  const files = await listFilesRecursive(dir);
  const result: Record<string, string> = {};
  for (const file of files) {
    const rel = path.relative(dir, file).split(path.sep).join('/');
    result[rel] = await fsp.readFile(file, 'utf8');
  }
  return result;
}

function assertTorKeyPathsStayInsideRuntime(app: RustChanServer): void {
  const dir = torKeyDir(app);
  for (const file of fs.readdirSync(dir, { recursive: true })) {
    const fullPath = path.resolve(dir, String(file));
    expect(fullPath.startsWith(path.resolve(dir))).toBe(true);
    expect(fullPath).not.toContain(`..${path.sep}`);
  }
}

function torKeyDir(app: RustChanServer): string {
  return path.join(app.dataDir, 'runtime', 'tor', 'state', 'keystore');
}

async function listFilesRecursive(root: string): Promise<string[]> {
  const entries = await fsp.readdir(root, { withFileTypes: true }).catch(() => []);
  const files: string[] = [];
  for (const entry of entries) {
    const fullPath = path.join(root, entry.name);
    if (entry.isDirectory()) {
      files.push(...await listFilesRecursive(fullPath));
    } else if (entry.isFile()) {
      files.push(fullPath);
    }
  }
  return files.sort();
}

function zipEntryNames(zipPath: string): string[] {
  const result = spawnSync('unzip', ['-Z1', zipPath], { encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error(`unzip -Z1 ${zipPath} failed: ${result.stderr || result.stdout}`);
  }
  return result.stdout.split(/\r?\n/).filter(Boolean);
}

function readJson<T>(filePath: string): T {
  return JSON.parse(fs.readFileSync(filePath, 'utf8')) as T;
}

function sha256File(filePath: string): string {
  return crypto.createHash('sha256').update(fs.readFileSync(filePath)).digest('hex');
}

function decodedLocation(location: string): string {
  return decodeURIComponent(location.replace(/\+/g, ' '));
}
