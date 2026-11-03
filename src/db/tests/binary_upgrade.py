#!/usr/bin/env python3
"""Bounded, synthetic 1.7.1 binary-only upgrade smoke; uses no operator data.

--output is a directory for a JSON report and sanitized process logs. All test
installations and the stopped rollback copy are removed, including on failure.
On Linux, --manual-linux-root verifies the protected/check-only root context.
--managed-linux-baseline verifies an ordinary-user 1.7.1 installation's real
controller state is preserved when its executable is replaced by a new check-only
release. Neither mode installs a signed managed update or bypasses old admission.
Graceful shutdown uses SIGINT on POSIX and an isolated console on Windows.
"""

import argparse
import collections
import contextlib
import hashlib
import html.parser
import http.cookiejar
import json
import os
import platform
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import sqlite3
import stat
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zlib


class VerificationError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise VerificationError(message)


def digest(value):
    return hashlib.sha256(value).hexdigest()


@contextlib.contextmanager
def database_connection(database, readonly=False):
    connection = (sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)
                  if readonly else sqlite3.connect(database))
    try:
        with connection:
            yield connection
    finally:
        connection.close()


def png(width=468, height=60):
    def chunk(kind, value):
        return (struct.pack(">I", len(value)) + kind + value
                + struct.pack(">I", zlib.crc32(kind + value)))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress((b"\0" + b"\x40\x80\xc0" * width) * height))
            + chunk(b"IEND", b""))


class Inputs(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.tokens = []

    def handle_starttag(self, tag, attributes):
        values = dict(attributes)
        if tag == "input" and values.get("name") == "_csrf" and values.get("value"):
            self.tokens.append(values["value"])


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


class Client:
    def __init__(self, origin):
        self.origin = origin
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(self.cookies), NoRedirect())

    def request(self, route, fields=None, multipart=None, timeout=45):
        require(route.startswith("/") and not route.startswith("//"), "unsafe fixture route")
        headers = {"Accept-Encoding": "identity", "User-Agent": "RustChan-upgrade-smoke"}
        body = None
        if fields is not None or multipart is not None:
            headers.update({"Origin": self.origin, "Referer": self.origin + "/admin/panel",
                            "Sec-Fetch-Site": "same-origin"})
            if multipart is None:
                body = urllib.parse.urlencode(fields).encode()
                headers["Content-Type"] = "application/x-www-form-urlencoded"
            else:
                boundary = "UpgradeFixture" + secrets.token_hex(12)
                chunks = []
                for name, value in fields.items():
                    chunks.extend([("--" + boundary + "\r\nContent-Disposition: form-data; "
                                    'name="' + name + '"\r\n\r\n').encode(),
                                   str(value).encode(), b"\r\n"])
                chunks.extend([("--" + boundary + "\r\nContent-Disposition: form-data; "
                                'name="banner"; filename="fixture.png"\r\n'
                                "Content-Type: image/png\r\n\r\n").encode(), multipart,
                               ("\r\n--" + boundary + "--\r\n").encode()])
                body = b"".join(chunks)
                headers["Content-Type"] = "multipart/form-data; boundary=" + boundary
        request = urllib.request.Request(self.origin + route, data=body, headers=headers)
        try:
            response = self.opener.open(request, timeout=timeout)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            content = response.read(8 * 1024 * 1024 + 1)
            require(len(content) <= 8 * 1024 * 1024, "fixture HTTP response exceeded bound")
            return response.code, dict(response.headers.items()), content

    def csrf(self, route="/admin/panel"):
        status, _, content = self.request(route)
        require(status == 200, "CSRF form could not be read: " + route + " status=" + str(status))
        parser = Inputs()
        parser.feed(content.decode("utf-8"))
        require(parser.tokens, "CSRF form missing its token: " + route)
        return parser.tokens[0]

    def login(self, username, password):
        token = self.csrf("/admin")
        status, headers, _ = self.request("/admin/login", {
            "username": username, "password": password, "_csrf": token})
        require(status in (302, 303), "valid administrator login was rejected")
        location = headers.get("Location", headers.get("location", ""))
        # Bootstrap URLs contain a one-use secret; never include them in output.
        for _ in range(4):
            require(location.startswith("/") and not location.startswith("//"),
                    "administrator login returned an unexpected redirect")
            status, headers, content = self.request(location)
            if status == 200:
                require(b"/admin/logout" in content, "login did not reach authenticated panel")
                return
            require(status in (302, 303), "administrator bootstrap failed")
            location = headers.get("Location", headers.get("location", ""))
        raise VerificationError("administrator login redirect limit exceeded")


