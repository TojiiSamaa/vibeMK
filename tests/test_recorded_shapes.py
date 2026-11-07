"""Display tools fed with the bodies CheckMK 2.3.0p52 and 2.5.0p18 really send.

Each fixture was copied from the live API. The earlier fixtures were written
from the field names the code expected, so they passed while every real call
printed "Unknown", "No title" or "None".
"""

import pytest

from vibemk.handlers.discovery import DiscoveryHandler
from vibemk.handlers.rules import RulesHandler
from vibemk.handlers.rulesets import RulesetsHandler


def ok(data):
    return {"success": True, "status": 200, "headers": {}, "data": data}


@pytest.mark.asyncio
async def test_show_ruleset_reads_the_top_level_title(mock_checkmk_client):
    mock_checkmk_client.get.return_value = ok(
        {
            "id": "ping_levels",
            "title": "PING and host check parameters",
            "extensions": {"name": "ping_levels", "number_of_rules": 1},
        }
    )

    text = (await RulesetsHandler(mock_checkmk_client).handle("vibemk_show_ruleset", {"ruleset_name": "ping_levels"}))[
        0
    ]["text"]

    assert "**Title:** PING and host check parameters\n**Number of Rules:** 1\n" in text
    for placeholder in ("No title", "Unknown", "No help available"):
        assert placeholder not in text


@pytest.mark.asyncio
async def test_get_rulesets_hides_the_none_string_checkmk_sends_for_no_help(mock_checkmk_client):
    mock_checkmk_client.get.return_value = ok(
        {
            "value": [
                {
                    "id": "checkgroup_parameters:interfaces",
                    "extensions": {"title": "Network interfaces and switch ports", "help": "None"},
                }
            ]
        }
    )

    text = (await RulesHandler(mock_checkmk_client).handle("vibemk_get_rulesets", {}))[0]["text"]

    assert "Help: No description" in text
    assert "..." not in text
    assert "None" not in text


@pytest.mark.asyncio
async def test_discovery_background_job_shows_state_and_logs(mock_checkmk_client):
    mock_checkmk_client.get.return_value = ok(
        {
            "id": "service_discovery-web01",
            "extensions": {
                "active": False,
                "state": "finished",
                "logs": {"result": [], "progress": ["Starting job...", "Completed."]},
            },
        }
    )

    text = (
        await DiscoveryHandler(mock_checkmk_client).handle(
            "vibemk_get_discovery_background_job", {"host_name": "web01"}
        )
    )[0]["text"]

    assert text == (
        "✅ **Discovery Background Job**\n\nHost: **web01**\nState: FINISHED\nActive: no\n\n"
        "• Starting job...\n• Completed.\n\n💡 Use 'get_discovery_status' to see discovery results."
    )


@pytest.mark.asyncio
async def test_ruleset_searches_send_the_fulltext_field(mock_checkmk_client):
    """The collection has no "search" field: CheckMK answered 400 "These fields have problems: search"."""
    from vibemk.handlers.host_group_rules import HostGroupRulesHandler

    mock_checkmk_client.get.return_value = ok({"value": []})

    await RulesHandler(mock_checkmk_client).handle("vibemk_get_rulesets", {"search": "temperature"})
    assert mock_checkmk_client.get.call_args.kwargs["params"] == {"fulltext": "temperature"}

    await HostGroupRulesHandler(mock_checkmk_client).handle("vibemk_find_host_grouping_rulesets", {})
    assert all(c.kwargs["params"].keys() == {"fulltext"} for c in mock_checkmk_client.get.call_args_list[1:])


def _downtime_listing(end_time):
    return ok({"value": [{"id": "24", "extensions": {"host_name": "web01", "end_time": end_time}}]})


@pytest.mark.asyncio
async def test_modify_downtime_reads_the_new_end_back(mock_checkmk_client):
    from vibemk.handlers.downtimes import DowntimeHandler

    mock_checkmk_client.put.return_value = ok({})
    mock_checkmk_client.get.return_value = _downtime_listing("2026-11-06T05:21:16+00:00")

    text = (
        await DowntimeHandler(mock_checkmk_client).handle(
            "vibemk_modify_downtime", {"downtime_id": 24, "end_time": "2026-11-06T05:21:16Z"}
        )
    )[0]["text"]

    assert text == "⏰ **Downtime Modified**\n\nTarget: downtime #24\nNew end: 2026-11-06T05:21:16Z"


@pytest.mark.asyncio
async def test_modify_downtime_ignored_by_the_core_is_not_reported_as_done(mock_checkmk_client, monkeypatch):
    """CheckMK 2.3 and 2.5 Raw/Community answer 204; Nagios logs "Unrecognized external command"."""
    from vibemk.handlers import downtimes

    async def no_wait(_):
        return None

    monkeypatch.setattr(downtimes.asyncio, "sleep", no_wait)
    mock_checkmk_client.put.return_value = ok({})
    mock_checkmk_client.get.return_value = _downtime_listing("2026-11-06T03:21:15+00:00")

    text = (
        await downtimes.DowntimeHandler(mock_checkmk_client).handle(
            "vibemk_modify_downtime", {"downtime_id": 24, "end_time": "2026-11-06T05:21:16Z"}
        )
    )[0]["text"]

    assert "Downtime not modified" in text
    assert mock_checkmk_client.get.call_count == downtimes.MODIFY_READBACK_TRIES
