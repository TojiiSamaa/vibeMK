"""Python vs Rust vibeMK over stdio, same fake CheckMK (needs psutil). Usage: bench.py <repo> <rust_exe> <python_exe>"""

import http.server
import json
import os
import socketserver
import statistics as st
import subprocess
import sys
import threading
import time

import psutil

repo, rust_exe, py_exe = sys.argv[1:4]
BODY = json.dumps({"site": "cmk", "versions": {"checkmk": "2.3.0"}, "value": []}).encode()


class H(http.server.BaseHTTPRequestHandler):
    def _r(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    do_GET = do_POST = do_PUT = do_DELETE = _r

    def log_message(self, *a):
        pass


class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


srv = S(("127.0.0.1", 0), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
url = f"http://127.0.0.1:{srv.server_address[1]}"

ENV = dict(
    os.environ, CHECKMK_SERVER_URL=url, CHECKMK_SITE="cmk", CHECKMK_USERNAME="a", CHECKMK_PASSWORD="b", PYTHONUTF8="1"
)
INIT = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "b", "version": "0"}},
}


class Srv:
    def __init__(self, cmd):
        self.p = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=ENV, text=True, bufsize=1
        )
        self.peak = 0
        self.stop = False
        threading.Thread(target=self._sample, daemon=True).start()

    def _sample(self):
        try:
            ps = psutil.Process(self.p.pid)
            while not self.stop:
                try:
                    procs = [ps] + ps.children(recursive=True)
                    self.peak = max(self.peak, sum(x.memory_info().rss for x in procs))
                except psutil.Error:
                    pass
                time.sleep(0.01)
        except psutil.Error:
            pass

    def rpc(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def close(self):
        self.stop = True
        try:
            self.p.stdin.close()
            self.p.wait(10)
        except Exception:
            self.p.kill()


def measure(cmd, runs=10, calls=300):
    starts, lists, peaks = [], [], []
    for _ in range(runs):
        t = time.perf_counter()
        s = Srv(cmd)
        s.rpc(INIT)
        starts.append(time.perf_counter() - t)
        s.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        t = time.perf_counter()
        r = s.rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        lists.append(time.perf_counter() - t)
        n = len(r["result"]["tools"])
        s.close()
        peaks.append(s.peak)
    s = Srv(cmd)
    s.rpc(INIT)
    lat = []
    call = lambda i: {
        "jsonrpc": "2.0",
        "id": i,
        "method": "tools/call",
        "params": {"name": "vibemk_get_checkmk_version", "arguments": {}},
    }
    s.rpc(call(10))  # warm-up: first call builds the registry
    t0 = time.perf_counter()
    for i in range(calls):
        t = time.perf_counter()
        r = s.rpc(call(100 + i))
        lat.append(time.perf_counter() - t)
    total = time.perf_counter() - t0
    ok = not r["result"].get("isError", False)
    s.close()
    lat.sort()
    return dict(
        tools=n,
        start_ms=st.median(starts) * 1e3,
        list_ms=st.median(lists) * 1e3,
        call_p50_ms=lat[len(lat) // 2] * 1e3,
        call_p99_ms=lat[int(len(lat) * 0.99)] * 1e3,
        calls_per_s=calls / total,
        peak_rss_mb=max(peaks) / 2**20,
        call_ok=ok,
    )


res = {"rust": measure([rust_exe]), "python": measure([py_exe, os.path.join(repo, "main.py")])}
json.dump(res, open(os.path.join(os.path.dirname(__file__), "bench.json"), "w"), indent=1)
print(json.dumps(res, indent=1))
