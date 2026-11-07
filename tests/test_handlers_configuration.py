"""vibemk_activate_changes fed with the bodies CheckMK 2.3.0p52 really sends.

The bodies were copied from the live API on 127.0.0.1:5080 (scratchpad
running_shape.txt and replay23.py of task A). On 2.3 a second
activate-changes sent while a run is in progress is accepted (HTTP 200,
new run id), yet the site refuses it ("Locked") and the run ends at once as
"Complete." with the later changes still pending.
"""

import pytest

from vibemk.api.exceptions import CheckMKError
from vibemk.handlers.configuration import ConfigurationHandler

PENDING = "domain-types/activation_run/collections/pending_changes"
RUNNING = "domain-types/activation_run/collections/running"
ACTIVATE = "domain-types/activation_run/actions/activate-changes/invoke"
ETAG = '"def677ef0233bf1a870dcf7f40037b8074f669252e30b7140ea029ee69e499ab"'

CHANGE = {
    "id": "85822ffa-1673-4999-9e6b-7f66c35a75dd",
    "user_id": "cmkadmin",
    "action_name": "new-rule",
    "text": "Created new rule #0 in ruleset &quot;Network interfaces and switch ports&quot; in folder &quot;Main&quot;",
    "time": "2026-11-06T21:38:14.646276+00:00",
}


def run(title, is_running):
    return {
        "domainType": "activation_run",
        "id": "6389bbe2-d9a0-45e3-be4f-e0e137baef31",
        "title": title,
        "members": {},
        "extensions": {
            "sites": ["cmk"],
            "is_running": is_running,
            "force_foreign_changes": True,
            "time_started": "2026-11-06T21:38:12.721958+00:00",
            "changes": [],
        },
    }


def ok(data, headers=None):
    return {"success": True, "status": 200, "headers": headers or {}, "data": data}


def pending_ok():
    return ok({"id": "activation_run", "value": [CHANGE]}, {"ETag": ETAG})


@pytest.fixture
def handler(mock_checkmk_client):
    h = ConfigurationHandler(mock_checkmk_client)
    h.running_poll_s = 0
    return h


@pytest.mark.asyncio
async def test_activation_waits_for_the_run_already_in_progress(handler, mock_checkmk_client):
    calls = []
    running = [
        ok({"id": "activation_run", "value": [run("Activation status: In progress.", True)]}),
        ok({"id": "activation_run", "value": [run("Activation status: Complete.", False)]}),
    ]

    def get(endpoint, *a, **k):
        calls.append(("GET", endpoint))
        return running.pop(0) if endpoint == RUNNING else pending_ok()

    def post(endpoint, *a, **k):
        calls.append(("POST", endpoint))
        return ok({"id": "d072208a-1602-4e0b-9378-fbcf83f9fb4b"})

    mock_checkmk_client.get.side_effect = get
    mock_checkmk_client.post.side_effect = post

    text = (await handler.handle("vibemk_activate_changes", {}))[0]["text"]

    assert "Activation ID: d072208a-1602-4e0b-9378-fbcf83f9fb4b" in text
    # The POST only goes out once the running collection shows no run in progress.
    assert calls == [("GET", RUNNING), ("GET", RUNNING), ("GET", PENDING), ("POST", ACTIVATE)]
    assert mock_checkmk_client.post.call_args.kwargs["headers"] == {"If-Match": ETAG}
    assert mock_checkmk_client.post.call_args.kwargs["data"] == {
        "redirect": False,
        "sites": [mock_checkmk_client.config.site],
        "force_foreign_changes": False,
    }


@pytest.mark.asyncio
async def test_a_finished_run_in_the_running_collection_does_not_block(handler, mock_checkmk_client):
    # 2.3 keeps listing a finished run under collections/running ("Complete.").
    def get(endpoint, *a, **k):
        if endpoint == RUNNING:
            return ok({"value": [run("Activation status: Complete.", False)]})
        return pending_ok()

    mock_checkmk_client.get.side_effect = get
    mock_checkmk_client.post.return_value = ok({"id": "act-1"})

    text = (await handler.handle("vibemk_activate_changes", {}))[0]["text"]

    assert "Activation ID: act-1" in text
    assert mock_checkmk_client.post.call_count == 1


@pytest.mark.asyncio
async def test_a_412_reads_the_pending_changes_again_once(handler, mock_checkmk_client):
    etags = ['"stale"', ETAG]

    def get(endpoint, *a, **k):
        if endpoint == RUNNING:
            return ok({"value": []})
        return ok({"value": [CHANGE]}, {"ETag": etags.pop(0)})

    mock_checkmk_client.get.side_effect = get
    mock_checkmk_client.post.side_effect = [
        CheckMKError("HTTP 412: Precondition Failed", status_code=412),
        ok({"id": "act-2"}),
    ]

    text = (await handler.handle("vibemk_activate_changes", {}))[0]["text"]

    assert "Activation ID: act-2" in text
    sent = [c.kwargs["headers"]["If-Match"] for c in mock_checkmk_client.post.call_args_list]
    assert sent == ['"stale"', ETAG]


def test_write_tools_activation_also_waits_for_the_run_in_progress(mock_checkmk_client):
    """_run_activation (activate_changes=true on a write tool, set_process_thresholds)
    used to POST straight away and hit the same "Locked" refusal on 2.3."""
    h = ConfigurationHandler(mock_checkmk_client)
    h.running_poll_s = 0
    mock_checkmk_client.config.never_activate_changes = False
    calls = []
    running = [
        ok({"id": "activation_run", "value": [run("Activation status: In progress.", True)]}),
        ok({"id": "activation_run", "value": [run("Activation status: Complete.", False)]}),
    ]

    def get(endpoint, *a, **k):
        calls.append(("GET", endpoint))
        return running.pop(0) if endpoint == RUNNING else pending_ok()

    def post(endpoint, *a, **k):
        calls.append(("POST", endpoint))
        return ok({"id": "act-3"})

    mock_checkmk_client.get.side_effect = get
    mock_checkmk_client.post.side_effect = post

    line = h._run_activation()

    assert line.startswith("✅ Activation started")
    assert calls == [("GET", RUNNING), ("GET", RUNNING), ("GET", PENDING), ("POST", ACTIVATE)]
