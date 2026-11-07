"""Ruleset descriptions (CheckMK's "help") and rule descriptions reach the model.

Shapes measured on CheckMK 2.3.0p52 and 2.5.0p18 (7 Nov 2026):
- objects/ruleset/{name} holds only the title and the rule count; the help is
  in domain-types/ruleset/collections/all alone.
- In the collection, `name` matches substrings and the default lists only the
  rulesets that hold rules; `used=false` lists only the others.
- About 250 of ~1,600 rulesets have help, median 415 characters, plain text.
- A rule's properties carry `description`, `comment` and `documentation_url`.
"""

import pytest

from vibemk.handlers.rules import RulesHandler
from vibemk.handlers.rulesets import RULESET_COLLECTION, RulesetsHandler, find_ruleset

CMK_INV_HELP = (
    "All hosts configured via this rule set will do a hardware and software inventory. For each configured "
    "host a new active check will be created. You should also create a rule for changing the normal interval "
    "for that check to something between a couple of hours and one day. Note: in order to get any useful "
    "result for agent based hosts make sure that you have installed the agent plug-in mk_inventory "
)
PROCESS_HELP = (
    "This rule set defines criteria for automatically creating checks for running processes based upon what "
    "is running when the service discovery is done. These services will be created with default parameters."
)


def ok(data):
    return {"success": True, "status": 200, "headers": {}, "data": data}


def entry(name, help_text, title="A title"):
    return {
        "id": name,
        "domainType": "ruleset",
        "extensions": {"name": name, "title": title, "match_type": "all", "help": help_text, "number_of_rules": 1},
    }


def collection(used, unused=()):
    """A fake collection that filters like CheckMK: substring name, used or unused only."""

    def get(endpoint, params=None, **_):
        assert endpoint == RULESET_COLLECTION
        params = params or {}
        pool = unused if params.get("used") == "false" else used
        return ok({"value": [r for r in pool if params.get("name", "") in r["id"]]})

    return get


def test_find_ruleset_keeps_the_exact_id_among_substring_matches(mock_checkmk_client):
    mock_checkmk_client.get.side_effect = collection(
        [entry("host_groups_extra", "wrong"), entry("host_groups", "right")]
    )

    assert find_ruleset(mock_checkmk_client, "host_groups")["extensions"]["help"] == "right"


def test_find_ruleset_also_looks_among_rulesets_without_rules(mock_checkmk_client):
    mock_checkmk_client.get.side_effect = collection([], unused=[entry("active_checks:cmk_inv", CMK_INV_HELP)])

    assert find_ruleset(mock_checkmk_client, "active_checks:cmk_inv")["id"] == "active_checks:cmk_inv"


@pytest.mark.asyncio
async def test_show_ruleset_gives_the_whole_description(mock_checkmk_client):
    obj = ok({"id": "active_checks:cmk_inv", "title": "Do hardware/software inventory", "extensions": {}})
    listed = collection([], unused=[entry("active_checks:cmk_inv", CMK_INV_HELP)])
    mock_checkmk_client.get.side_effect = lambda endpoint, *a, **k: (
        obj if endpoint.startswith("objects/") else listed(endpoint, *a, **k)
    )

    text = (
        await RulesetsHandler(mock_checkmk_client).handle(
            "vibemk_show_ruleset", {"ruleset_name": "active_checks:cmk_inv"}
        )
    )[0]["text"]

    assert CMK_INV_HELP.strip() in text
    assert "**Match Type:** all" in text


@pytest.mark.asyncio
async def test_show_ruleset_says_when_checkmk_has_no_description(mock_checkmk_client):
    obj = ok({"id": "checkgroup_parameters:filesystem", "title": "Filesystems", "extensions": {}})
    listed = collection([], unused=[entry("checkgroup_parameters:filesystem", "None")])
    mock_checkmk_client.get.side_effect = lambda endpoint, *a, **k: (
        obj if endpoint.startswith("objects/") else listed(endpoint, *a, **k)
    )

    text = (
        await RulesetsHandler(mock_checkmk_client).handle(
            "vibemk_show_ruleset", {"ruleset_name": "checkgroup_parameters:filesystem"}
        )
    )[0]["text"]

    assert "**Description:** CheckMK has none for this ruleset." in text
    assert "None" not in text.replace("has none", "")


@pytest.mark.asyncio
async def test_searches_no_longer_cut_the_description_at_100_characters(mock_checkmk_client):
    mock_checkmk_client.get.return_value = ok({"value": [entry("inventory_processes_rules", PROCESS_HELP)]})

    search = (await RulesetsHandler(mock_checkmk_client).handle("vibemk_search_rulesets", {"fulltext": "process"}))[0]
    listed = (await RulesHandler(mock_checkmk_client).handle("vibemk_get_rulesets", {"search": "process"}))[0]

    assert PROCESS_HELP in search["text"]
    assert PROCESS_HELP in listed["text"]


@pytest.mark.asyncio
async def test_get_ruleset_shows_the_ruleset_and_each_rules_description(mock_checkmk_client):
    rule = {
        "id": "a1b2",
        "extensions": {
            "folder": "/",
            "value_raw": "{'descr': 'cmk-automation'}",
            "conditions": {},
            "properties": {
                "description": "Shipped rule to monitor site's automation helpers",
                "comment": "This rule is shipped with Checkmk.",
                "documentation_url": "https://docs.checkmk.com/latest/en/",
                "disabled": False,
            },
        },
    }
    listed = collection([entry("inventory_processes_rules", PROCESS_HELP, "Process discovery")])

    def get(endpoint, *a, **k):
        if endpoint == "domain-types/rule/collections/all":
            return ok({"value": [rule]})
        return listed(endpoint, *a, **k)

    mock_checkmk_client.get.side_effect = get

    text = (
        await RulesHandler(mock_checkmk_client).handle(
            "vibemk_get_ruleset", {"ruleset_name": "inventory_processes_rules"}
        )
    )[0]["text"]

    assert "**Title:** Process discovery" in text
    assert f"**Description:** {PROCESS_HELP}" in text
    assert "Description: Shipped rule to monitor site's automation helpers" in text
    assert "Comment: This rule is shipped with Checkmk." in text
    assert "Documentation: https://docs.checkmk.com/latest/en/" in text


@pytest.mark.asyncio
async def test_get_ruleset_still_lists_the_rules_when_the_collection_fails(mock_checkmk_client):
    from vibemk.api.exceptions import CheckMKAPIError

    def get(endpoint, *a, **k):
        if endpoint == "domain-types/rule/collections/all":
            return ok({"value": []})
        raise CheckMKAPIError("HTTP 500", 500, {})

    mock_checkmk_client.get.side_effect = get

    text = (await RulesHandler(mock_checkmk_client).handle("vibemk_get_ruleset", {"ruleset_name": "x"}))[0]["text"]

    assert "No rules configured in this ruleset" in text
