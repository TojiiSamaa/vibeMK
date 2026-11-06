"""Audit A5a regressions: S3 (non-ASCII bearer) and S4 (backslash in direct URL)."""

import pytest
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import PlainTextResponse
from starlette.routing import Route
from starlette.testclient import TestClient

from vibemk.handlers.connection import ConnectionHandler
from vibemk.server.http_auth import BearerTokenMiddleware


def test_s3_non_ascii_bearer_is_401_not_500():
    async def ok(_r: Request) -> PlainTextResponse:
        return PlainTextResponse("reached")

    app = Starlette(routes=[Route("/mcp", ok)])
    app.add_middleware(BearerTokenMiddleware, token="s3cr3t-token-value")
    client = TestClient(app, raise_server_exceptions=False)
    r = client.get("/mcp", headers={"Authorization": "Bearer ".encode() + "é".encode("latin-1")})
    assert r.status_code == 401


@pytest.mark.parametrize("path", [r"x\..\..\y", "x%5c..%5c..%5cy", "x%5C"])
def test_s4_backslash_in_path_is_refused(mock_checkmk_client, path):
    handler = ConnectionHandler(mock_checkmk_client)
    assert handler._refuse_foreign_url(f"{handler.client.api_base_url}/{path}") is not None
