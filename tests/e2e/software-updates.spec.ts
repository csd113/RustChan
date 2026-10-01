import { test, expect, adminLogin, adminCsrf, createStandaloneApp, isNoJsProject } from './helpers';
import { expectHttpError, expectTransportFailure, newAuditedContext } from './diagnostics';
import fsp from 'node:fs/promises';
import path from 'node:path';
import fs from 'node:fs';
import http from 'node:http';
import { BUILTIN_THEMES } from './phase4-helpers';

const packageVersion = fs.readFileSync(path.resolve('Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)?.[1];
if (!packageVersion) throw new Error('Missing RustChan package version');
const candidateVersion = `${packageVersion.split('.')[0]}.${Number(packageVersion.split('.')[1]) + 1}.0`;

// State fixtures contain no production hooks: the real server reads its persisted
// check-only discovery file, just as it does after a GitHub check.
async function savedStatus(app: {dataDir:string}, changes: Record<string, unknown> = {}) {
  const status = {phase:'idle', installed:packageVersion, discovery:null, approval:null, checked_at:'2026-09-30T00:00:00Z', job:null, administrator:null, previous_version:null, target_version:null, message:'No installation attempted.', backup:null, backups:[], updated_at:'2026-09-30T00:00:00Z', ...changes};
  await fsp.mkdir(path.join(app.dataDir,'runtime'), {recursive:true});
  const destination=path.join(app.dataDir,'runtime/software-updates.json');
  await fsp.writeFile(`${destination}.tmp`, JSON.stringify(status));
  await fsp.rename(`${destination}.tmp`, destination);
}
const available = {available:{id:42, version:candidateVersion, published_at:'2026-09-30T00:00:00Z', notes:'Release fixture', manifest:null, size:100, verification:'Release failed verification: signature mismatch.', compatible:false}};

for (const width of [1280,390,320]) {
  test(`software updates fit current admin themes at ${width}px and no-JS task navigation`, async ({page,browser}, info) => {
    const app = await createStandaloneApp({admin:true, env:{RUSTCHAN_CONTAINER:'1'}});
    try {
      await adminLogin(page,app);
      const storageState = await page.context().storageState();
      for (const [theme] of BUILTIN_THEMES) {
        const context = await newAuditedContext(browser,{storageState, viewport:{width,height:900}, javaScriptEnabled:!isNoJsProject(info)});
        try {

          const ui = await context.newPage();
          await savedStatus(app,{discovery:'up_to_date'});
          await ui.goto(`${app.baseURL}/theme/${theme}?return_to=${encodeURIComponent('/admin/panel?open=software-updates#software-updates')}`);
          await expect(ui.locator('html')).toHaveAttribute('data-active-theme',theme);
          await expect(ui.locator('#software-updates')).toBeVisible();
          await expect(ui.locator('.admin-task:visible')).toHaveCount(1);
          await expect(ui.locator('#software-updates')).toContainText('Up to date');
          await expect(ui.locator('#software-updates')).toContainText('Container installation is deployment-managed');
          await expect(ui.locator('#admin-update-install')).toHaveCount(0);
          expect(await ui.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBe(true);
          await savedStatus(app,{discovery:available});
          await ui.reload();
          await expect(ui.locator('.admin-section-index a[href="#software-updates"]')).toContainText('Update available');
          await expect(ui.locator('#software-updates')).toContainText('signature mismatch');
          await expect(ui.locator('#admin-update-install')).toHaveCount(0);
          await ui.locator('#admin-update-check button').focus();
          await expect(ui.locator('#admin-update-check button')).toBeFocused();
          expect(await ui.locator('#admin-update-check button').evaluate(element => parseFloat(getComputedStyle(element).outlineWidth))).toBeGreaterThanOrEqual(2);
          await ui.screenshot({path:info.outputPath(`updates-${theme}-${width}.png`), fullPage:true});
        } finally {await context.close();}
      }
    } finally {await app.dispose();}
  });
}

test('update data is administrator-only and public readiness exposes only running version', async ({page,browser},info)=>{
  const app=await createStandaloneApp({admin:true,boards:[{short:'pub',name:'Public'}]});
  try {
    await savedStatus(app,{discovery:available});
    await adminLogin(page,app);
    const csrf=await adminCsrf(page,app);
    const ready=await page.request.get(`${app.baseURL}/readyz`);
    expect(await ready.json()).toEqual({status:'ready',version:packageVersion});
    // RustChan has no ordinary-account or reduced moderator session role.
    // Forged public-session cookies must never satisfy the full-admin gate.
    for(const cookies of [[],[{name:'chan_user_session',value:'ordinary-account',url:app.baseURL}],[{name:'chan_user_session',value:'moderator',url:app.baseURL}]]) {
      const context=await newAuditedContext(browser,{javaScriptEnabled:!isNoJsProject(info)});
      try {
        await context.addCookies(cookies);
        const publicPage=await context.newPage();
        await publicPage.goto(app.baseURL);
        await expect(publicPage.locator('body')).not.toContainText('Update available');
        for(const route of ['status','check','install']){
          const response=route==='status'?await context.request.get(`${app.baseURL}/admin/updates/${route}`):await context.request.post(`${app.baseURL}/admin/updates/${route}`,{form:route==='check'?{_csrf:csrf}:{_csrf:csrf,current_password:'AdminPass123!',confirmation:'INSTALL',approval:'00000000-0000-4000-8000-000000000000'},headers:{Origin:app.baseURL}});
          expect([400,403]).toContain(response.status());
          expect(await response.text()).not.toContain(candidateVersion);
        }
      }finally{await context.close();}
    }
    for(const route of ['check','install']){
      const get=await page.request.get(`${app.baseURL}/admin/updates/${route}`);
      expect(get.status()).toBe(405);
      const forged=await page.request.post(`${app.baseURL}/admin/updates/${route}`,{form:route==='check'?{_csrf:'forged'}:{_csrf:'forged',current_password:'bad',confirmation:'INSTALL',approval:'00000000-0000-4000-8000-000000000000'},headers:{Origin:app.baseURL}});
      expect([400,403]).toContain(forged.status());
    }
    const password=await page.request.post(`${app.baseURL}/admin/updates/install`,{form:{_csrf:csrf,current_password:'incorrect',confirmation:'INSTALL',approval:'00000000-0000-4000-8000-000000000000'},headers:{Origin:app.baseURL}});
    expect(password.status()).toBe(403);
    expect(await password.text()).toContain('Current password did not verify');
  }finally{await app.dispose();}
});

test('persisted update progress survives a real outage and reconnects to the final result', async ({page},info)=>{
  test.skip(isNoJsProject(info),'automatic reconnect is an enhanced-client behavior; persisted no-JS result has separate coverage');
  const app=await createStandaloneApp({admin:true});
  try {
    await adminLogin(page,app);
    await savedStatus(app,{phase:'health_checking',message:'Checking the restarted release.'});
    await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
    await expect(page.locator('#admin-update-message')).toContainText('Checking the restarted release.');
    expectTransportFailure({url:`${app.baseURL}/admin/updates/status`,pattern:/ERR_CONNECTION_REFUSED|Failed to fetch|Could not connect|Connection refused|NS_ERROR_CONNECTION_REFUSED/i,times:'any',reason:'The disposable fixture is intentionally stopped while the admin reconnect loop runs.'});
    await app.stop();
    await expect(page.locator('#admin-update-message')).toContainText('restarting');
    await savedStatus(app,{phase:'succeeded',message:'RustChan updated successfully after health verification.'});
    await app.start();
    await expect(page.locator('#admin-update-message')).toContainText('updated successfully');
    await expect(page).toHaveURL(/open=software-updates/);
    await page.reload();
    await expect(page.locator('#admin-update-message')).toContainText('updated successfully');
  }finally{await app.dispose();}
});

test('rollback and verified pre-upgrade snapshots remain visible after reload without JavaScript',async({page},info)=>{
  const app=await createStandaloneApp({admin:true});
  try{
    await adminLogin(page,app);
    await savedStatus(app,{phase:'rolled_back',previous_version:'1.5.0',target_version:'1.6.0',message:'Readiness failed; previous software, database and configuration restored.',backups:[{id:'00000000-0000-4000-8000-000000000001',created_at:'2026-09-30T00:00:00Z',previous_version:'1.5.0',target_version:'1.6.0',previous_schema:'1.5.0',target_schema:'1.6.0',size:123,database_sha256:'00'.repeat(32),configuration_sha256:'00'.repeat(32),verified:true,reason:'pre_upgrade',persistent_sha256:'00'.repeat(32)}]});
    await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
    await expect(page.locator('#software-updates')).toContainText('Readiness failed');
    await page.reload();
    await expect(page.locator('#software-updates')).toContainText('configuration restored');
    await page.locator('.admin-section-index a[href="#backups"]').click();
    await expect(page.locator('#pre-upgrade-backups')).toContainText('Verified pre-upgrade backup');
    await expect(page.locator('#pre-upgrade-backups form')).toHaveCount(0);
  }finally{await app.dispose();}
});

test('native renderer confirmation and enhanced install flow reconnect to persisted success or rollback',async({page,browserName},info)=>{
  const noJs = isNoJsProject(info);
  const app=await createStandaloneApp({admin:true});
  try{
    await adminLogin(page,app);
    const native=await fsp.readFile(path.resolve('output/playwright/update-fixtures/native-available.html'),'utf8');
    const csrf=await adminCsrf(page,app);
    for(const result of ['succeeded','rolled_back']) {
      await savedStatus(app,{message:'Ready for installation.'});
      // Replace only the update section with the exact native Rust renderer output.
      // The shell, styles, scripts and authenticated server routes remain real.
      await page.route('**/admin/panel?open=software-updates*',async route=>{
        // Playwright's API helper does not decode Firefox's zstd navigation response.
        const response=await route.fetch({headers:{...route.request().headers(),'accept-encoding':'identity'}});
        expect(response.status()).toBe(200);
        const html=await response.text();
        const marker='<section class="admin-section admin-settings-section" id="software-updates"';
        const start=html.indexOf(marker);
        if(start<0)throw new Error('Missing real software update section');
        const end=html.indexOf('</section>',start)+'</section>'.length;
        await route.fulfill({response,body:html.slice(0,start)+native.replaceAll('FIXTURE_CSRF',csrf)+html.slice(end)});
      });
      await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
      // Force a fetch when a previous terminal redirect already selected this URL.
      await page.reload();
      await expect(page.locator('#admin-update-install')).toBeVisible();
      if (result === 'succeeded') {
        for (const [theme] of BUILTIN_THEMES) {
          await page.goto(`${app.baseURL}/theme/${theme}?return_to=${encodeURIComponent('/admin/panel?open=software-updates#software-updates')}`);
          // Redirected requests do not invoke a new route handler; reload the exact native fixture.
          await page.reload();
          for (const width of [1280, 390, 320]) {
            await page.setViewportSize({width,height:900});
            expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBe(true);
            await page.locator('#update-current-password').focus();
            await page.keyboard.press('Tab');
            await expect(page.locator('#update-confirmation')).toBeFocused();
            // Safari's default keyboard preference tabs through text fields; Option-Tab
            // also reaches buttons without changing the operator's system preference.
            await page.keyboard.press(browserName === 'webkit' ? 'Alt+Tab' : 'Tab');
            await expect(page.locator('#admin-update-install button')).toBeFocused();
            expect(await page.locator('#admin-update-install button').evaluate(element => parseFloat(getComputedStyle(element).outlineWidth))).toBeGreaterThanOrEqual(2);
            await page.screenshot({path:info.outputPath(`native-confirmation-${theme}-${width}.png`),fullPage:true});
          }
        }
        await page.setViewportSize({width:1280,height:900});
        // Start functional submission from a fresh document after the viewport matrix.
        await page.goto(`${app.baseURL}/theme/forest?return_to=${encodeURIComponent('/admin/panel?open=software-updates#software-updates')}`);
        await page.reload();
      }
      await page.locator('#update-current-password').fill('AdminPass123!');
      await page.locator('#update-confirmation').fill('wrong');
      expect(await page.locator('#admin-update-install').evaluate((form:HTMLFormElement)=>form.checkValidity())).toBe(false);
      await page.locator('#update-confirmation').fill('INSTALL');
      await page.route('**/admin/updates/install',async route=>{
        expect(route.request().method()).toBe('POST');
        const data=new URLSearchParams(route.request().postData()??'');
        expect(data.get('current_password')).toBe('AdminPass123!');
        expect(data.get('confirmation')).toBe('INSTALL');
        await savedStatus(app,{phase:'health_checking',message:'Checking the restarted release.'});
        if (noJs) {
          await page.unroute('**/admin/panel?open=software-updates*');
          await route.fulfill({status:303,headers:{Location:'/admin/panel?open=software-updates#software-updates'},body:''});
        } else {
          await route.fulfill({status:202,body:'accepted'});
        }
      });
      await page.locator('#admin-update-install button').click();
      await expect(page.locator('#admin-update-message')).toContainText('Checking the restarted release.');
      if (!noJs) await expect(page.locator('#update-current-password')).toHaveValue('');
      await page.unroute('**/admin/panel?open=software-updates*');
      await savedStatus(app,{phase:result,message:result==='succeeded'?'Update succeeded after health verification.':'Update failed; complete previous state restored.'});
      if (noJs) await page.reload();
      await expect(page.locator('#admin-update-message')).toContainText(result==='succeeded'?'Update succeeded':'complete previous state restored');
      await page.unroute('**/admin/updates/install');
      await page.reload();
      await expect(page.locator('#admin-update-message')).toContainText(result==='succeeded'?'Update succeeded':'complete previous state restored');
    }
  }finally{await app.dispose();}
});


test('persisted transaction phases, failed checks and malformed status keep a usable narrow admin task', async ({page},info) => {
  const app=await createStandaloneApp({admin:true,env:{RUSTCHAN_CONTAINER:'1'}});
  try {
    await adminLogin(page,app);
    await page.setViewportSize({width:320,height:844});
    const active=['downloading','verifying','stopping','backing_up','staged','activating','restarting','health_checking','rolling_back','restarting_previous','resuming_previous'];
    const terminal=['idle','succeeded','rolled_back','failed','failed_manual_intervention'];
    for (const phase of [...active,...terminal]) {
      // Stop the previous phase's reconnect loop and force a real document fetch.
      // Repeating an identical fragment URL can otherwise reuse the existing DOM.
      await page.goto(`${app.baseURL}/admin/panel?open=control-center#control-center`);
      const message=`${phase}: retained transaction result. ${'ReleaseMetadata'.repeat(20)}`;
      await savedStatus(app,{phase,message,discovery:{unable_to_check:'Official release check failed; retry after the network recovers.'}});
      await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
      await expect(page.locator('#admin-update-message')).toContainText(message);
      await expect(page.locator('#admin-update-message')).toHaveAttribute('role','status');
      await expect(page.locator('#software-updates')).toContainText('Official release check failed');
      await expect(page.locator('#admin-update-install')).toHaveCount(0);
      if(active.includes(phase)) await expect(page.locator('#admin-update-check button')).toBeDisabled();
      else await expect(page.locator('#admin-update-check button')).toBeEnabled();
      expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBe(true);
      if(['downloading','rolled_back','failed_manual_intervention'].includes(phase)) await page.screenshot({path:info.outputPath(`state-${phase}-320.png`),fullPage:true});
    }
    await page.goto(`${app.baseURL}/admin/panel?open=control-center#control-center`);
    await fsp.writeFile(path.join(app.dataDir,'runtime/software-updates.json'),'{invalid status');
    await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
    await expect(page.locator('#software-updates')).toContainText('Not checked');
    await expect(page.locator('#admin-update-check button')).toBeEnabled();
    await expect(page.locator('#admin-update-install')).toHaveCount(0);
    await page.locator('.admin-section-index a[href="#backups"]').click();
    await expect(page.locator('#pre-upgrade-backups')).toContainText('No pre-upgrade snapshots retained.');
  } finally {await app.dispose();}
});


test('native release checks retain their task through a pending request, failure and retry', async ({page},info) => {
  const app=await createStandaloneApp({admin:true,env:{RUSTCHAN_CONTAINER:'1'}});
  let finishResponse=()=>{};
  // WebKit cannot fulfill a synthetic redirect response. A disposable real
  // loopback response preserves the native POST -> 303 -> authenticated GET.
  const redirector=http.createServer((request,response)=>{
    request.resume();
    response.writeHead(303,{Location:`${app.baseURL}/admin/panel?open=software-updates#software-updates`});
    response.end();
  });
  try {
    await new Promise<void>((resolve,reject)=>{
      redirector.once('error',reject);
      redirector.listen(0,'127.0.0.1',resolve);
    });
    const address=redirector.address();
    if(!address||typeof address==='string')throw new Error('Missing release-check fixture listener');
    const redirectedCheckURL=`http://127.0.0.1:${address.port}/admin/updates/check`;
    await adminLogin(page,app);
    await page.setViewportSize({width:320,height:844});
    await savedStatus(app);
    await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
    for (const outcome of ['failed','up_to_date']) {
      let receivedRequest=()=>{};
      const received=new Promise<void>(resolve=>{receivedRequest=resolve;});
      const responseAllowed=new Promise<void>(resolve=>{finishResponse=resolve;});
      await page.route('**/admin/updates/check',async route=>{
        expect(route.request().method()).toBe('POST');
        expect(new URLSearchParams(route.request().postData()??'').get('_csrf')).toMatch(/\S/);
        receivedRequest();
        // Hold the real native-form navigation without making a GitHub request.
        await responseAllowed;
        await savedStatus(app,{discovery:outcome==='failed'?{unable_to_check:'Official release service unavailable. Retry later.'}:'up_to_date',message:'Release check completed. Installation is deployment-managed.'});
        await route.continue({url:redirectedCheckURL});
      });
      if(outcome==='failed') await page.screenshot({path:info.outputPath('release-check-ready-320.png'),fullPage:true});
      const submission=page.locator('#admin-update-check button').click();
      await received;
      // Checking is native navigation, not a fabricated transaction phase.
      // Chromium can suspend DOM inspection behind provisional navigation.
      finishResponse();
      await submission;
      await expect(page).toHaveURL(/open=software-updates/);
      await expect(page.locator('#software-updates')).toContainText(outcome==='failed'?'Official release service unavailable':'Up to date');
      await expect(page.locator('#admin-update-check button')).toBeEnabled();
      await expect(page.locator('#admin-update-install')).toHaveCount(0);
      expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBe(true);
      await page.unroute('**/admin/updates/check');
    }
  } finally {
    finishResponse();
    await app.dispose();
    if(redirector.listening) {
      redirector.closeAllConnections();
      await new Promise<void>((resolve,reject)=>redirector.close(error=>error?reject(error):resolve()));
    }
  }
});

test('rejected installation reauthentication gives a recoverable enhanced or native result', async ({page},info) => {
  const app=await createStandaloneApp({admin:true,env:{RUSTCHAN_CONTAINER:'1'}});
  try {
    await adminLogin(page,app);
    const csrf=await adminCsrf(page,app);
    const native=await fsp.readFile(path.resolve('output/playwright/update-fixtures/native-available.html'),'utf8');
    await page.route('**/admin/panel?open=software-updates*',async route=>{
      const response=await route.fetch({headers:{...route.request().headers(),'accept-encoding':'identity'}});
      const html=await response.text();
      const marker='<section class="admin-section admin-settings-section" id="software-updates"';
      const start=html.indexOf(marker);
      if(start<0)throw new Error('Missing real software update section');
      const end=html.indexOf('</section>',start)+'</section>'.length;
      await route.fulfill({response,body:html.slice(0,start)+native.replaceAll('FIXTURE_CSRF',csrf)+html.slice(end)});
    });
    await page.setViewportSize({width:320,height:844});
    await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
    await page.locator('#update-current-password').fill('NotThePassword123!');
    await page.locator('#update-confirmation').fill('INSTALL');
    expectHttpError({method:'POST',path:'/admin/updates/install',status:403,reason:'The real administrator handler rejects an intentionally incorrect current password before any updater request.'});
    // This POST reaches the real handler, not a mocked success. Container mode
    // provides a second fail-closed boundary even if the test password changed.
    await page.locator('#admin-update-install button').click();
    if(isNoJsProject(info)) {
      await expect(page.locator('body')).toContainText('Current password did not verify');
    } else {
      await expect(page.locator('#admin-update-message')).toContainText('Installation request was rejected');
      await expect(page.locator('#admin-update-install button')).toBeEnabled();
      await expect(page.locator('#update-current-password')).toHaveValue('');
    }
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth)).toBe(true);
    await page.screenshot({path:info.outputPath('reauthentication-rejected-320.png'),fullPage:true});
    await page.unroute('**/admin/panel?open=software-updates*');
    await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
    await page.reload();
    await expect(page.locator('#software-updates')).toBeVisible();
    await expect(page.locator('#admin-update-check button')).toBeEnabled();
    await expect(page.locator('#admin-update-install')).toHaveCount(0);
    expect((await page.request.get(`${app.baseURL}/admin/updates/status`)).status()).toBe(200);
  } finally {await app.dispose();}
});

// Successful native transactions retain signed discovery metadata for the journal.
// The restarted application must compare that record to its own running version.
test('completed updates clear stale availability while rollback keeps newer release notice',async({page},info)=>{
  const app=await createStandaloneApp({admin:true,env:{RUSTCHAN_CONTAINER:'1'}});
  try {
    await adminLogin(page,app);
    for(const [phase,version,notice] of [['succeeded',packageVersion,false],['succeeded','1.5.0',false],['rolled_back',candidateVersion,true]] as const){
      await savedStatus(app,{phase,discovery:{available:{...available.available,version}},message:phase==='succeeded'?'RustChan updated successfully after health verification.':'Previous version restored after failed readiness.'});
      await page.goto(`${app.baseURL}/admin/panel?open=control-center#control-center`);
      await page.goto(`${app.baseURL}/admin/panel?open=software-updates#software-updates`);
      await expect(page.locator('#admin-update-message')).toContainText(phase==='succeeded'?'updated successfully':'Previous version restored');
      await page.screenshot({path:info.outputPath(`terminal-${phase}-${version}.png`),fullPage:true});
      const link=page.locator('.admin-section-index a[href="#software-updates"]');
      if(notice)await expect(link).toContainText('Update available');
      else await expect(link).not.toContainText('Update available');
      await expect(page.locator('#admin-update-install')).toHaveCount(0);
    }
  }finally{await app.dispose();}
});
