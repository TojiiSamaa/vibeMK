"""
activate_changes rereads the pending changes once after a 412.

Seen live on 2.3 and 2.5: a host rename job activates on its own, so the
ETag read just before the POST is already stale when the POST lands.
"""

import pytest

from vibemk.api.exceptions import CheckMKAPIError
from vibemk.handlers.configuration import ConfigurationHandler

PENDING = {"success": True, "data": {"value": [{"id": "c1"}]}, "headers": {"ETag": '"a"'}}
EMPTY = {"success": True, "data": {"value": []}, "headers": {"ETag": '"b"'}}
STALE = CheckMKAPIError("HTTP 412: PRECONDITION FAILED", 412, {})


@pytest.mark.asyncio
async def test_a_412_then_no_pending_changes_is_not_an_error(mock_checkmk_client):
    mock_checkmk_client.get.side_effect = [PENDING, EMPTY]
    mock_checkmk_client.post.side_effect = [STALE]

    result = await ConfigurationHandler(mock_checkmk_client).handle("vibemk_activate_changes", {})

    assert "No Pending Changes" in result[0]["text"]


@pytest.mark.asyncio
async def test_a_412_is_retried_with_the_fresh_etag(mock_checkmk_client):
    mock_checkmk_client.get.side_effect = [PENDING, {**PENDING, "headers": {"ETag": '"c"'}}]
    mock_checkmk_client.post.side_effect = [STALE, {"success": True, "data": {"id": "act-1"}}]

    result = await ConfigurationHandler(mock_checkmk_client).handle("vibemk_activate_changes", {})

    assert "act-1" in result[0]["text"]
    assert mock_checkmk_client.post.call_args.kwargs["headers"] == {"If-Match": '"c"'}


@pytest.mark.asyncio
async def test_a_second_412_is_reported(mock_checkmk_client):
    mock_checkmk_client.get.side_effect = [PENDING, PENDING]
    mock_checkmk_client.post.side_effect = [STALE, STALE]

    result = await ConfigurationHandler(mock_checkmk_client).handle("vibemk_activate_changes", {})

    assert "Activation failed" in result[0]["text"]
