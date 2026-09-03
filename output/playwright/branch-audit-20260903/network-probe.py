import contextlib
import hashlib
import html
import http.client
import http.cookies
import json
import os
import pathlib
import re
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.parse
import uuid

OUT = pathlib.Path(__file__).resolve().parent
BINARY = pathlib.Path('target/debug/rustchan-cli').resolve()
CHECKS = []
SECRET = 'a' * 64  # Disposable fixture only.

def check(name, condition, detail=None):
    if not condition:
        raise AssertionError(f'{name}: {detail}')
    CHECKS.append({'name': name, 'detail': detail})
    print('PASS', name, detail if detail is not None else '', flush=True)

class Client:
    def __init__(self, port, headers=None):
        self.port, self.cookies, self.headers = port, {}, headers or {}

    def request(self, path, form=None, fields=None, headers=None):
        supplied = dict(self.headers)
        supplied.update(headers or {})
        if self.cookies:
            supplied['Cookie'] = '; '.join(f'{k}={v}' for k,v in self.cookies.items())
        body = None
        if form is not None:
            body = urllib.parse.urlencode(form).encode()
            supplied['Content-Type'] = 'application/x-www-form-urlencoded'
        if fields is not None:
            boundary = 'audit-' + uuid.uuid4().hex
            body = b''.join(f'--{boundary}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode() for k,v in fields.items()) + f'--{boundary}--\r\n'.encode()
            supplied['Content-Type'] = f'multipart/form-data; boundary={boundary}'
        conn = http.client.HTTPConnection('127.0.0.1', self.port, timeout=20)
        conn.request('POST' if body is not None else 'GET', path, body=body, headers=supplied)
        response = conn.getresponse()
        status, pairs, body = response.status, response.getheaders(), response.read()
        conn.close()
        for key,value in pairs:
            if key.lower() == 'set-cookie':
                parsed = http.cookies.SimpleCookie(value)
                self.cookies.update({k:m.value for k,m in parsed.items()})
        return status, {k.lower():v for k,v in pairs}, body.decode(errors='replace'), pairs

    def token(self, path):
        status, _, text, _ = self.request(path)
        match = re.search(r'name="_csrf"\s+value="([^"]+)"', text)
        if not match:
            raise AssertionError(f'No CSRF at {path}: {status}')
        return html.unescape(match[1])

@contextlib.contextmanager
def instance(name, **overrides):
    root = pathlib.Path(tempfile.mkdtemp(prefix='rustchan-net-audit-'))
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
    env = dict(os.environ, CHAN_HOST='127.0.0.1', CHAN_PORT=str(port), CHAN_BIND=f'127.0.0.1:{port}', CHAN_TOR_SUPPORT='0', CHAN_REQUIRE_FFMPEG='0', CHAN_FFMPEG_PATH='__audit_missing_ffmpeg__', CHAN_FFPROBE_PATH='__audit_missing_ffprobe__', CHAN_HTTPS_COOKIES='1', CHAN_COOKIE_SECRET=SECRET, CHAN_PUBLIC_HOSTS='example.test,127.0.0.1,localhost', CHAN_AUTO_FULL_BACKUP_HOURS='0', CHAN_RATE_GETS='1000', CHAN_RATE_WINDOW='1', RUST_BACKTRACE='1')
    env.update(overrides)
    data = root / 'data'
    with (OUT / f'network-{name}.log').open('w') as log:
        for args in [['admin','create-admin','admin','AuditPassword!'], ['admin','create-board','net','Network audit']]:
            subprocess.run([str(BINARY),'--data-dir',str(data)]+args,env=env,stdout=log,stderr=log,check=True)
        proc = subprocess.Popen([str(BINARY),'--data-dir',str(data),'serve'],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log)
        try:
            for _ in range(100):
                try:
                    if Client(port).request('/readyz')[0] == 200: break
                except OSError: pass
                if proc.poll() is not None: raise RuntimeError('startup failed')
                time.sleep(.1)
            else: raise RuntimeError('readiness timeout')
            yield port, data
        finally:
            proc.terminate()
            proc.wait(timeout=10)
            check(f'{name}: clean SIGTERM exit', proc.returncode == 0, proc.returncode)
            (OUT / f'network-{name}-instance.json').write_text(json.dumps({'data': str(data), 'port':port}))

def post_identity(client, data, expected, label):
    token = client.token('/net')
    identity = f"visitor:{client.cookies['rustchan_visitor_id']}" if expected == '127.0.0.1' else expected
    status, _, _, _ = client.request('/net', fields={'_csrf':token,'submission_token':uuid.uuid4().hex,'body':label})
    check(label + ': post accepted', status == 303, status)
    with sqlite3.connect(data / 'chan.db') as db:
        ip_hash = db.execute('SELECT ip_hash FROM posts ORDER BY id DESC LIMIT 1').fetchone()[0]
    check(label + ': stored request identity', ip_hash == hashlib.sha256(f'{SECRET}:{identity}'.encode()).hexdigest(), expected)

def protect(client, data):
    csrf = client.token('/admin')
    status, headers, _, _ = client.request('/admin/login', form={'_csrf':csrf,'username':'admin','password':'AuditPassword!'})
    check('network fixture admin login', status == 303, status)
    csrf = client.token(headers['location'])
    with sqlite3.connect(data / 'chan.db') as db:
        board = db.execute("SELECT id FROM boards WHERE short_name='net'").fetchone()[0]
    status, _, _, _ = client.request('/admin/board/settings', form={'_csrf':csrf,'board_id':board,'name':'Network audit','description':'Password boundary','bump_limit':300,'max_threads':150,'max_archived_threads':150,'post_cooldown_secs':0,'max_image_size_mb':8,'max_video_size_mb':50,'max_audio_size_mb':150,'max_pdf_size_mb':8,'banner_mode':'inherit','access_mode':'view_password','access_password':'GatePassword!','allow_images':'1','allow_video':'1'})
    check('password board configured through admin HTTP form', status == 303, status)

try:
    for name, proxy, trusted, expected in [('direct','0','127.0.0.1/32','127.0.0.1'),('untrusted','1','192.0.2.0/24','127.0.0.1'),('trusted','1','127.0.0.1/32','203.0.113.8')]:
        with instance(name, CHAN_BEHIND_PROXY=proxy, CHAN_TRUSTED_PROXY_CIDRS=trusted) as (port,data):
            c = Client(port, {'X-Forwarded-For':'203.0.113.8, 198.51.100.4'})
            post_identity(c,data,expected,name+' X-Forwarded-For')
            c.headers['X-Real-IP']='198.51.100.77'
            post_identity(c,data,'198.51.100.77' if name=='trusted' else '127.0.0.1',name+' X-Real-IP')
            for https in [False,True]:
                h={'Host':'example.test'}
                if https: h['X-Forwarded-Proto']='https'
                status,headers,_,pairs=Client(port,h).request('/admin')
                cookie=next(v for k,v in pairs if k.lower()=='set-cookie' and v.startswith('csrf_token='))
                check(f'{name}: CSRF secure cookie with forwarded HTTPS={https}', ('; Secure' in cookie) == (name=='trusted' and https))
                check(f'{name}: HSTS with forwarded HTTPS={https}', ('strict-transport-security' in headers) == (name=='trusted' and https))
            if name=='trusted':
                admin=Client(port)
                protect(admin,data)
                c=Client(port,{'X-Forwarded-For':'203.0.113.20'})
                csrf=c.token('/net/unlock')
                for _ in range(2):
                    check('wrong board password denied before limit',c.request('/net/unlock',form={'_csrf':csrf,'password':'wrong'})[0]==403)
                status,_,_,pairs=c.request('/net/unlock',form={'_csrf':csrf,'password':'GatePassword!'})
                check('correct board unlock succeeds after failures',status==303,status)
                check('unlocked board is viewable',c.request('/net')[0]==200)
                c.cookies.pop('rustchan_board_access_net',None)
                for i in range(5):
                    status,headers,_,_=c.request('/net/unlock',form={'_csrf':csrf,'password':'wrong'})
                    check(f'unlock failure {i+1} after successful reset',status==(429 if i==4 else 403),status)
                check('board lockout advertises Retry-After',int(headers.get('retry-after','0'))>0)
                check('correct password cannot bypass active lockout',c.request('/net/unlock',form={'_csrf':csrf,'password':'GatePassword!'})[0]==429)
                other=Client(port,{'X-Forwarded-For':'203.0.113.21'})
                check('board lockout is isolated by trusted client IP',other.request('/net/unlock',form={'_csrf':other.token('/net/unlock'),'password':'GatePassword!'})[0]==303)
                with sqlite3.connect(data/'chan.db') as db:
                    db.execute('UPDATE admin_sessions SET expires_at=0')
                check('expired admin session denied',admin.request('/admin/panel')[0] in [302,303,403])
    with instance('rate',CHAN_RATE_GETS='4',CHAN_RATE_WINDOW='1',CHAN_BEHIND_PROXY='0') as (port,data):
        c=Client(port)
        statuses=[c.request('/net',headers={'X-Forwarded-For':f'203.0.113.{i}'})[0] for i in range(8)]
        check('request limiter rejects excess requests despite spoofed headers',statuses[-1]==429 and 200 in statuses,statuses)
        check('rate-limited clients retain static asset access',c.request('/static/style.css')[0]==200)
        time.sleep(2.2)
        check('request limiter recovers after configured window',c.request('/net')[0]==200)
    (OUT/'network-results.json').write_text(json.dumps({'status':'PASS','checks':CHECKS},indent=2))
except Exception as error:
    (OUT/'network-results.json').write_text(json.dumps({'status':'FAIL','error':str(error),'checks':CHECKS},indent=2))
    raise