def rows(database):
    """Compare business values, not SQLite pages, derived FTS layout or builtin CSS."""
    result = {}
    with database_connection(database, readonly=True) as connection:
        tables = connection.execute("SELECT name FROM sqlite_schema WHERE type='table'").fetchall()
        for (name,) in tables:
            if name.startswith(("sqlite_", "posts_fts")) or name == "schema_version":
                continue
            quoted = '"' + name.replace('"', '""') + '"'
            selection = " WHERE is_builtin=0" if name == "themes" else ""
            encoded = [json.dumps(row, ensure_ascii=True, separators=(",", ":"))
                       for row in connection.execute("SELECT * FROM " + quoted + selection)]
            result[name] = collections.Counter(encoded)
    return result


def preserved(before, after, exact=False):
    for table, original in before.items():
        require(table in after, "business table missing after operation: " + table)
        require(not (original - after[table]), "retained business values changed: " + table)
    if exact:
        require(before == after, "restart changed business rows")


def inventory(data, stable=True):
    result = {}
    for entry in sorted(data.rglob("*")):
        relative = entry.relative_to(data).as_posix()
        if stable:
            if relative.startswith(("logs/", ".software-updates/")):
                continue
            if relative in ("chan.db", "chan.db-wal", "chan.db-shm", ".settings.lock"):
                continue
            if relative.startswith("runtime/") and not relative.startswith(
                    ("runtime/banner/", "runtime/tor/", "runtime/tls/")):
                continue
        if entry.is_symlink():
            target = Path(os.readlink(entry))
            require(not target.is_absolute() and entry.resolve().is_relative_to(data.resolve()),
                    "synthetic symlink leaves its owned data tree")
            result[relative] = "symlink:" + str(target)
            continue
        if not entry.is_file():
            continue
        result[relative] = digest(entry.read_bytes())
    return result


def files_preserved(before, after, exact=False):
    require(all(after.get(name) == value for name, value in before.items()),
            "a retained stable file was removed or changed")
    if exact:
        require(before == after, "stopped full-copy backup differed from its source")


def inspect_database(database, expected_version):
    with database_connection(database, readonly=True) as connection:
        require(connection.execute("PRAGMA integrity_check").fetchall() == [("ok",)],
                "SQLite integrity check failed")
        require(connection.execute("PRAGMA foreign_key_check").fetchall() == [],
                "SQLite foreign key check failed")
        actual = connection.execute("SELECT version FROM schema_version").fetchall()
        require(actual == [(expected_version,)], "automatic schema version stamp was incorrect")


