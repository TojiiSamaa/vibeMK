"""Live check of AUDIT S2 and E9c against a real CheckMK site.

A second admin leaves a change pending; the server under test (which logs in as
CHECKMK_USERNAME) must not activate it on its own, must say why, and must
activate it when asked with force_foreign_changes=true. Also checks that
"nothing to activate" is an info, not an error.

    CHECKMK_SERVER_URL=http://127.0.0.1:5080 CHECKMK_SITE=cmk \\
    CHECKMK_USERNAME=cmkadmin CHECKMK_PASSWORD=... \\
    python scripts/live_foreign_changes.py .venv/bin/vibemk

Creates the user vmkother and the hosts vmk-foreign and vmk-own, then deletes
them and activates, so the site ends with nothing pending. Exit code 1 on any
failed check.
"""

import base64
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from typing import Any, Dict, Optional

URL, SITE = os.environ["CHECKMK_SERVER_URL"], os.environ["CHECKMK_SITE"]
USER, PASSWORD = os.environ["CHECKMK_USERNAME"], os.environ["CHECKMK_PASSWORD"]
API = f"{URL}/{SITE}/check_mk/api/1.0/"
PENDING = API + "domain-types/activation_run/collections/pending_changes"
ACTIVATE = API + "domain-types/activation_run/actions/activate-changes/invoke"
OTHER, OTHER_PASSWORD = "vmkother", "vmk-other-pass-123!"


class Reply:
    def __init__(self, status: int, body: bytes, headers: Any) -> None:
        self.status, self.body, self.headers = status, body, headers

    def json(self) -> Any:
        return json.loads(self.body)


class Session:
    """Plain urllib, so the script needs nothing beyond the standard library."""

    def __init__(self, user: str, password: str) -> None:
        self.auth = "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()

    def request(self, method: str, url: str, body: Any = None, headers: Optional[Dict[str, str]] = None) -> Reply:
        data = json.dumps(body).encode() if body is not None else None
        sent = {"Authorization": self.auth, "Accept": "application/json", **(headers or {})}
        if data is not None:
            sent["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, headers=sent, method=method)
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return Reply(r.status, r.read(), r.headers)
        except urllib.error.HTTPError as e:
            return Reply(e.code, e.read(), e.headers)

    def get(self, url: str) -> Reply:
        return self.request("GET", url)

    def post(self, url: str, json: Any = None, headers: Optional[Dict[str, str]] = None) -> Reply:
        return self.request("POST", url, json, headers)

    def delete(self, url: str) -> Reply:
        return self.request("DELETE", url)


def session(user: str, password: str) -> Session:
    return Session(user, password)


ADMIN = session(USER, PASSWORD)


def pending() -> list:
    return ADMIN.get(PENDING).json()["value"]


def wait_until_nothing_pending() -> None:
    for _ in range(90):
        if not pending():
            return
        time.sleep(1)


def force_activate() -> None:
    reply = ADMIN.get(PENDING)
    if reply.json()["value"]:
        body = {"redirect": False, "sites": [SITE], "force_foreign_changes": True}
        ADMIN.post(ACTIVATE, json=body, headers={"If-Match": reply.headers.get("ETag", "*")})
    wait_until_nothing_pending()


class Server:
    def __init__(self, cmd: list) -> None:
        self.p = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            bufsize=1,
            env=dict(os.environ, PYTHONUTF8="1"),
        )
        self.n = 0
        hello = {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "foreign", "version": "0"}}
        self.rpc("initialize", hello)
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, msg: dict) -> None:
        assert self.p.stdin is not None
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def rpc(self, method: str, params: dict) -> dict:
        assert self.p.stdout is not None
        self.n += 1
        self.send({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params})
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise EOFError(f"server exited (code {self.p.poll()})")
            msg = json.loads(line)
            if msg.get("id") == self.n:
                return msg

    def call(self, tool: str, args: dict) -> tuple:
        result = self.rpc("tools/call", {"name": tool, "arguments": args})["result"]
        return bool(result.get("isError")), " ".join(block.get("text", "") for block in result["content"])


def main() -> int:
    failures = 0

    def check(label: str, held: bool, got: str) -> None:
        nonlocal failures
        failures += not held
        print(("PASS " if held else "FAIL ") + label + ("" if held else f": {got[:300]}"), flush=True)

    server = Server(sys.argv[1:])
    try:
        force_activate()
        is_error, text = server.call("vibemk_activate_changes", {})
        check("nothing pending is an info", not is_error and "No Pending Changes" in text, text)

        ADMIN.post(
            API + "domain-types/user_config/collections/all",
            json={
                "username": OTHER,
                "fullname": "vmk other",
                "roles": ["admin"],
                "auth_option": {"auth_type": "password", "password": OTHER_PASSWORD},
            },
        )
        force_activate()
        host = {"folder": "/", "host_name": "vmk-foreign", "attributes": {"ipaddress": "127.0.0.2"}}
        session(OTHER, OTHER_PASSWORD).post(API + "domain-types/host_config/collections/all", json=host)

        is_error, text = server.call("vibemk_activate_changes", {})
        check(
            "another user's change: refused and explained",
            is_error and "other users have pending changes" in text,
            text,
        )

        own = {
            "host_name": "vmk-own",
            "folder": "/",
            "attributes": {"ipaddress": "127.0.0.3"},
            "activate_changes": True,
        }
        is_error, text = server.call("vibemk_create_host", own)
        check("write tool: written, not activated, explained", "other users have pending changes" in text, text)

        time.sleep(3)
        users = {change["user_id"] for change in pending()}
        check("both changes still pending", users >= {OTHER, USER}, str(users))

        is_error, text = server.call("vibemk_activate_changes", {"force_foreign_changes": True})
        check("force_foreign_changes=true activates", not is_error and "Started Successfully" in text, text)
        wait_until_nothing_pending()
        check("nothing left pending", not pending(), "")
    finally:
        server.p.kill()
        for name in ("vmk-foreign", "vmk-own"):
            ADMIN.delete(API + f"objects/host_config/{name}")
        ADMIN.delete(API + f"objects/user_config/{OTHER}")
        force_activate()
        print(f"pending at the end: {len(pending())}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
