import { test, expect, adminLogin, adminCsrf, createStandaloneApp, sqliteExec, sqliteQuery, isNoJsProject } from './helpers';
import { expectHttpError, newAuditedContext } from './diagnostics';
import fsp from 'node:fs/promises';
import path from 'node:path';
import net from 'node:net';

async function freePort(): Promise<number> {
  const server = net.createServer();
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Missing disposable listener address');
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

// Every instance, SQL trigger and destructive action is confined to disposable loopback fixtures.
test('task navigation exposes one workspace at a time on desktop and mobile without JavaScript', async ({ page, browser }, info) => {
  const app = await createStandaloneApp({ admin: true, boards:[{short:"pub",name:"Public Board"}] });
  try {
    await adminLogin(page, app);
    const storageState = await page.context().storageState();
    for (const [layout, viewport] of [['desktop', {width:1280,height:900}], ['mobile', {width:390,height:844}]] as const) {
      const context = await newAuditedContext(browser, { viewport, javaScriptEnabled: !isNoJsProject(info), storageState });
      try {
        const ui = await context.newPage();
        await ui.goto(`${app.baseURL}/admin/panel`);
        await expect(ui.locator('.admin-task:visible')).toHaveCount(1);
        const links = await ui.locator('.admin-section-index a').evaluateAll(items => items.map(item => item.getAttribute('href')!));
        expect(links.length).toBeGreaterThanOrEqual(17);
        for (const href of links) {
          await ui.locator(`.admin-section-index a[href="${href}"]`).click();
          await expect(ui.locator('.admin-task:visible')).toHaveCount(1);
          await expect(ui.locator(href)).toBeVisible();
          expect(await ui.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
        }
        if (layout === 'desktop') {
          for (const [open, target] of [['media-settings','#media-settings'],['database-maintenance','#database-maintenance'],['full-backup-restore','#full-backup-restore'],['board-pub','#board-pub'],['board-appearance-pub','#board-appearance-pub'],['reports','#reports']]) {
            await ui.goto(`${app.baseURL}/admin/panel?open=${open}`);
            await expect(ui.locator('.admin-task:visible')).toHaveCount(1);
            await expect(ui.locator(target)).toBeVisible();
          }
        }
        await ui.locator('.admin-section-index a[href="#accounts"]').click();
        await ui.locator('#accounts').scrollIntoViewIfNeeded();
        await ui.screenshot({path:info.outputPath(`accounts-${layout}.png`)});
        await ui.locator('.admin-section-index a[href="#control-center"]').click();
        await ui.locator('#control-center').scrollIntoViewIfNeeded();
        await ui.screenshot({path:info.outputPath(`overview-${layout}.png`)});
      } finally { await context.close(); }
    }
  } finally { await app.dispose(); }
});

test('HTTPS controls stage real TLS configuration and the restarted native listener serves HTTPS', async ({page}) => {
  const app = await createStandaloneApp({admin:true});
  try {
    await adminLogin(page,app);
    await page.goto(`${app.baseURL}/admin/panel#https`);
    await expect(page.locator('#https .admin-setting')).toHaveCount(12);
    await expect(page.locator('#https')).toContainText('Build support: ACME');
    await expect(page.locator('#https')).toContainText('self-signed development certificates available');
    const port = await freePort();
    await page.locator('[name="tls.port"]').fill(String(port));
    await page.locator('[name="tls.enabled"]').selectOption('true');
    await page.locator('#admin-https-settings button[type=submit]').click();
    await expect(page.locator('.admin-panel > .admin-flash')).toContainText('Restart required');
    await expect(page.locator('[id="state-tls.enabled"]')).toContainText('restart required');
    const file = path.join(app.dataDir,'settings.toml');
    const before = await fsp.readFile(file,'utf8');
    await page.locator('[name="tls.manual_cert.cert_path"]').fill('missing.pem');
    expectHttpError({method:'POST',path:'/admin/config/https',status:422,reason:'Incomplete or missing manual certificate source must fail before saving.'});
    await page.locator('#admin-https-settings button[type=submit]').click();
    await expect(page.getByRole('alert')).toContainText('No changes saved');
    expect(await fsp.readFile(file,'utf8')).toBe(before);
    await app.restart();
    const secure = await page.request.get(`https://127.0.0.1:${port}/healthz`, {ignoreHTTPSErrors:true});
    expect(secure.status()).toBe(200);
    await page.goto(`${app.baseURL}/admin/panel#https`);
    await expect(page.locator('[id="state-tls.enabled"]')).toContainText('Saved configuration matches');
  } finally { await app.dispose(); }
});

test('account creation and password resets require reauthentication and revoke target sessions', async ({page,browser},info) => {
  const app = await createStandaloneApp({admin:true});
  let targetContext;
  try {
    await adminLogin(page,app);
    await page.goto(`${app.baseURL}/admin/panel#accounts`);
    const create = page.locator('#admin-account-create');
    await create.locator('[name=username]').fill('operator');
    await create.locator('[name=current_password]').fill('incorrect');
    await create.locator('[name=new_password]').fill('OperatorPass123!');
    await create.locator('[name=confirm_password]').fill('OperatorPass123!');
    await create.getByRole('button').click();
    await expect(page.locator('.admin-panel > .admin-flash')).toContainText('No account changes saved');
    expect(sqliteQuery(app,"SELECT COUNT(*) FROM admin_users WHERE username='operator';")).toBe('0');
    await create.locator('[name=current_password]').fill('AdminPass123!');
    await create.locator('[name=username]').fill('operator');
    await create.locator('[name=new_password]').fill('OperatorPass123!');
    await create.locator('[name=confirm_password]').fill('OperatorPass123!');
    await create.getByRole('button').click();
    await expect(page.locator('#accounts table')).toContainText('operator');
    targetContext = await newAuditedContext(browser,{javaScriptEnabled:!isNoJsProject(info)});
    const target = await targetContext.newPage();
    await target.goto(`${app.baseURL}/admin`);
    await target.getByLabel('Username').fill('operator');
    await target.getByLabel('Password').fill('OperatorPass123!');
    await target.getByRole('button',{name:'authenticate'}).click();
    await expect(target.locator('.admin-panel')).toBeVisible();
    const reset=page.locator('#admin-account-password');
    await reset.locator('[name=username]').selectOption('operator');
    await reset.locator('[name=current_password]').fill('AdminPass123!');
    await reset.locator('[name=new_password]').fill('ReplacementPass123!');
    await reset.locator('[name=confirm_password]').fill('ReplacementPass123!');
    await reset.getByRole('button').click();
    await expect(page.locator('.admin-panel > .admin-flash')).toContainText('Account saved');
    expect(sqliteQuery(app,"SELECT COUNT(*) FROM admin_sessions WHERE admin_id=(SELECT id FROM admin_users WHERE username='operator');")).toBe('0');
    expectHttpError({method:'GET',path:'/admin/panel',status:403,reason:'Reset target session is revoked and the protected panel fails closed.'});
    const revoked = await target.goto(`${app.baseURL}/admin/panel`);
    expect(revoked?.status()).toBe(403);
    await expect(target.locator('main')).toContainText('Session expired or invalid');
    await target.goto(`${app.baseURL}/admin`);
    await expect(target.getByRole('button',{name:'authenticate'})).toBeVisible();
    await target.getByLabel('Username').fill('operator');
    await target.getByLabel('Password').fill('ReplacementPass123!');
    await target.getByRole('button',{name:'authenticate'}).click();
    await expect(target.locator('.admin-panel')).toBeVisible();
    const html=await page.content();
    for(const secret of ['AdminPass123!','OperatorPass123!','ReplacementPass123!','$argon2']) expect(html).not.toContain(secret);
  } finally { if(targetContext) await targetContext.close(); await app.dispose(); }
});

test('secret rotation is staged, redacted, and invalidates administrator sessions only after restart', async ({page}) => {
  const app=await createStandaloneApp({admin:true});
  try {
    await adminLogin(page,app);
    await page.goto(`${app.baseURL}/admin/panel#storage`);
    await expect(page.locator('#storage')).toContainText('--data-dir /absolute/new/data serve');
    await expect(page.locator('#storage')).toContainText('CHAN_UPLOADS');
    const settings=path.join(app.dataDir,'settings.toml');
    const before=await fsp.readFile(settings,'utf8');
    const rotate=page.locator('#admin-secret-rotation');
    await rotate.locator('[name=current_password]').fill('AdminPass123!');
    await rotate.locator('[name=confirmation]').fill('ROTATE');
    await rotate.getByRole('button').click();
    await expect(page.locator('.admin-panel > .admin-flash')).toContainText('Fresh secret staged');
    await expect(page.locator('#storage')).toContainText('different file secret is staged');
    const after=await fsp.readFile(settings,'utf8');
    const secret=(after.match(/^cookie_secret\s*=\s*"([a-f0-9]{64})"/m)??[])[1];
    expect(Boolean(secret)).toBe(true);
    // Compare only non-secret content, avoiding secret dumps on test failures.
    expect(after.replace(/^cookie_secret.*$/m,'cookie_secret = [redacted]')).toBe(before.replace(/^cookie_secret.*$/m,'cookie_secret = [redacted]'));
    expect((await page.content()).includes(secret)).toBe(false);
    expect(sqliteQuery(app,'SELECT COUNT(*) FROM admin_sessions;')).not.toBe('0');
    await app.restart();
    expect(sqliteQuery(app,'SELECT COUNT(*) FROM admin_sessions;')).toBe('0');
    expectHttpError({method:'GET',path:'/admin/panel',status:403,reason:'Secret rotation revokes the existing session at restart.'});
    const revoked = await page.goto(`${app.baseURL}/admin/panel`);
    expect(revoked?.status()).toBe(403);
    await expect(page.locator('main')).toContainText('Session expired or invalid');
    await page.goto(`${app.baseURL}/admin`);
    await expect(page.getByRole('button',{name:'authenticate'})).toBeVisible();
    await adminLogin(page,app);
    await page.goto(`${app.baseURL}/admin/panel#storage`);
    await expect(page.locator('#storage')).toContainText('No staged file rotation detected');
  } finally {await app.dispose();}
});

test('database failures roll back complete Appearance and Media saves without changing live state or file seeds', async ({page}) => {
  const app=await createStandaloneApp({admin:true});
  try {
    await adminLogin(page,app);
    const settings=path.join(app.dataDir,'settings.toml');
    const bytes=await fsp.readFile(settings,'utf8');
    const dbBefore=sqliteQuery(app,"SELECT key||'='||value FROM site_settings ORDER BY key;");
    sqliteExec(app,"CREATE TRIGGER reject_theme BEFORE INSERT ON site_settings WHEN NEW.key='default_theme' BEGIN SELECT RAISE(ABORT,'fixture rejected theme save'); END;");
    const site=await page.request.post(`${app.baseURL}/admin/site/settings`,{form:{_csrf:await adminCsrf(page,app),site_name:'MUST NOT APPLY',site_subtitle:'MUST NOT APPLY',default_theme:'terminal'},maxRedirects:0});
    expect(site.status()).toBe(500);
    expect(sqliteQuery(app,"SELECT key||'='||value FROM site_settings ORDER BY key;")).toBe(dbBefore);
    expect(await fsp.readFile(settings,'utf8')).toBe(bytes);
    await page.goto(app.baseURL);
    await expect(page.locator('body')).not.toContainText('MUST NOT APPLY');
    sqliteExec(app,"DROP TRIGGER reject_theme; CREATE TRIGGER reject_timeout BEFORE INSERT ON site_settings WHEN NEW.key='ffmpeg_timeout_secs' BEGIN SELECT RAISE(ABORT,'fixture rejected media save'); END;");
    const media=await page.request.post(`${app.baseURL}/admin/media/settings`,{form:{_csrf:await adminCsrf(page,app),ffmpeg_timeout_secs:'1800',media_auto_prune_enabled:'1',media_max_active_content_size:'1',media_max_active_content_size_unit:'gib'},maxRedirects:0});
    expect(media.status()).toBe(500);
    expect(sqliteQuery(app,"SELECT key||'='||value FROM site_settings ORDER BY key;")).toBe(dbBefore);
    expect(await fsp.readFile(settings,'utf8')).toBe(bytes);
  } finally {await app.dispose();}
});

test('logging and bounded operator policies save actual supported values and apply after restart', async ({page}) => {
  const app=await createStandaloneApp({admin:true,env:{RUST_LOG:undefined}});
  try {
    await adminLogin(page,app);
    for(const [section,key,value] of [['access','admin_login_fail_limit','7'],['display','index_threads_per_page','12'],['timeouts','write_timeout_secs','420'],['logging','log_filter','warn,admin=info']]) {
      await page.goto(`${app.baseURL}/admin/panel#${section}`);
      await page.locator(`[name=${key}]`).fill(value);
      await page.locator(`#admin-${section}-settings button[type=submit]`).click();
      await expect(page.locator('.admin-panel > .admin-flash')).toContainText('Restart required');
      await expect(page.locator(`#state-${key}`)).toContainText('restart required');
    }
    const file=path.join(app.dataDir,'settings.toml');
    const before=await fsp.readFile(file,'utf8');
    await page.locator('[name=log_filter]').fill('not a valid directive[');
    expectHttpError({method:'POST',path:'/admin/config/logging',status:422,reason:'Malformed tracing filters must fail before persistence.'});
    await page.locator('#admin-logging-settings button[type=submit]').click();
    await expect(page.getByRole('alert')).toContainText('No changes saved');
    expect(await fsp.readFile(file,'utf8')).toBe(before);
    await app.restart();
    await page.goto(`${app.baseURL}/admin/panel#access`);
    await expect(page.locator('#setting-admin_login_fail_limit')).toHaveValue('7');
    await expect(page.locator('#state-admin_login_fail_limit')).toContainText('Saved configuration matches');
  } finally {await app.dispose();}
});

test('regular Appearance control applies the site NSFW default live while explicit visitor cookies win',async({page})=>{
  const app=await createStandaloneApp({admin:true});
  try{
    await adminLogin(page,app);
    await page.goto(`${app.baseURL}/admin/panel#site-settings`);
    await page.locator('#setting-default_hide_nsfw_boards').selectOption('true');
    await page.getByRole('button',{name:'Save visitor default'}).click();
    await expect(page.locator('.admin-panel > .admin-flash')).toContainText('applied live');
    expect(sqliteQuery(app,"SELECT value FROM site_settings WHERE key='default_hide_nsfw_boards';")).toBe('1');
    await page.goto(app.baseURL);
    await expect(page.locator('input[name=hide_nsfw_boards]').first()).toBeChecked();
    await page.context().addCookies([{name:'rustchan_hide_nsfw',value:'0',url:app.baseURL}]);
    await page.reload();
    await expect(page.locator('input[name=hide_nsfw_boards]').first()).not.toBeChecked();
    await app.restart();
    await page.goto(app.baseURL);
    await expect(page.locator('input[name=hide_nsfw_boards]').first()).not.toBeChecked();
  }finally{await app.dispose();}
});


test('every new configuration and credential route rejects anonymous, forged and cross-origin saves', async ({page}) => {
  const app = await createStandaloneApp({admin:true});
  try {
    const routes = ['https','tor','media','maintenance','system','access','display','logging','timeouts'].map(section=>`/admin/config/${section}`);
    routes.push('/admin/accounts/create','/admin/accounts/password','/admin/secrets/rotate','/admin/appearance/defaults');
    const fields = {username:'owner',current_password:'AdminPass123!',new_password:'ChangedPass123!',confirm_password:'ChangedPass123!',confirmation:'ROTATE',default_hide_nsfw_boards:'true'};
    const settings = path.join(app.dataDir,'settings.toml');
    const fileBefore = await fsp.readFile(settings,'utf8');
    const dbBefore = sqliteQuery(app,"SELECT key||'='||value FROM site_settings ORDER BY key;");
    for(const route of routes) {
      const response = await page.request.post(`${app.baseURL}${route}`,{form:fields,maxRedirects:0});
      expect(response.status(),route).toBe(403);
    }
    await adminLogin(page,app);
    const csrf = await adminCsrf(page,app);
    for(const route of routes) {
      const forged = await page.request.post(`${app.baseURL}${route}`,{form:{...fields,_csrf:'forged'},maxRedirects:0});
      expect(forged.status(),route).toBe(403);
      const cross = await page.request.post(`${app.baseURL}${route}`,{headers:{Origin:'https://attacker.example'},form:{...fields,_csrf:csrf},maxRedirects:0});
      expect(cross.status(),route).toBe(403);
    }
    expect(await fsp.readFile(settings,'utf8')).toBe(fileBefore);
    expect(sqliteQuery(app,"SELECT key||'='||value FROM site_settings ORDER BY key;")).toBe(dbBefore);
    expect(sqliteQuery(app,'SELECT COUNT(*) FROM admin_users;')).toBe('1');
  } finally {await app.dispose();}
});

test('backup save failure preserves the live scheduler and invalid environment fallback is reported accurately', async ({page}) => {
  const app = await createStandaloneApp({admin:true,env:{CHAN_AUTO_FULL_BACKUP_COPIES:'malformed'}});
  const file = path.join(app.dataDir,'settings.toml');
  const preserved = path.join(app.dataDir,'preserved-settings.toml');
  let replaced = false;
  try {
    await adminLogin(page,app);
    await page.goto(`${app.baseURL}/admin/panel?open=full-backup-restore#full-backup-restore`);
    const form = page.locator('form[action="/admin/backup/settings"]:has([name=auto_full_backup_copies_to_keep])');
    await form.locator('[name=auto_full_backup_copies_to_keep]').fill('7');
    const before = await fsp.readFile(file,'utf8');
    await fsp.rename(file,preserved);
    await fsp.mkdir(file);
    replaced = true;
    expectHttpError({method:'POST',path:'/admin/backup/settings',status:400,reason:'Actual settings path I/O failure must not update the live scheduler.'});
    await form.getByRole('button',{name:/save/i}).click();
    await expect(page.locator('main')).toContainText('No settings saved');
    expect(await fsp.readFile(preserved,'utf8')).toBe(before);
    await fsp.rmdir(file); await fsp.rename(preserved,file); replaced = false;
    await page.goto(`${app.baseURL}/admin/panel#configuration-state`);
    const row = page.locator('#application-auto_full_backup_copies_to_keep');
    await expect(row).toContainText('present but invalid');
    await expect(row.locator('td').nth(0)).toHaveText('1');
    await page.goto(`${app.baseURL}/admin/panel?open=full-backup-restore#full-backup-restore`);
    await form.locator('[name=auto_full_backup_copies_to_keep]').fill('7');
    await form.getByRole('button',{name:/save/i}).click();
    await page.goto(`${app.baseURL}/admin/panel#configuration-state`);
    await expect(row.locator('td').nth(0)).toHaveText('7');
    await expect(row.locator('td').nth(1)).toHaveText('7');
    await expect(row).toContainText('present but invalid');
    await app.restart();
    await page.goto(`${app.baseURL}/admin/panel#configuration-state`);
    await expect(row.locator('td').nth(0)).toHaveText('7');
  } finally {
    if(replaced) {await fsp.rmdir(file);await fsp.rename(preserved,file);}
    await app.dispose();
  }
});