class Smoke:
    def __init__(self, args, temporary, logs):
        self.args, self.temporary, self.logs = args, temporary, logs
        self.process = None
        self.log_handle = None
        self.log_paths = []
        self.checks = []
        self.password = "Upgrade-fixture-only-" + secrets.token_hex(12)
        self.username = "upgrade_fixture_admin"
        self.media = png(24, 16)
        self.media_name = "8e" * 16 + ".png"
        self.port = self.free_port()
        self.origin = "http://127.0.0.1:" + str(self.port)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("CHAN_", "RUSTCHAN_"))}
        self.env.update({"CHAN_HOST": "127.0.0.1", "CHAN_TOR_SUPPORT": "false",
                         "CHAN_TOR_ONLY": "false", "CHAN_HTTPS_COOKIES": "false",
                         "CHAN_COOKIE_SECRET": "42" * 32,
                         "CHAN_AUTO_FULL_BACKUP_HOURS": "0",
                         "CHAN_MEDIA_AUTO_PRUNE_ENABLED": "false",
                         "CHAN_RATE_GETS": "10000", "RUST_LOG": "info"})

    @staticmethod
    def free_port():
        try:
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                return listener.getsockname()[1]
        except PermissionError:
            raise VerificationError("execution environment denied loopback socket allocation at 127.0.0.1:0") from None

    def check(self, name):
        self.checks.append(name)

    def cli(self, binary, arguments, label):
        log = self.temporary / (label + ".log")
        self.log_paths.append(log)
        with log.open("wb") as output:
            try:
                result = subprocess.run([str(binary), *arguments], cwd=binary.parent,
                                        env=self.env, stdin=subprocess.DEVNULL,
                                        stdout=output, stderr=subprocess.STDOUT, timeout=60)
            except subprocess.TimeoutExpired:
                raise VerificationError("CLI timed out: " + label) from None
        require(result.returncode == 0, "CLI failed: " + label)

    def seed_cli(self, binary, prefix):
        self.cli(binary, ["admin", "create-admin", self.username, self.password], prefix + "-admin")
        self.cli(binary, ["admin", "create-board", "test", "Upgrade fixture",
                          "Synthetic retained board"], prefix + "-board")

    def launch(self, binary, label):
        require(self.process is None, "an owned server is already running")
        log = self.temporary / (label + ".log")
        self.log_paths.append(log)
        self.log_handle = log.open("wb")
        self.process = subprocess.Popen([str(binary), "--port", str(self.port), "serve"],
                                        cwd=binary.parent, env=self.env, stdin=subprocess.DEVNULL,
                                        stdout=self.log_handle, stderr=subprocess.STDOUT,
                                        creationflags=subprocess.CREATE_NEW_CONSOLE if os.name == "nt" else 0)
        return log

    def ready(self, expected_version, timeout=45):
        client = Client(self.origin)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "server exited before becoming ready")
            try:
                status, _, content = client.request("/readyz", timeout=1)
                if status == 200:
                    value = json.loads(content)
                    require(value.get("version") == expected_version,
                            "running server version did not match requested executable")
                    # The old Linux supervisor can expose /readyz before its
                    # controller admits ordinary application requests.
                    admin_status = client.request("/admin", timeout=1)[0]
                    if admin_status == 200:
                        return
                    require(admin_status == 503,
                            "administrator route unavailable after readiness: " + str(admin_status))
            except (urllib.error.URLError, TimeoutError, ConnectionError, json.JSONDecodeError):
                pass
            time.sleep(0.1)
        raise VerificationError("server readiness timed out")

    def stop(self, strict=True):
        if self.process is None:
            return
        process = self.process
        forced = False
        try:
            if process.poll() is None:
                if os.name == "nt":
                    helper = r"""
import ctypes, sys, time
kernel = ctypes.WinDLL("kernel32", use_last_error=True)
kernel.FreeConsole()
if not kernel.AttachConsole(int(sys.argv[1])):
    raise ctypes.WinError(ctypes.get_last_error())
if not kernel.SetConsoleCtrlHandler(None, True):
    raise ctypes.WinError(ctypes.get_last_error())
if not kernel.GenerateConsoleCtrlEvent(0, 0):
    raise ctypes.WinError(ctypes.get_last_error())
time.sleep(0.2)
"""
                    try:
                        result = subprocess.run([sys.executable, "-c", helper, str(process.pid)],
                                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                                stderr=subprocess.PIPE, timeout=10)
                        delivered = result.returncode == 0
                        if not delivered and self.log_handle is not None:
                            self.log_handle.write(b"Windows console signal failed: " + result.stderr[:2048])
                    except (OSError, subprocess.TimeoutExpired):
                        delivered = False
                    if not delivered:
                        # Failed validation must still stop the owned process so
                        # cleanup can release its files. Never count this as a pass.
                        forced = True
                        process.terminate()
                else:
                    process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    forced = True
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
            if strict:
                require(not forced and process.returncode == 0,
                        "owned server did not exit cleanly after its graceful shutdown signal")
        finally:
            if process.poll() is not None:
                self.process = None
            if self.log_handle is not None:
                self.log_handle.close()
                self.log_handle = None

    def rejected_start(self, binary, label, diagnostic):
        log = self.launch(binary, label)
        deadline = time.monotonic() + 45
        while self.process.poll() is None and time.monotonic() < deadline:
            try:
                require(Client(self.origin).request("/readyz", timeout=0.5)[0] != 200,
                        "invalid database was served as ready")
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                pass
            time.sleep(0.1)
        require(self.process.poll() is not None and self.process.returncode != 0,
                "invalid database was not rejected at startup")
        self.stop(strict=False)
        require(re.search(diagnostic, log.read_text(errors="replace"), re.I),
                "startup failure lacked the expected database diagnostic")

    @staticmethod
    def install(directory, source):
        directory.mkdir()
        binary = directory / ("rustchan-cli.exe" if os.name == "nt" else "rustchan-cli")
        shutil.copy2(source, binary)
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        return binary, directory / "rustchan-data"

    def seed_sql(self, data):
        database = data / "chan.db"
        now = int(time.time())
        with database_connection(database) as connection:
            connection.execute("PRAGMA foreign_keys=ON")
            board = connection.execute("SELECT id FROM boards WHERE short_name='test'").fetchone()[0]
            admin, password_hash = connection.execute(
                "SELECT id,password_hash FROM admin_users WHERE username=?", (self.username,)).fetchone()
            connection.execute("INSERT INTO boards(short_name,name,access_mode,access_password_hash) "
                               "VALUES('private','Retained private board','view_password',?)", (password_hash,))
            thread = connection.execute("INSERT INTO threads(board_id,subject,reply_count) VALUES(?,?,1)",
                                        (board, "Retained upgrade thread")).lastrowid
            original = connection.execute(
                "INSERT INTO posts(thread_id,board_id,body,body_html,ip_hash,deletion_token,is_op,"
                "file_path,file_name,file_size,mime_type,media_type) VALUES(?,?,?,?,?,?,1,?,?,?,?,?)",
                (thread, board, "upgrade-retained-original", "<p>upgrade-retained-original</p>",
                 "ab" * 32, secrets.token_hex(32), "test/" + self.media_name,
                 "fixture.png", len(self.media), "image/png", "image")).lastrowid
            reply = connection.execute(
                "INSERT INTO posts(thread_id,board_id,body,body_html,ip_hash,deletion_token,is_op) "
                "VALUES(?,?,?,?,?,?,0)", (thread, board, "upgrade-retained-reply",
                                           "<p>upgrade-retained-reply</p>", "ab" * 32,
                                           secrets.token_hex(32))).lastrowid
            connection.execute("INSERT INTO file_hashes(sha256,file_path,thumb_path,mime_type) "
                               "VALUES(?,?,?,'image/png')", (digest(self.media),
                                                            "test/" + self.media_name, ""))
            connection.execute("INSERT INTO admin_sessions(id,admin_id,expires_at) VALUES(?,?,?)",
                               (secrets.token_hex(32), admin, now + 86400))
            connection.execute("INSERT INTO site_settings(key,value) VALUES('upgrade-fixture','retained-value')")
            connection.execute("INSERT INTO themes(slug,display_name,custom_css) "
                               "VALUES('upgrade-fixture','Fixture theme','body{--fixture-color:#123456}')")
            connection.execute("INSERT INTO word_filters(pattern,replacement) VALUES('retained-filter','kept')")
            connection.execute("INSERT INTO bans(ip_hash,reason,expires_at) VALUES(?,?,?)",
                               ("cd" * 32, "Retained fixture ban", now + 86400))
            connection.execute("INSERT INTO ban_appeals(ip_hash,reason) VALUES(?,?)",
                               ("cd" * 32, "Retained fixture appeal"))
            poll = connection.execute("INSERT INTO polls(thread_id,question,expires_at) VALUES(?,?,?)",
                                      (thread, "Retained poll?", now + 86400)).lastrowid
            option = connection.execute("INSERT INTO poll_options(poll_id,text) VALUES(?,?)",
                                        (poll, "Yes")).lastrowid
            connection.execute("INSERT INTO poll_options(poll_id,text,position) VALUES(?,?,1)", (poll, "No"))
            connection.execute("INSERT INTO poll_votes(poll_id,option_id,ip_hash) VALUES(?,?,?)",
                               (poll, option, "ab" * 32))
            connection.execute("INSERT INTO reports(post_id,thread_id,board_id,reason,reporter_hash) "
                               "VALUES(?,?,?,?,?)", (reply, thread, board, "Retained report", "ef" * 32))
            connection.execute("INSERT INTO user_thread_preferences(user_hash,thread_id,pinned) VALUES(?,?,1)",
                               ("ab" * 32, thread))
            connection.execute("INSERT INTO post_submissions(submission_token,ip_hash,board_id,thread_id,"
                               "post_id,is_thread) VALUES(?,?,?,?,?,1)",
                               (secrets.token_hex(32), "ab" * 32, board, thread, original))
            payload = json.dumps({"t": "SpamCheck", "d": {
                "post_id": original, "ip_hash": "ab" * 32, "body_len": 25}})
            connection.execute("INSERT INTO background_jobs(job_type,payload,status,attempts) "
                               "VALUES('spam_check',?,'done',1)", (payload,))
            connection.execute("INSERT INTO chan_net_import_ledger(tx_id) VALUES('retained-historical-ledger')")
        media_directory = data / "boards" / "test"
        media_directory.mkdir(parents=True, exist_ok=True)
        (media_directory / self.media_name).write_bytes(self.media)
        for subsystem in ("tor", "tls"):
            marker = data / "runtime" / subsystem / "upgrade-fixture-preserved.bin"
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_bytes(("synthetic " + subsystem + " retained bytes\n").encode())
        settings = data / "settings.toml"
        text = settings.read_text()
        value = 'forum_name = "Upgrade fixture retained configuration"'
        text, count = re.subn(r"(?m)^forum_name\s*=.*$", value, text)
        if not count:
            text += "\n" + value + "\n"
        settings.write_text(text)
        return thread

    def readable(self, client, thread, banner_id, banner_hash):
        status, _, content = client.request("/test/thread/" + str(thread))
        require(status == 200 and b"upgrade-retained-original" in content
                and b"upgrade-retained-reply" in content, "retained posts were not readable")
        status, _, content = client.request("/boards/test/" + self.media_name)
        require(status == 200 and digest(content) == digest(self.media), "retained media was not readable")
        status, _, content = client.request("/banner/assets/" + str(banner_id))
        require(status == 200 and digest(content) == banner_hash, "retained banner was not readable")
        public = Client(self.origin)
        status, _, content = public.request("/private")
        require(status == 403 and b"/private/unlock" in content,
                "retained board access policy was not enforced")

    @staticmethod
    def completed_pending_job(before, data, pending_job):
        original = [json.loads(encoded) for encoded, count in before["background_jobs"].items()
                    if json.loads(encoded)[0] == pending_job and count == 1]
        require(len(original) == 1, "known pending job identity was not unique")
        deadline = time.monotonic() + 15
        while True:
            with database_connection(data / "chan.db", readonly=True) as connection:
                columns = [row[1] for row in connection.execute("PRAGMA table_info(background_jobs)")]
                actual = connection.execute("SELECT * FROM background_jobs WHERE id=?", (pending_job,)).fetchone()
            require(actual is not None and len(actual) == len(original[0]), "pending job shape or identity changed")
            previous, current = dict(zip(columns, original[0])), dict(zip(columns, actual))
            require(previous["status"] == "pending", "exception was not the known pre-upgrade pending job")
            if current["status"] == "done":
                require(current["attempts"] == previous["attempts"] + 1 and current["last_error"] is None
                        and current["updated_at"] >= previous["updated_at"], "pending job did not complete exactly once")
                allowed = {"status", "attempts", "last_error", "updated_at"}
                require(all(current[key] == value for key, value in previous.items() if key not in allowed),
                        "pending job identity or payload changed")
                expected = {table: values.copy() for table, values in before.items()}
                old = json.dumps(original[0], ensure_ascii=True, separators=(",", ":"))
                new = json.dumps(actual, ensure_ascii=True, separators=(",", ":"))
                expected["background_jobs"][old] -= 1
                expected["background_jobs"][new] += 1
                expected["background_jobs"] += collections.Counter()
                return expected
            require(time.monotonic() < deadline, "pending old-version job did not finish after upgrade")
            time.sleep(0.1)

    @staticmethod
    def controller_preserved(data, original):
        state = data / ".software-updates"
        if original is None:
            require(not state.exists(), "candidate unexpectedly created managed controller state")
        else:
            files_preserved(original, inventory(state, stable=False), exact=True)

    def run(self):
        binary, data = self.install(self.temporary / "installed", self.args.old_bin)
        self.seed_cli(binary, "old")
        thread = self.seed_sql(data)
        self.launch(binary, "old-server")
        self.ready("1.7.1")
        client = Client(self.origin)
        client.login(self.username, self.password)
        status, _, _ = client.request("/admin/home/banner", {
            "_csrf": client.csrf(), "target_type": "none", "enabled": "1",
            "show_on_index": "1", "show_on_catalog": "1"}, multipart=png())
        require(status == 303, "old binary could not create its own banner")
        with database_connection(data / "chan.db") as connection:
            banner_id = connection.execute("SELECT id FROM banner_assets WHERE scope_type='home'").fetchone()[0]
        status, _, content = client.request("/banner/assets/" + str(banner_id))
        require(status == 200, "old binary could not read its own banner")
        banner_hash = digest(content)
        self.readable(client, thread, banner_id, banner_hash)
        status, _, _ = client.request("/admin/backup/create", {
            "_csrf": client.csrf(), "storage_mode": "directory"}, timeout=60)
        require(status == 303, "old binary could not create a saved full backup")
        require(any(entry.is_file() for entry in (data / "backups").rglob("*")),
                "old binary did not publish its saved backup")
        self.stop()
        with database_connection(data / "chan.db") as connection:
            require(connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0] == 0,
                    "stopped old database checkpoint could not complete")
        inspect_database(data / "chan.db", "1.7.1")
        with database_connection(data / "chan.db") as connection:
            payload = json.dumps({"t": "SpamCheck", "d": {"post_id": 1, "ip_hash": "ab" * 32, "body_len": 25}})
            pending_job = connection.execute("INSERT INTO background_jobs(job_type,payload,status) VALUES('spam_check',?,'pending')", (payload,)).lastrowid
        original_rows, original_files = rows(data / "chan.db"), inventory(data)
        state = data / ".software-updates"
        controller_files = None
        if self.args.managed_linux_baseline:
            require((state / "origin.json").is_file(), "old ordinary-user Linux did not retain its controller origin")
            origin = json.loads((state / "origin.json").read_text())
            require(origin.get("version") == "1.7.1" and origin.get("program") == str(binary),
                    "old managed controller origin did not match the actual installed baseline")
            controller_files = inventory(state, stable=False)
            require(controller_files, "old managed controller state was empty")
            self.check("genuine_ordinary_user_linux_171_controller_retention")
        else:
            require(not state.exists(), "unexpected managed context; choose explicit managed Linux baseline mode")
        self.check("old_171_cli_http_seed_and_clean_shutdown")

        backup = self.temporary / "stopped-backup"
        shutil.copytree(data, backup, symlinks=True)
        files_preserved(inventory(data, stable=False), inventory(backup, stable=False), exact=True)
        preserved(original_rows, rows(backup / "chan.db"), exact=True)
        self.check("complete_stopped_copy_backup_matches_all_files_and_business_rows")

        original_stat = binary.stat()
        replacement = binary.with_name(".replacement")
        shutil.copy2(self.args.new_bin, replacement)
        replacement.chmod(stat.S_IMODE(original_stat.st_mode))
        os.replace(replacement, binary)
        require(binary.stat().st_uid == original_stat.st_uid, "binary owner changed")
        require(digest(binary.read_bytes()) == digest(self.args.new_bin.read_bytes()),
                "installed candidate bytes did not match supplied executable")
        preserved(original_rows, rows(data / "chan.db"), exact=True)
        files_preserved(original_files, inventory(data), exact=True)
        self.check("only_same_binary_path_replaced_before_candidate_start")

        self.launch(binary, "candidate-server")
        self.ready(self.args.expected_version)
        inspect_database(data / "chan.db", self.args.expected_version)
        expected_candidate_rows = self.completed_pending_job(original_rows, data, pending_job)
        self.check("pending_old_version_job_resumes_without_losing_its_identity_or_payload")
        preserved(expected_candidate_rows, rows(data / "chan.db"))
        files_preserved(original_files, inventory(data))
        self.controller_preserved(data, controller_files)
        # Reuse the old authenticated cookie jar before any new login: sessions
        # must survive the executable swap, independently of password verification.
        require(client.request("/admin/panel")[0] == 200, "old authenticated session was lost")
        self.readable(client, thread, banner_id, banner_hash)
        self.cli(binary, ["admin", "db-status"], "candidate-live-db-status")
        new_login = Client(self.origin)
        new_login.login(self.username, self.password)
        unauthenticated = Client(self.origin)
        require(unauthenticated.request("/admin/updates/status")[0] == 403, "version metadata leaked without authentication")
        require(unauthenticated.request("/admin/updates/check", {"_csrf": "invalid"})[0] == 403, "unauthenticated version check was accepted")
        require(new_login.request("/admin/updates/check", {"_csrf": "invalid"})[0] == 403, "version check bypassed signed CSRF")
        require(new_login.request("/admin/updates/check", {"_csrf": new_login.csrf(), "url": "https://foreign.invalid/"})[0] == 422, "version check accepted a supplied URL")
        require(new_login.request("/admin/updates/install", {"_csrf": new_login.csrf()})[0] == 404, "removed automatic install route still exists")
        require(new_login.request("/admin/restart", {"_csrf": new_login.csrf()})[0] == 404, "removed automatic restart route still exists")
        status, _, updates_page = new_login.request("/admin/panel?open=software-updates")
        require(status == 200 and b"/admin/updates/check" in updates_page and b"Download from GitHub releases" in updates_page
                and b"/admin/updates/install" not in updates_page, "manual update page was incomplete")
        self.check("version_check_auth_csrf_fixed_url_and_removed_execution_boundaries")
        status, _, _ = new_login.request("/admin/filter/add", {
            "_csrf": new_login.csrf(), "pattern": "candidate-upgrade-probe", "replacement": "accepted"})
        require(status == 303, "normal authenticated candidate mutation was rejected")
        with database_connection(data / "chan.db") as connection:
            require(connection.execute("SELECT COUNT(*) FROM word_filters WHERE pattern=?",
                                       ("candidate-upgrade-probe",)).fetchone()[0] == 1,
                    "candidate mutation was not persisted")
        self.stop()
        inspect_database(data / "chan.db", self.args.expected_version)
        preserved(expected_candidate_rows, rows(data / "chan.db"))
        files_preserved(original_files, inventory(data))
        self.controller_preserved(data, controller_files)
        self.check("automatic_stamp_integrity_fk_rows_files_old_session_new_login_and_mutation")

        restart_rows, restart_files = rows(data / "chan.db"), inventory(data)
        self.launch(binary, "candidate-restart")
        self.ready(self.args.expected_version)
        require(new_login.request("/admin/panel")[0] == 200, "candidate session was lost on restart")
        self.readable(new_login, thread, banner_id, banner_hash)
        self.stop()
        inspect_database(data / "chan.db", self.args.expected_version)
        preserved(restart_rows, rows(data / "chan.db"), exact=True)
        files_preserved(restart_files, inventory(data), exact=True)
        self.controller_preserved(data, controller_files)
        self.check("repeated_restart_is_idempotent")

        fresh_binary, fresh_data = self.install(self.temporary / "fresh", self.args.new_bin)
        self.seed_cli(fresh_binary, "fresh")
        self.launch(fresh_binary, "fresh-server")
        self.ready(self.args.expected_version)
        Client(self.origin).login(self.username, self.password)
        self.stop()
        inspect_database(fresh_data / "chan.db", self.args.expected_version)
        self.controller_preserved(fresh_data, None)
        with database_connection(fresh_data / "chan.db") as connection:
            require(connection.execute("SELECT COUNT(*) FROM posts").fetchone()[0] == 0,
                    "fresh install contained old fixture posts")
        self.check("fresh_candidate_install_cli_http_login_and_integrity")

        broken_binary, broken_data = self.install(self.temporary / "unknown-schema", self.args.new_bin)
        shutil.copytree(backup, broken_data, symlinks=True)
        with database_connection(broken_data / "chan.db") as connection:
            connection.execute("CREATE TABLE upgrade_unknown_schema_fixture(value TEXT)")
            connection.execute("INSERT INTO upgrade_unknown_schema_fixture VALUES('must remain intact')")
        broken_rows, broken_files = rows(broken_data / "chan.db"), inventory(broken_data)
        self.rejected_start(broken_binary, "unknown-schema-server", r"unexpected table upgrade_unknown_schema_fixture")
        inspect_database(broken_data / "chan.db", "1.7.1")
        preserved(broken_rows, rows(broken_data / "chan.db"), exact=True)
        files_preserved(broken_files, inventory(broken_data), exact=True)
        self.controller_preserved(broken_data, controller_files)
        self.check("unknown_schema_fails_closed_without_stamping_or_data_loss")

        corrupt_binary, corrupt_data = self.install(self.temporary / "corrupt-database", self.args.new_bin)
        shutil.copytree(backup, corrupt_data, symlinks=True)
        database = corrupt_data / "chan.db"
        original_bytes = database.read_bytes()
        database.write_bytes(b"invalid sqlite!!" + original_bytes[16:])
        corrupt_hash, corrupt_files = digest(database.read_bytes()), inventory(corrupt_data)
        self.rejected_start(corrupt_binary, "corrupt-database-server", r"not a database|malformed|database disk image")
        require(digest(database.read_bytes()) == corrupt_hash,
                "corrupt database input bytes were overwritten")
        files_preserved(corrupt_files, inventory(corrupt_data), exact=True)
        self.controller_preserved(corrupt_data, controller_files)
        inspect_database(backup / "chan.db", "1.7.1")
        preserved(original_rows, rows(backup / "chan.db"), exact=True)
        self.check("corrupt_database_fails_closed_preserving_input_and_recoverable_stopped_backup")

        # Restore the entire stopped old data copy, including every runtime file,
        # before starting the old binary. Never open a newly stamped DB with it.
        shutil.rmtree(data)
        shutil.copytree(backup, data, symlinks=True)
        shutil.copy2(self.args.old_bin, binary)
        files_preserved(inventory(backup, stable=False), inventory(data, stable=False), exact=True)
        self.launch(binary, "rollback-old-server")
        self.ready("1.7.1")
        rollback_client = Client(self.origin)
        rollback_client.login(self.username, self.password)
        self.readable(rollback_client, thread, banner_id, banner_hash)
        expected_rollback_rows = self.completed_pending_job(original_rows, data, pending_job)
        self.stop()
        inspect_database(data / "chan.db", "1.7.1")
        preserved(expected_rollback_rows, rows(data / "chan.db"))
        files_preserved(original_files, inventory(data))
        self.check("stopped_backup_rollback_to_old_binary_is_readable_and_authenticates")

        return {"baseline_business_tables": {
                    table: {"rows": sum(values.values()),
                            "sha256": digest(json.dumps(sorted(values.items())).encode())}
                    for table, values in sorted(original_rows.items())},
                "baseline_stable_file_count": len(original_files),
                "baseline_stable_file_manifest_sha256": digest(json.dumps(original_files, sort_keys=True).encode()),
                "baseline_stopped_backup_file_count": len(inventory(backup, stable=False)),
                "legacy_controller_files_preserved": len(controller_files) if controller_files is not None else None,
                "fixture": {"seeded_via_cli": ["admin", "board"],
                            "seeded_via_http": ["login", "home banner", "saved directory full backup"],
                            "seeded_via_valid_offline_sql": ["posts", "media references", "protected board", "polls",
                                "reports", "filters", "bans", "appeals", "sessions", "terminal job", "custom theme",
                                "settings", "submission receipt", "thread preference", "historical ledger"],
                            "runtime_markers": "synthetic Tor/TLS byte retention; no live identity/bootstrap test"}}

    def save_logs(self):
        self.logs.mkdir(parents=True, exist_ok=True)
        for log in self.log_paths:
            if not log.exists():
                continue
            text = log.read_text(errors="replace").replace(self.password, "[redacted-fixture-password]")
            text = re.sub(r"(?i)(bootstrap(?:=|%3D))[^\s&<>\"']+", r"\1[redacted]", text)
            text = re.sub(r"(?i)\b[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b",
                          "[redacted-identifier]", text)
            text = re.sub(r"(?i)\b[0-9a-f]{32,}\b", "[redacted-token-or-hash]", text)
            (self.logs / log.name).write_text(text)


