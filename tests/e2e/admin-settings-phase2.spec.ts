import fs from 'node:fs';
import path from 'node:path';
import {
  adminCsrf,
  adminLogin,
  expect,
  expectSafePage,
  expectSafeResponse,
  sqliteQuery,
  test,
} from './helpers';

test.describe('phase 2 admin settings save flows', () => {
  test('site, media, backup, banner toggles, invalid values, and TOML writeback stay deterministic', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'browser-generic admin settings coverage runs on Chromium first');

    await adminLogin(page, app);
    const csrf = await adminCsrf(page, app);

    const longName = `Phase 2 ${'N'.repeat(90)}`;
    const longSubtitle = `Subtitle ${'S'.repeat(160)}`;
    const longSave = await page.request.post(`${app.baseURL}/admin/site/settings`, {
      form: {
        _csrf: csrf,
        site_name: longName,
        site_subtitle: longSubtitle,
        default_theme: '../not-a-theme',
      },
      maxRedirects: 0,
    });
    expect(longSave.status()).toBe(303);
    expect(settingValue(app, 'site_name')).toBe(longName.slice(0, 64));
    expect(settingValue(app, 'site_subtitle')).toBe(longSubtitle.slice(0, 128));
    expect(settingValue(app, 'default_theme')).toBe('forest');
    expect(settingValue(app, 'homepage_new_thread_badges_enabled')).toBe('0');
    expect(settingValue(app, 'homepage_new_reply_badges_enabled')).toBe('0');
    expect(settingValue(app, 'thread_new_reply_badges_enabled')).toBe('0');
    let settingsToml = fs.readFileSync(path.join(app.dataDir, 'settings.toml'), 'utf8');
    expect(settingsToml).toContain('homepage_new_thread_badges_enabled = false');
    expect(settingsToml).toContain('homepage_new_reply_badges_enabled = false');
    expect(settingsToml).toContain('thread_new_reply_badges_enabled = false');
    expect(settingsToml).toContain('default_theme = "forest"');

    await page.goto(`${app.baseURL}/admin/panel#site-settings`);
    const siteForm = page.locator('form[action="/admin/site/settings"]').first();
    await siteForm.locator('input[name="site_name"]').fill('Phase 2 Settings');
    await siteForm.locator('input[name="site_subtitle"]').fill('settings persisted through restart');
    await siteForm.locator('select[name="default_theme"]').selectOption('terminal');
    await siteForm.locator('input[name="homepage_new_thread_badges_enabled"]').uncheck();
    await siteForm.locator('input[name="homepage_new_reply_badges_enabled"]').uncheck();
    await siteForm.locator('input[name="thread_new_reply_badges_enabled"]').uncheck();
    await Promise.all([
      page.waitForURL(/\/admin\/panel/),
      siteForm.getByRole('button', { name: /^save settings$/i }).click(),
    ]);
    expect(settingValue(app, 'site_name')).toBe('Phase 2 Settings');
    expect(settingValue(app, 'default_theme')).toBe('terminal');
    expect(settingValue(app, 'homepage_new_thread_badges_enabled')).toBe('0');

    await page.goto(`${app.baseURL}/admin/panel?open=media-settings#media-settings`);
    const mediaForm = page.locator('form[action="/admin/media/settings"]');
    await mediaForm.locator('input[name="ffmpeg_timeout_secs"]').fill('1800');
    await mediaForm.locator('input[name="media_max_active_content_size"]').fill('2');
    await mediaForm.locator('select[name="media_max_active_content_size_unit"]').selectOption('mib');
    await mediaForm.locator('input[name="media_auto_prune_enabled"]').check();
    await Promise.all([
      page.waitForURL(/\/admin\/panel/),
      mediaForm.getByRole('button', { name: /save media settings/i }).click(),
    ]);
    settingsToml = fs.readFileSync(path.join(app.dataDir, 'settings.toml'), 'utf8');
    expect(settingsToml).toContain('ffmpeg_timeout_secs = 1800');
    expect(settingsToml).toContain('media_auto_prune_enabled = true');
    expect(settingsToml).toContain('media_max_active_content_size_bytes = 2097152');
    expect(settingValue(app, 'media_auto_prune_enabled')).toBe('true');
    expect(settingValue(app, 'media_max_active_content_size_bytes')).toBe('2097152');

    const invalidMedia = await page.request.post(`${app.baseURL}/admin/media/settings`, {
      form: {
        _csrf: await adminCsrf(page, app),
        ffmpeg_timeout_secs: '29',
        media_auto_prune_enabled: '1',
        media_max_active_content_size: '0',
        media_max_active_content_size_unit: 'mib',
      },
      maxRedirects: 0,
    });
    expect(invalidMedia.status()).toBe(303);
    expect(decodeURIComponent(invalidMedia.headers().location ?? '')).toContain('ffmpeg_timeout_secs');
    expect(settingValue(app, 'media_auto_prune_enabled')).toBe('true');

    const backupSave = await page.request.post(`${app.baseURL}/admin/backup/settings`, {
      form: {
        _csrf: await adminCsrf(page, app),
        auto_full_backup_interval_hours: '0',
        auto_full_backup_copies_to_keep: '7',
        auto_full_backup_storage_mode: 'split_zip',
        auto_full_backup_split_zip_part_size_gib: '2',
      },
      maxRedirects: 0,
    });
    expect(backupSave.status()).toBe(303);
    settingsToml = fs.readFileSync(path.join(app.dataDir, 'settings.toml'), 'utf8');
    expect(settingsToml).toContain('auto_full_backup_interval_hours = 0');
    expect(settingsToml).toContain('auto_full_backup_copies_to_keep = 7');
    expect(settingsToml).toContain('auto_full_backup_include_tor_hidden_service_keys = false');
    expect(settingsToml).toContain('auto_full_backup_storage_mode = "split_zip"');
    expect(settingsToml).toContain('auto_full_backup_split_zip_part_size_gib = 2');

    const invalidBackup = await page.request.post(`${app.baseURL}/admin/backup/settings`, {
      form: {
        _csrf: await adminCsrf(page, app),
        auto_full_backup_interval_hours: '5',
        auto_full_backup_copies_to_keep: '5',
        auto_full_backup_storage_mode: 'tar',
        auto_full_backup_split_zip_part_size_gib: '2',
      },
      maxRedirects: 0,
    });
    expect(invalidBackup.status()).toBe(400);
    await expectSafeResponse(invalidBackup);

    const bannerOnly = await page.request.post(`${app.baseURL}/admin/site/settings`, {
      form: {
        _csrf: await adminCsrf(page, app),
        banner_rotation_interval_minutes: '15',
      },
      maxRedirects: 0,
    });
    expect(bannerOnly.status()).toBe(303);
    expect(settingValue(app, 'banner_rotation_interval_minutes')).toBe('15');
    expect(settingValue(app, 'banner_external_links_enabled')).toBe('0');
    expect(settingValue(app, 'homepage_new_thread_badges_enabled')).toBe('0');

    await app.restart();
    await page.goto(app.baseURL);
    await expect(page.locator('body')).toContainText('Phase 2 Settings');
    await expect(page.locator('body')).toContainText('settings persisted through restart');
    await expectSafePage(page);
    settingsToml = fs.readFileSync(path.join(app.dataDir, 'settings.toml'), 'utf8');
    expect(settingsToml).toContain('forum_name = "Phase 2 Settings"');
    expect(settingsToml).toContain('site_subtitle = "settings persisted through restart"');
  });

  test('admin settings mutations are admin-only and CSRF-protected', async ({ page, app }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium', 'admin security route coverage runs on Chromium first');

    const loggedOut = await page.request.post(`${app.baseURL}/admin/site/settings`, {
      form: {
        _csrf: 'missing-session',
        site_name: 'should not save',
        site_subtitle: 'should not save',
        default_theme: 'forest',
      },
      maxRedirects: 0,
    });
    expect(loggedOut.status()).toBe(403);
    await expectSafeResponse(loggedOut);

    await adminLogin(page, app);
    for (const route of ['/admin/site/settings', '/admin/media/settings', '/admin/backup/settings']) {
      const response = await page.request.post(`${app.baseURL}${route}`, {
        form: { _csrf: 'invalid.csrf' },
        maxRedirects: 0,
      });
      expect(response.status(), route).toBe(403);
      await expectSafeResponse(response);
    }
  });
});

function settingValue(app: { dbPath(): string }, key: string): string {
  return sqliteQuery(
    app,
    `SELECT value FROM site_settings WHERE key = '${key.replaceAll("'", "''")}' LIMIT 1;`,
  );
}
