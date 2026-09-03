const { request } = require('@playwright/test');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const { spawn, spawnSync } = require('node:child_process');
const { once } = require('node:events');
const out = __dirname;
const checks = [];
function check(name, ok, detail) {
  if (!ok) throw new Error(name + ': ' + detail);
  checks.push({ name, detail });
  console.log('PASS', name, detail ?? '');
}
async function freePort() {
  const server = net.createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}
(async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rustchan-tls-audit-'));
  const data = path.join(root, 'data');
  fs.mkdirSync(data);
  const port = await freePort(), tlsPort = await freePort(), redirectPort = await freePort();
  fs.writeFileSync(path.join(data, 'settings.toml'), `cookie_secret = "${'b'.repeat(64)}"\n[tls]\nenabled = true\nrequire_https = true\nport = ${tlsPort}\nredirect_http = true\nhttp_port = ${redirectPort}\n`);
  const env = { ...process.env, RUST_LOG:'info', CHAN_HOST: '127.0.0.1', CHAN_BIND: `127.0.0.1:${port}`, CHAN_PORT: String(port), CHAN_TOR_SUPPORT: '0', CHAN_REQUIRE_FFMPEG: '0', CHAN_AUTO_FULL_BACKUP_HOURS: '0', CHAN_RATE_GETS:'1000', CHAN_HTTPS_COOKIES: '1', CHAN_PUBLIC_HOSTS: 'localhost,127.0.0.1' };
  const binary = path.resolve('target/debug/rustchan-cli');
  const log = fs.openSync(path.join(out, 'native-tls-server.log'), 'w');
  for (const args of [['admin','create-admin','admin','TlsAuditPass!'],['admin','create-board','tls','TLS Audit']]) {
    const result = spawnSync(binary, ['--data-dir', data, ...args], { env, stdio: ['ignore',log,log] });
    if (result.status !== 0) throw new Error('CLI setup failed');
  }
  const child = spawn(binary, ['--data-dir',data,'serve'], { env, stdio:['ignore',log,log] });
  const exit = once(child, 'exit');
  const http = await request.newContext({ ignoreHTTPSErrors: true });
  const base = `https://127.0.0.1:${tlsPort}`;
  try {
    let ready = false;
    for(let i=0;i<100;i++) {
      try { if ((await http.get(base + '/readyz', { timeout:1000 })).status() === 200) { ready=true; break; } } catch {}
      if (child.exitCode !== null) throw new Error('TLS startup exited');
      await new Promise(r=>setTimeout(r,100));
    }
    check('native self-signed HTTPS listener is ready', ready);
    let response = await http.get(base + '/admin');
    check('HTTPS admin login page', response.status()===200, response.status());
    const html = await response.text();
    const csrf = html.match(/name="_csrf"\s+value="([^"]+)"/)[1];
    const csrfCookie = response.headersArray().find(h=>h.name.toLowerCase()==='set-cookie' && h.value.startsWith('csrf_token='));
    check('native HTTPS creates Secure CSRF cookie', /;\s*Secure/.test(csrfCookie?.value));
    response = await http.post(base+'/admin/login',{ form:{username:'admin',password:'TlsAuditPass!',_csrf:csrf}, headers:{Origin:base, Referer:base+'/admin'}, maxRedirects:0 });
    check('admin login over native HTTPS', response.status()===303,response.status());
    const sessionCookie = response.headersArray().find(h=>h.name.toLowerCase()==='set-cookie' && h.value.startsWith('chan_admin_session='));
    check('native HTTPS creates Secure HttpOnly session', /;\s*Secure/.test(sessionCookie?.value) && /;\s*HttpOnly/.test(sessionCookie?.value));
    response = await http.get(base+response.headers().location);
    check('HTTPS session bootstrap reaches panel',response.status()===200 && (await response.text()).includes('admin-panel-logout'));
    response = await http.get(base+'/tls');
    const publicForm = (await response.text()).match(/<form[^>]*action="\/tls"[\s\S]*?<\/form>/)[0];
    const publicCsrf = publicForm.match(/name="_csrf"\s+value="([^"]+)"/)[1];
    response = await http.post(base+'/tls',{multipart:{_csrf:publicCsrf,submission_token:'native-tls-thread',body:'Posted through actual HTTPS'},maxRedirects:0});
    check('public posting through native HTTPS',response.status()===303,response.status());
    response = await http.get(base+response.headers().location);
    check('HTTPS thread content renders',response.status()===200 && (await response.text()).includes('Posted through actual HTTPS'));
    let plaintextRefused = false;
    try { await http.get(`http://127.0.0.1:${port}/tls`, { timeout:2000 }); } catch (error) { plaintextRefused = error.message.includes('ECONNREFUSED'); }
    check('HTTPS-only mode disables the main plaintext application listener', plaintextRefused);
    for (const p of [redirectPort]) {
      response = await http.get(`http://127.0.0.1:${p}/tls?audit=1`,{maxRedirects:0});
      check('HTTP port '+p+' redirects with preserved path/query', [301,302,307,308].includes(response.status()) && response.headers().location===base+'/tls?audit=1',response.headers().location);
      response = await http.get(`http://127.0.0.1:${p}/tls`,{headers:{Host:'evil.example'},maxRedirects:0});
      check('HTTP port '+p+' replaces untrusted Host with configured redirect host',response.status()===308 && response.headers().location===base+'/tls',response.headers().location);
    }
    fs.writeFileSync(path.join(out,'native-tls-results.json'),JSON.stringify({status:'PASS',root,ports:{port,tlsPort,redirectPort},checks},null,2));
  } finally {
    await http.dispose();
    child.kill('SIGTERM');
    const [code] = await exit;
    fs.closeSync(log);
    check('TLS instance shuts down cleanly',code===0,code);
  }
})().catch(error=>{console.error(error);process.exitCode=1;});