def version(binary):
    try:
        result = subprocess.run([str(binary), "--version"], stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15,
                                env={key: value for key, value in os.environ.items()
                                     if not key.startswith(("CHAN_", "RUSTCHAN_"))})
    except (OSError, subprocess.TimeoutExpired):
        raise VerificationError("supplied binary could not report its version") from None
    require(result.returncode == 0, "supplied binary rejected --version")
    match = re.fullmatch(rb"rustchan-cli ([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)\s*", result.stdout)
    require(match is not None, "supplied binary returned an unexpected version format")
    return match.group(1).decode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-bin", type=Path, required=True)
    parser.add_argument("--new-bin", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--output", type=Path, required=True, help="directory for JSON report and sanitized logs")
    linux_context = parser.add_mutually_exclusive_group()
    linux_context.add_argument("--manual-linux-root", action="store_true",
                              help="require actual Linux root; verifies protected manual context")
    linux_context.add_argument("--managed-linux-baseline", action="store_true",
                              help="require ordinary-user Linux 1.7.1 retention, preserve it with check-only candidate")
    args = parser.parse_args()
    args.old_bin, args.new_bin, args.output = args.old_bin.resolve(), args.new_bin.resolve(), args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    run_id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + secrets.token_hex(3)
    report_path = args.output / (run_id + "-upgrade.json")
    report = {"success": False, "expected_version": args.expected_version,
              "platform": sys.platform, "os": platform.platform(), "machine": platform.machine(), "checks": [], "temporary_installations_removed": False,
              "limits": ["synthetic bounded fixture, not arbitrary operator database certification",
                         "native HTTP client with correct Origin/Referer; browser CSRF reproduction is separate",
                         "no live Tor/TLS service, media transcoding, signed managed update or cross-OS execution",
                         "stable logical rows exclude schema_version, derived FTS and builtin theme definitions; only the known pending job may complete exactly once",
                         "stable files exclude DB pages/WAL/SHM, logs, locks and other volatile runtime metadata"]}
    smoke, temporary = None, None
    exit_code = 1
    try:
        require(os.name in ("posix", "nt"), "unsupported process and shutdown platform")
        linux = sys.platform.startswith("linux")
        require(not linux or os.geteuid() == 0 or args.managed_linux_baseline,
                "ordinary-user Linux requires explicit --managed-linux-baseline")
        require(not args.manual_linux_root or (linux and os.geteuid() == 0),
                "--manual-linux-root requires Linux and actual root privileges")
        require(not args.managed_linux_baseline or (linux and os.geteuid() != 0),
                "--managed-linux-baseline requires Linux and an actual ordinary user")
        report["deployment_context"] = ("linux-ordinary-171-managed-to-check-only" if args.managed_linux_baseline
                                        else "linux-protected-root-manual" if linux
                                        else "native-windows-manual" if os.name == "nt" else "native-posix-manual")
        require(args.old_bin.is_file() and args.new_bin.is_file(), "both supplied executables must be regular files")
        report["old_version"], report["candidate_version"] = version(args.old_bin), version(args.new_bin)
        require(report["old_version"] == "1.7.1", "baseline executable must be exactly 1.7.1")
        require(report["candidate_version"] == args.expected_version, "candidate is not the expected release version")
        report["old_binary_sha256"] = digest(args.old_bin.read_bytes())
        report["candidate_binary_sha256"] = digest(args.new_bin.read_bytes())
        report["data_layout"] = "same installed executable path; default adjacent rustchan-data; no storage-path overrides"
        temporary = Path(tempfile.mkdtemp(prefix="rustchan-binary-upgrade-")).resolve()
        smoke = Smoke(args, temporary, args.output / (run_id + "-logs"))
        report.update(smoke.run())
        report["success"] = True
        exit_code = 0
    except VerificationError as error:
        report["error"] = str(error)
    except Exception as error:
        # Exception details can contain a command password, token, or response URL.
        # Raw process logs are retained only after redaction below.
        report["error"] = "unexpected verifier error: " + type(error).__name__
    finally:
        if smoke is not None:
            try:
                smoke.stop(strict=False)
            except Exception:
                report["cleanup_error"] = "owned server cleanup failed"
                report["success"] = False
                exit_code = 1
            try:
                smoke.save_logs()
            except Exception:
                report["log_cleanup_error"] = "sanitized process logs could not be saved"
                report["success"] = False
                exit_code = 1
            report["checks"] = smoke.checks
            report["sanitized_logs"] = str(smoke.logs)
        if temporary is not None:
            if smoke is not None and smoke.process is not None:
                report["remaining_temporary_installation"] = str(temporary)
                report["cleanup_error"] = "owned server could not be confirmed stopped; its data was retained"
            else:
                try:
                    shutil.rmtree(temporary)
                    report["temporary_installations_removed"] = not temporary.exists()
                except OSError:
                    report["cleanup_error"] = "temporary installation could not be removed"
                    report["remaining_temporary_installation"] = str(temporary)
                    report["success"] = False
                    exit_code = 1
        report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        print(json.dumps({"success": report["success"], "expected_version": args.expected_version,
                          "checks_passed": len(report["checks"]), "report": str(report_path)}))
        if not report["success"]:
            print(report.get("error", "verification or cleanup failed"), file=sys.stderr)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
