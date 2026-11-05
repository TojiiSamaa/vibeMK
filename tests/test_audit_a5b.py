"""Audit lot A5b: client, base handler, hosts, downtimes (AUDIT.md ids in each test name)."""

import asyncio
import logging
import threading
import time
import urllib.error
from email.message import Message
from http.server import BaseHTTPRequestHandler, HTTPServer
from unittest.mock import MagicMock, patch

import pytest

from vibemk.api.client import CheckMKClient
from vibemk.api.exceptions import CheckMKAPIError
from vibemk.handlers.base import BaseHandler, time_ago
from vibemk.handlers.hosts import HostHandler


class _Server:
    """Real local HTTP server; `routes` maps path -> list of (status, headers) answered in order."""

    def __init__(self, routes):
        self.routes, self.seen = routes, []
        outer = self

        class H(BaseHTTPRequestHandler):
            def do_GET(self):
                outer.seen.append((self.path, self.headers.get("Authorization")))
                answers = outer.routes[self.path]
                status, headers = answers.pop(0) if len(answers) > 1 else answers[0]
                self.send_response(status)
                for k, v in headers.items():
                    self.send_header(k, v)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"{}")

            def log_message(self, *a):
                pass

        self.httpd = HTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.httpd.server_port}"
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


def _client(mock_config, url):
    mock_config.server_url, mock_config.max_retries = url, 2
    c = CheckMKClient(mock_config, skip_url_detection=True)
    c.api_base_url = url
    return c


def test_S1_authorization_not_sent_to_another_host(mock_config):
    target = _Server({"/stolen": [(200, {})]})
    origin = _Server({"/x": [(302, {"Location": f"{target.url}/stolen"})]})
    try:
        _client(mock_config, origin.url).get("x")
    except Exception:
        pass
    finally:
        origin.close(), target.close()
    assert target.seen == [] or target.seen[0][1] is None


def test_E3_replay_after_5xx_keeps_follow_redirects_false(mock_config):
    srv = _Server({"/d": [(503, {}), (302, {"Location": "/e"})], "/e": [(200, {})]})
    try:
        with patch("time.sleep"):
            result = _client(mock_config, srv.url).request("d", follow_redirects=False)
    finally:
        srv.close()
    assert result["status"] == 302


def test_E4_retry_after_is_capped():
    err = urllib.error.HTTPError("u", 429, "x", {"Retry-After": "3600"}, None)
    assert CheckMKClient._retry_delay(err, 0) <= 30.0


def test_E1_etag_header_found_whatever_its_case():
    msg = Message()
    msg["etag"] = "abc"
    assert CheckMKClient._result(200, {}, "", msg)["headers"].get("ETag") == "abc"


def test_S5_warns_when_tls_verification_is_off(mock_config, caplog):
    mock_config.verify_ssl = False
    with caplog.at_level(logging.WARNING):
        CheckMKClient(mock_config, skip_url_detection=True)
    assert any("verify" in r.message.lower() or "tls" in r.message.lower() for r in caplog.records)


class _H(BaseHandler):
    async def handle(self, tool_name, arguments):
        return []


def _handler(**responses):
    client = MagicMock()
    client.config.never_activate_changes = False
    client.config.site = "s"
    client.get.return_value = responses.get("get")
    client.post.return_value = responses.get("post", {"success": True})
    return _H(client), client


@pytest.mark.parametrize("data", [None, [], {"extensions": None}, {"extensions": {"meta_data": None}}])
def test_E2_extract_etag_survives_odd_bodies(data):
    assert BaseHandler._extract_etag({"headers": {}, "data": data}) == "*"


def test_E11_activation_survives_null_pending_data():
    h, _ = _handler(get={"success": True, "data": None, "headers": {}})
    assert isinstance(h._run_activation(), str)


def test_E8_wildcard_fallback_is_logged(caplog):
    with caplog.at_level(logging.WARNING):
        BaseHandler._extract_etag({"headers": {}, "data": {}})
    assert any("*" in r.message or "etag" in r.message.lower() for r in caplog.records)


def test_E9a_activation_only_says_started():
    h, _ = _handler(get={"success": True, "data": {"value": [1]}, "headers": {"ETag": "e"}})
    assert "started" in h._run_activation().lower()


def test_E10a_time_ago_future_never_negative():
    assert not time_ago(time.time() + 18000).startswith("-")


def test_N1_update_host_with_null_data_does_not_raise():
    client = MagicMock()
    client.get.return_value = {"success": True, "data": None, "headers": {}}
    client.put.return_value = {"success": True, "data": {}}
    out = asyncio.run(HostHandler(client)._update_host({"host_name": "h", "attributes": {"alias": "a"}}))
    assert isinstance(out, list)


def test_E6_update_host_412_gives_the_concurrency_advice():
    client = MagicMock()
    client.get.return_value = {"success": True, "data": {}, "headers": {"ETag": "e"}}
    client.put.side_effect = CheckMKAPIError("HTTP 412: Precondition Failed", 412, {})
    out = asyncio.run(HostHandler(client)._update_host({"host_name": "h", "attributes": {"alias": "a"}}))
    assert "Concurrent modification" in out[0]["text"]
