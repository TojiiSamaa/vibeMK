"""Call every vibeMK tool, over stdio, against a REAL CheckMK, and report what each one did.

Usage: live_sweep.py <label> <server command...>
Env:   CHECKMK_SERVER_URL, CHECKMK_SITE, CHECKMK_USERNAME, CHECKMK_PASSWORD (a throwaway test site:
       the plan creates, changes and deletes hosts, users, rules, groups...).
Writes live_sweep_<label>.json next to this script and prints one line per call.

Each step is (tool, arguments, expected): expected "ok" means the call must succeed, "err" means
CheckMK has nothing to act on in a fresh site (no events, no LDAP...) and a clean error is the
right answer. A step passes when isError matches. `$name` strings are resolved just before the
call, from CheckMK itself or from an earlier answer, so ids never have to be guessed.
"""

import base64
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

LABEL, CMD = sys.argv[1], sys.argv[2:]
URL, SITE = os.environ["CHECKMK_SERVER_URL"], os.environ["CHECKMK_SITE"]
AUTH = (
    "Basic " + base64.b64encode(f"{os.environ['CHECKMK_USERNAME']}:{os.environ['CHECKMK_PASSWORD']}".encode()).decode()
)
API = f"{URL}/{SITE}/check_mk/api/1.0"


def rest(path):
    req = urllib.request.Request(f"{API}/{path}", headers={"Authorization": AUTH, "Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def status(path):
    try:
        rest(path)
        return 200
    except urllib.error.HTTPError as e:
        return e.code


def exists(path):
    return status(path) == 200


def gone(path):
    return status(path) == 404


def ext(path):
    return rest(path)["extensions"]


def rules(ruleset):
    return rest(f"domain-types/rule/collections/all?ruleset_name={ruleset}")["value"]


def host_rule(ruleset, host=None):
    """The rule of `ruleset` whose host condition names `host` (H by default)."""
    host = host or H
    return first(r for r in rules(ruleset) if host in json.dumps(r["extensions"]["conditions"].get("host_name", {})))


def service(host, desc):
    cols = "columns=description&columns=has_been_checked&columns=acknowledged&columns=state"
    return first(
        s["extensions"]
        for s in rest(f"objects/host/{host}/collections/services?{cols}")["value"]
        if s["extensions"]["description"] == desc
    )


def comment(text):
    return first(
        c["extensions"]
        for c in rest("domain-types/comment/collections/all")["value"]
        if c["extensions"]["comment"] == text
    )


def downtime(text):
    return first(
        d for d in rest("domain-types/downtime/collections/all")["value"] if d["extensions"]["comment"] == text
    )


def listed(kind, obj_id):
    """The comment or downtime with this id, read from its collection (the object URL also wants site_id)."""
    return first(
        o["extensions"] for o in rest(f"domain-types/{kind}/collections/all")["value"] if str(o["id"]) == str(obj_id)
    )


def activated():
    """The run the last activate_changes started has ended and left nothing pending; else CheckMK's own report."""
    run_id = re.search(r"Activation ID: (\S+)", SEEN["vibemk_activate_changes"]).group(1)
    # 2.5 drops a finished run from objects/activation_run (404): finished, then.
    run = ext(f"objects/activation_run/{run_id}") if exists(f"objects/activation_run/{run_id}") else {}
    if run.get("is_running"):
        return False
    left = rest("domain-types/activation_run/collections/pending_changes")["value"]
    if left:
        titles = [c.get("title") for c in left]
        sites = run.get("status_per_site")
        # 2.3 has no status_per_site; its verdict is only in the run's title ("Activation status: Complete."
        # or the failure), so report it. Repro on 5080 (2026-11-06, Python and Rust servers): edit a host,
        # activate -> run ends after ~7 s with pending_changes empty; the handler's body/ETag are not the cause.
        title = rest(f"objects/activation_run/{run_id}").get("title") if run else None
        raise AssertionError(
            f"run {run_id} ended, {len(left)} still pending: {titles[:12]} status_per_site={sites} title={title}"
        )
    return True


# CheckMK 2.3 has no action to remove an acknowledgement (405 to a GET when the route exists, 404 when not).
CAN_REMOVE_ACK = status("domain-types/acknowledge/actions/delete/invoke") != 404
# Where it is missing, the tool must say so and the acknowledgement must still be there.
ACK_AFTER_REMOVE = 0 if CAN_REMOVE_ACK else 1


def verify(label, check):
    """A read-back step: after the write before it, ask CheckMK whether the change really happened."""
    return ("verify", check, label)


def until(check, timeout):
    """Wait, polling the REST API, until `check` holds (CheckMK runs checks and commands asynchronously)."""
    return ("until", check, timeout)


# What a tool resolved its arguments to on its last call: a delete must make that id disappear.
LAST = {}
# Words that mean a field was not found where the code looked. A legitimate value can contain one
# ("Unknown" service state), so a hit is reported, not failed; the fixed cases are verify() steps below.
PLACEHOLDER = re.compile(
    r"\b(unknown|no title|n/a|none|null|undefined|not available|no help available|no description|not finished)\b",
    re.I,
)


def unix(value):
    return dt.datetime.fromisoformat(str(value).replace("Z", "+00:00")).timestamp()


def iso(minutes):
    return (dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=minutes)).strftime("%Y-%m-%dT%H:%M:%SZ")


H, H2, CL, FOLDER = "vmk-h1", "vmk-h2", "vmk-cl", "/vmktest"
SEEN = {}  # text of the last answer per tool, for ids that only the answer carries


def first(values, pred=lambda v: True):
    return next((v for v in values if pred(v)), None)


def vmk_rule(ruleset):
    rules = rest(f"domain-types/rule/collections/all?ruleset_name={ruleset}")["value"]
    return first(r["id"] for r in rules if "vmk" in json.dumps(r["extensions"]))


# Rulesets the OPS phase writes host rules into. Deleting a host does not
# delete the rules that name it, so CLEANUP removes them one by one and the
# last check makes sure none is left (they used to pile up, two per sweep).
HOST_RULESETS = (
    "active_checks:http",
    "active_checks:tcp",
    "active_checks:icmp",
    "custom_checks",
    "active_checks:dns",
    "active_checks:smtp",
    "active_checks:ftp",
    "active_checks:ldap",
    "active_checks:disk_smb",
    "active_checks:mkevents",
    "active_checks:cmk_inv",
    "checkgroup_parameters:interfaces",
    "checkgroup_parameters:memory_linux",
    "inventory_processes_rules",
)


def no_rule_names_a_test_host():
    hosts = (H, H2, H2 + "c", CL)
    return all(host_rule(rs, h) is None for rs in HOST_RULESETS for h in hosts)


def notif_cfg(description):
    # CheckMK wants a complete notification rule: start from the one every site ships with.
    cfg = rest("domain-types/notification_rule/collections/all")["value"][0]["extensions"]["rule_config"]
    cfg["rule_properties"]["description"] = description
    cfg["rule_properties"]["comment"] = "vmk"
    return cfg


RESOLVE = {
    "$RULE": lambda: first(
        r["id"]
        for r in rest("domain-types/rule/collections/all?ruleset_name=checkgroup_parameters:temperature")["value"]
        if "vmk" in r["extensions"]["properties"].get("comment", "")
    ),
    # The rule set_process_thresholds wrote for H. Not "the first rule of the
    # ruleset": 2.5 ships ~24 default process rules, and that first one is
    # CheckMK's own (each 2.5 sweep used to delete one and leave its own).
    "$PROC_RULE": lambda: host_rule("inventory_processes_rules")["id"],
    "$NOTIF": lambda: first(
        r["id"]
        for r in rest("domain-types/notification_rule/collections/all")["value"]
        if "vmk" in json.dumps(r.get("extensions", {}))
    ),
    "$DT": lambda: int(
        first(
            d["id"] for d in rest("domain-types/downtime/collections/all")["value"] if d["extensions"]["host_name"] == H
        )
    ),
    "$COMMENT": lambda: int(
        first(
            c["id"]
            for c in rest("domain-types/comment/collections/all")["value"]
            if c["extensions"]["host_name"] == H and c["extensions"]["comment"] == "vmk comment"
        )
    ),
    "$JOB": lambda: re.search(
        r"(bulk_discovery[\w-]*|[0-9a-f-]{8,})", SEEN.get("vibemk_start_bulk_discovery", "")
    ).group(1),
    "$IF_RULE": lambda: host_rule("checkgroup_parameters:interfaces")["id"],
    "$MEM_RULE": lambda: host_rule("checkgroup_parameters:memory_linux")["id"],
    "$CG_RULE": lambda: vmk_rule("host_contactgroups"),
    "$HG_RULE": lambda: vmk_rule("host_groups"),
    "$NOTIF_CFG": lambda: notif_cfg("vmk notif"),
    "$NOTIF_CFG2": lambda: notif_cfg("vmk notif 2"),
    "$START": lambda: iso(1),
    "$END": lambda: iso(120),
    "$END2": lambda: iso(240),
}


def resolve(v):
    if isinstance(v, str) and v in RESOLVE:
        return RESOLVE[v]()
    if isinstance(v, dict):
        return {k: resolve(x) for k, x in v.items()}
    if isinstance(v, list):
        return [resolve(x) for x in v]
    return v


READS = [
    (n, a, "ok")
    for n, a in [
        ("vibemk_debug_checkmk_connection", {}),
        ("vibemk_debug_url_detection", {}),
        ("vibemk_test_direct_url", {"test_url": f"{API}/version"}),
        ("vibemk_test_all_endpoints", {}),
        ("vibemk_get_checkmk_version", {}),
        ("vibemk_get_checkmk_hosts", {}),
        ("vibemk_get_checkmk_services", {}),
        ("vibemk_get_current_problems", {}),
        ("vibemk_get_pending_changes", {}),
        ("vibemk_get_folders", {}),
        ("vibemk_get_users", {}),
        ("vibemk_get_contact_groups", {}),
        ("vibemk_list_user_roles", {}),
        ("vibemk_show_user_role", {"role_id": "user"}),
        ("vibemk_get_host_groups", {}),
        ("vibemk_get_service_groups", {}),
        ("vibemk_get_comments", {}),
        ("vibemk_get_downtimes", {}),
        ("vibemk_get_rulesets", {}),
        ("vibemk_get_ruleset", {"ruleset_name": "checkgroup_parameters:temperature"}),
        ("vibemk_get_host_tags", {}),
        ("vibemk_get_timeperiods", {}),
        ("vibemk_get_passwords", {}),
        ("vibemk_get_notification_rules", {}),
        ("vibemk_debug_api_endpoints", {}),
        ("vibemk_debug_permissions", {}),
        ("vibemk_find_host_grouping_rulesets", {}),
        ("vibemk_get_example_rule_structures", {}),
        ("vibemk_list_downtimes", {}),
        ("vibemk_get_active_downtimes", {}),
        ("vibemk_list_service_groups", {}),
        ("vibemk_search_rulesets", {"fulltext": "temperature"}),
        ("vibemk_show_ruleset", {"ruleset_name": "checkgroup_parameters:temperature"}),
        ("vibemk_list_rulesets", {}),
        ("vibemk_list_acknowledgements", {}),
        ("vibemk_list_active_checks", {}),
        ("vibemk_get_events", {}),
        ("vibemk_get_aux_tags", {}),
        ("vibemk_get_audit_log", {}),
        ("vibemk_get_sites", {}),
        ("vibemk_list_process_rules", {}),
        ("vibemk_checkmk_rules_guide", {}),
    ]
] + [
    ("vibemk_baking_status", {}, "err")
]  # Raw/Community has no agent bakery

NO_AGENT = {"tag_agent": "no-agent", "tag_snmp_ds": "no-snmp"}
SETUP = [
    (n, a, "ok")
    for n, a in [
        ("vibemk_create_folder", {"folder": "vmktest", "parent": "/", "title": "vibeMK test"}),
        ("vibemk_create_host", {"host_name": H, "folder": "/", "attributes": {"ipaddress": "192.0.2.10", **NO_AGENT}}),
        ("vibemk_create_host_group", {"name": "vmkhg", "alias": "vmk host group"}),
        ("vibemk_create_contact_group", {"name": "vmkcg", "alias": "vmk contact group"}),
        ("vibemk_create_service_group", {"name": "vmksg", "alias": "vmk service group"}),
        (
            "vibemk_bulk_create_service_groups",
            {"entries": [{"name": "vmksg2", "alias": "two"}, {"name": "vmksg3", "alias": "three"}]},
        ),
        ("vibemk_create_user", {"username": "vmkuser", "fullname": "vmk user", "password": "Vmk-passw0rd!x"}),
        ("vibemk_create_user_role", {"base_role_id": "user", "new_role_id": "vmkrole"}),
        (
            "vibemk_create_host_tag",
            {
                "tag_id": "vmktag",
                "title": "vmk tag",
                "tags": [{"id": "vmka", "title": "A"}, {"id": "vmkb", "title": "B"}],
            },
        ),
        ("vibemk_create_aux_tag", {"tag_id": "vmkaux", "title": "vmk aux", "topic": "vmk"}),
        (
            "vibemk_create_timeperiod",
            {
                "name": "vmktp",
                "alias": "vmk tp",
                "active_time_ranges": [{"day": "monday", "time_ranges": [{"start": "08:00", "end": "17:00"}]}],
            },
        ),
        ("vibemk_create_password", {"ident": "vmkpw", "title": "vmk pw", "password": "s3cret"}),
        (
            "vibemk_create_rule",
            {
                "ruleset_name": "checkgroup_parameters:temperature",
                "rule_config": "{'levels': (38.0, 45.0)}",
                "comment": "vmk rule",
                "conditions": {
                    "host_label_groups": [
                        {"operator": "and", "label_group": [{"operator": "and", "label": "cmk/site:cmk"}]}
                    ]
                },
            },
        ),
        (
            "vibemk_create_notification_rule",
            {"rule_config": "$NOTIF_CFG"},
        ),
        ("vibemk_activate_changes", {}),
    ]
] + [
    verify("folder created", lambda: rest("objects/folder_config/~vmktest")["title"] == "vibeMK test"),
    verify(
        "host created with its IP", lambda: ext(f"objects/host_config/{H}")["attributes"]["ipaddress"] == "192.0.2.10"
    ),
    verify("host group created", lambda: exists("objects/host_group_config/vmkhg")),
    verify("contact group created", lambda: exists("objects/contact_group_config/vmkcg")),
    verify(
        "service groups created",
        lambda: all(exists(f"objects/service_group_config/{n}") for n in ("vmksg", "vmksg2", "vmksg3")),
    ),
    verify("user created", lambda: ext("objects/user_config/vmkuser")["fullname"] == "vmk user"),
    verify("role created", lambda: exists("objects/user_role/vmkrole")),
    verify("tag group created with 2 tags", lambda: len(ext("objects/host_tag_group/vmktag")["tags"]) == 2),
    verify("aux tag created", lambda: exists("objects/aux_tag/vmkaux")),
    verify("time period created", lambda: exists("objects/time_period/vmktp")),
    verify("password created", lambda: exists("objects/password/vmkpw")),
    verify(
        "rule created with its value and label condition",
        lambda: (
            "38.0" in ext(f"objects/rule/{RESOLVE['$RULE']()}")["value_raw"]
            and ext(f"objects/rule/{RESOLVE['$RULE']()}")["conditions"]["host_label_groups"]
        ),
    ),
    verify("notification rule created", lambda: RESOLVE["$NOTIF"]() is not None),
    until(activated, 90),
]

OPS = [
    ("vibemk_get_host_status", {"host_name": H}, "ok"),
    ("vibemk_get_host_status", {"host_name": "vmk-does-not-exist"}, "err"),
    ("vibemk_get_host_details", {"host_name": H}, "ok"),
    ("vibemk_get_host_config", {"host_name": H}, "ok"),
    ("vibemk_get_host_effective_attributes", {"host_name": H}, "ok"),
    ("vibemk_validate_host_config", {"host_name": H}, "ok"),
    (
        "vibemk_compare_host_states",
        {"host_name": H, "desired_attributes": {"ipaddress": "192.0.2.10"}},
        "ok",
    ),
    ("vibemk_get_checkmk_hosts", {"folder": "/", "effective_attributes": True}, "ok"),
    ("vibemk_get_checkmk_services", {"host_name": H}, "ok"),
    ("vibemk_get_current_problems", {"host_name": H}, "ok"),
    (
        "vibemk_update_host",
        {"host_name": H, "attributes": {"alias": "vmk alias"}},
        "ok",
    ),
    verify(
        "update_host set the alias",
        lambda: ext(f"objects/host_config/{H}")["attributes"].get("alias") == "vmk alias",
    ),
    (
        "vibemk_update_host",
        {"host_name": H, "update_mode": "remove", "remove_attributes": ["alias"]},
        "ok",
    ),
    verify(
        "update_host remove dropped the alias",
        lambda: "alias" not in ext(f"objects/host_config/{H}")["attributes"],
    ),
    (
        "vibemk_get_service_status",
        {"host_name": H, "service_description": "PING"},
        "ok",
    ),
    ("vibemk_get_host_metrics", {"host_name": H}, "ok"),
    (
        "vibemk_get_service_metrics",
        {"host_name": H, "service_description": "PING"},
        "ok",
    ),
    ("vibemk_list_available_metrics", {"host_name": H}, "ok"),
    (
        "vibemk_search_metrics",
        {"host_filter": H, "service_filter": "PING", "metric_id": "rta"},
        "ok",
    ),
    ("vibemk_get_custom_graph", {"custom_graph_id": "nope"}, "err"),
    (
        "vibemk_bulk_create_hosts",
        {
            "entries": [
                {
                    "host_name": H2 + "b",
                    "folder": "/",
                    "attributes": {"ipaddress": "192.0.2.11", **NO_AGENT},
                }
            ]
        },
        "ok",
    ),
    verify(
        "bulk_create_hosts created the host",
        lambda: exists(f"objects/host_config/{H2}b"),
    ),
    (
        "vibemk_bulk_update_hosts",
        {"entries": [{"host_name": H2 + "b", "attributes": {"alias": "bulk"}}]},
        "ok",
    ),
    verify(
        "bulk_update_hosts set the alias",
        lambda: ext(f"objects/host_config/{H2}b")["attributes"].get("alias") == "bulk",
    ),
    ("vibemk_clone_host", {"source_hostname": H, "target_hostname": H2}, "ok"),
    verify(
        "clone_host copied the IP",
        lambda: ext(f"objects/host_config/{H2}")["attributes"].get("ipaddress") == "192.0.2.10",
    ),
    ("vibemk_move_host", {"host_name": H2, "target_folder": FOLDER}, "ok"),
    verify(
        "move_host moved it",
        lambda: ext(f"objects/host_config/{H2}")["folder"] == FOLDER,
    ),
    ("vibemk_get_folder_hosts", {"folder": FOLDER}, "ok"),
    ("vibemk_update_folder", {"folder": FOLDER, "title": "vibeMK test 2"}, "ok"),
    verify(
        "update_folder set the title",
        lambda: rest("objects/folder_config/~vmktest")["title"] == "vibeMK test 2",
    ),
    ("vibemk_create_cluster_host", {"host_name": CL, "nodes": [H, H2]}, "ok"),
    verify(
        "create_cluster_host set the nodes",
        lambda: sorted(ext(f"objects/host_config/{CL}")["cluster_nodes"]) == [H, H2],
    ),
    ("vibemk_get_ruleset", {"ruleset_name": "checkgroup_parameters:temperature"}, "ok"),
    ("vibemk_get_rulesets", {"search": "temperature"}, "ok"),
    (
        "vibemk_update_rule",
        {"rule_id": "$RULE", "rule_config": "{'levels': (40.0, 45.0)}"},
        "ok",
    ),
    verify(
        "update_rule changed the value and kept the label condition",
        lambda: (
            "40.0" in ext(f"objects/rule/{LAST['vibemk_update_rule']['rule_id']}")["value_raw"]
            and ext(f"objects/rule/{LAST['vibemk_update_rule']['rule_id']}")["conditions"]["host_label_groups"]
        ),
    ),
    ("vibemk_move_rule", {"rule_id": "$RULE", "position": "top_of_folder"}, "ok"),
    (
        "vibemk_create_host_contactgroup_rule",
        {"contact_groups": ["vmkcg"], "comment": "vmk"},
        "ok",
    ),
    verify("contact group rule created", lambda: RESOLVE["$CG_RULE"]() is not None),
    (
        "vibemk_create_host_hostgroup_rule",
        {"host_groups": ["vmkhg"], "comment": "vmk"},
        "ok",
    ),
    verify("host group rule created", lambda: RESOLVE["$HG_RULE"]() is not None),
    ("vibemk_update_host_group", {"name": "vmkhg", "alias": "vmk hg 2"}, "ok"),
    verify(
        "host group alias changed",
        lambda: rest("objects/host_group_config/vmkhg")["title"] == "vmk hg 2",
    ),
    ("vibemk_update_contact_group", {"name": "vmkcg", "alias": "vmk cg 2"}, "ok"),
    verify(
        "contact group alias changed",
        lambda: rest("objects/contact_group_config/vmkcg")["title"] == "vmk cg 2",
    ),
    ("vibemk_get_service_group", {"name": "vmksg"}, "ok"),
    ("vibemk_update_service_group", {"name": "vmksg", "alias": "vmk sg 2"}, "ok"),
    verify(
        "service group alias changed",
        lambda: rest("objects/service_group_config/vmksg")["title"] == "vmk sg 2",
    ),
    (
        "vibemk_bulk_update_service_groups",
        {"entries": [{"name": "vmksg2", "attributes": {"alias": "two b"}}]},
        "ok",
    ),
    verify(
        "bulk service group alias changed",
        lambda: rest("objects/service_group_config/vmksg2")["title"] == "two b",
    ),
    ("vibemk_update_user", {"username": "vmkuser", "fullname": "vmk user 2"}, "ok"),
    verify(
        "update_user set the name",
        lambda: ext("objects/user_config/vmkuser")["fullname"] == "vmk user 2",
    ),
    ("vibemk_add_user_to_group", {"username": "vmkuser", "group_name": "vmkcg"}, "ok"),
    verify(
        "user is in the group",
        lambda: "vmkcg" in ext("objects/user_config/vmkuser").get("contactgroups", []),
    ),
    (
        "vibemk_remove_user_from_group",
        {"username": "vmkuser", "group_name": "vmkcg"},
        "ok",
    ),
    verify(
        "user left the group",
        lambda: "vmkcg" not in ext("objects/user_config/vmkuser").get("contactgroups", []),
    ),
    ("vibemk_update_user_role", {"role_id": "vmkrole", "alias": "vmk role 2"}, "ok"),
    verify(
        "role alias changed",
        lambda: ext("objects/user_role/vmkrole")["alias"] == "vmk role 2",
    ),
    ("vibemk_update_host_tag", {"tag_id": "vmktag", "title": "vmk tag 2"}, "ok"),
    verify(
        "tag group title changed",
        lambda: rest("objects/host_tag_group/vmktag")["title"] == "vmk tag 2",
    ),
    ("vibemk_update_aux_tag", {"tag_id": "vmkaux", "title": "vmk aux 2"}, "ok"),
    verify(
        "aux tag title changed",
        lambda: rest("objects/aux_tag/vmkaux")["title"] == "vmk aux 2",
    ),
    ("vibemk_update_timeperiod", {"name": "vmktp", "alias": "vmk tp 2"}, "ok"),
    verify(
        "time period alias changed",
        lambda: rest("objects/time_period/vmktp")["title"] == "vmk tp 2",
    ),
    ("vibemk_update_password", {"ident": "vmkpw", "password": "s3cret2"}, "ok"),
    ("vibemk_get_notification_rule", {"rule_id": "$NOTIF"}, "ok"),
    verify(
        "get_notification_rule shows the description",
        lambda: "Description: vmk notif\n" in SEEN["vibemk_get_notification_rule"],
    ),
    (
        "vibemk_update_notification_rule",
        {"rule_id": "$NOTIF", "rule_config": "$NOTIF_CFG2"},
        "ok",
    ),
    verify(
        "notification rule description changed",
        lambda: (
            ext(f"objects/notification_rule/{LAST['vibemk_update_notification_rule']['rule_id']}")["rule_config"][
                "rule_properties"
            ]["description"]
            == "vmk notif 2"
        ),
    ),
    ("vibemk_get_notification_rules", {}, "ok"),
    verify(
        "get_notification_rules lists the description",
        lambda: "**vmk notif 2**" in SEEN["vibemk_get_notification_rules"],
    ),
    ("vibemk_create_http_check", {"hostname": H, "uri": "/"}, "ok"),
    ("vibemk_create_tcp_check", {"hostname": H, "port": 22}, "ok"),
    ("vibemk_create_icmp_check", {"hostname": H}, "ok"),
    (
        "vibemk_create_custom_check",
        {"hostname": H, "service_description": "vmk custom", "command_line": "echo ok"},
        "ok",
    ),
    ("vibemk_create_dns_check", {"hostname": H}, "ok"),
    ("vibemk_create_smtp_check", {"hostname": H}, "ok"),
    ("vibemk_create_ftp_check", {"hostname": H}, "ok"),
    ("vibemk_create_ldap_check", {"hostname": H, "base_dn": "dc=example,dc=org"}, "ok"),
    ("vibemk_create_smb_check", {"hostname": H, "share": "data"}, "ok"),
    ("vibemk_create_mkevents_check", {"hostname": H}, "ok"),
    ("vibemk_create_inventory_check", {"hostname": H}, "ok"),
    verify(
        "each active check is a rule for the host",
        lambda: all(
            host_rule(rs)
            for rs in (
                "active_checks:http",
                "active_checks:tcp",
                "active_checks:icmp",
                "custom_checks",
                "active_checks:dns",
                "active_checks:smtp",
                "active_checks:ftp",
                "active_checks:ldap",
                "active_checks:disk_smb",
                "active_checks:mkevents",
                "active_checks:cmk_inv",
            )
        ),
    ),
    ("vibemk_list_active_checks", {}, "ok"),
    ("vibemk_delete_active_check", {"hostname": H, "check_type": "tcp"}, "ok"),
    verify(
        "delete_active_check removed the tcp rule",
        lambda: host_rule("active_checks:tcp") is None,
    ),
    (
        "vibemk_set_process_thresholds",
        {"hostname": H, "process_name": "sshd", "warn_max": 5, "crit_max": 10},
        "ok",
    ),
    verify("process rule created", lambda: RESOLVE["$PROC_RULE"]() is not None),
    (
        "vibemk_set_interface_params",
        {"hostname": H, "interface_name": "eth0", "expected_speed_mbit": 1000},
        "ok",
    ),
    verify(
        "interface rule created",
        lambda: host_rule("checkgroup_parameters:interfaces") is not None,
    ),
    (
        "vibemk_set_memory_thresholds",
        {"hostname": H, "ram_warn_percent": 80, "ram_crit_percent": 90},
        "ok",
    ),
    verify(
        "memory rule created",
        lambda: host_rule("checkgroup_parameters:memory_linux") is not None,
    ),
    ("vibemk_list_process_rules", {}, "ok"),
    ("vibemk_delete_service_param_rule", {"rule_id": "$PROC_RULE"}, "ok"),
    verify(
        "process rule deleted",
        lambda: gone(f"objects/rule/{LAST['vibemk_delete_service_param_rule']['rule_id']}"),
    ),
    ("vibemk_activate_changes", {}, "ok"),
    until(activated, 90),
    ("vibemk_start_service_discovery", {"host_name": H, "mode": "refresh"}, "ok"),
    ("vibemk_wait_for_discovery", {"host_name": H}, "ok"),
    ("vibemk_get_discovery_status", {"host_name": H}, "ok"),
    ("vibemk_get_discovery_background_job", {"host_name": H}, "ok"),
    verify(
        "discovery job shows its real state",
        lambda: (
            "State: FINISHED" in SEEN["vibemk_get_discovery_background_job"]
            and "Not finished" not in SEEN["vibemk_get_discovery_background_job"]
        ),
    ),
    ("vibemk_start_bulk_discovery", {"hostnames": [H]}, "ok"),
    ("vibemk_get_bulk_discovery_status", {"job_id": "$JOB"}, "ok"),
    (
        "vibemk_add_comment",
        {"comment_type": "host", "host_name": H, "comment": "vmk comment"},
        "ok",
    ),
    (
        "vibemk_add_comment",
        {
            "comment_type": "service",
            "host_name": H,
            "service_description": "PING",
            "comment": "vmk svc",
        },
        "ok",
    ),
    until(lambda: comment("vmk comment") and comment("vmk svc"), 30),
    verify(
        "service comment is on PING",
        lambda: comment("vmk svc")["service_description"] == "PING",
    ),
    ("vibemk_get_comments", {}, "ok"),
    ("vibemk_delete_comment", {"comment_id": "$COMMENT"}, "ok"),
    until(
        lambda: listed("comment", LAST["vibemk_delete_comment"]["comment_id"]) is None,
        30,
    ),
    (
        "vibemk_delete_comment",
        {"delete_type": "by_query", "host_name": H, "service_description": "PING"},
        "ok",
    ),
    until(lambda: comment("vmk svc") is None, 30),
    (
        "vibemk_schedule_downtime",
        {
            "downtime_type": "host",
            "host_name": H,
            "start_time": "$START",
            "end_time": "$END",
            "comment": "vmk dt",
        },
        "ok",
    ),
    (
        "vibemk_schedule_host_downtime",
        {"host_name": H, "duration": 30, "comment": "vmk dt2"},
        "ok",
    ),
    (
        "vibemk_schedule_service_downtime",
        {
            "host_name": H,
            "service_descriptions": ["PING"],
            "duration": 30,
            "comment": "vmk dt3",
        },
        "ok",
    ),
    (
        "vibemk_schedule_downtime",
        {
            "downtime_type": "service",
            "host_name": H,
            "service_description": "PING",
            "start_time": "$START",
            "end_time": "$END",
            "comment": "vmk dt4",
        },
        "ok",
    ),
    until(
        lambda: all(downtime(c) for c in ("vmk dt", "vmk dt2", "vmk dt3", "vmk dt4")),
        30,
    ),
    verify(
        "service downtimes are on PING",
        lambda: downtime("vmk dt3")["extensions"].get("service_description") == "PING",
    ),
    ("vibemk_check_host_downtime_status", {"host_name": H}, "ok"),
    ("vibemk_list_downtimes", {"host_name": H, "active_only": True}, "ok"),
    # The Nagios core (Raw, Community) logs "Unrecognized external command" for MODIFY_HOST_DOWNTIME
    # while the API answers 204: the tool must say it was not applied, and the end must not have moved.
    # shortcut: both test sites run the Nagios core; against a CMC site, expect "ok" and the new end.
    ("vibemk_modify_downtime", {"downtime_id": "$DT", "end_time": "$END2"}, "err"),
    verify(
        "the downtime end did not move",
        lambda: (
            abs(
                unix(listed("downtime", LAST["vibemk_modify_downtime"]["downtime_id"])["end_time"])
                - unix(LAST["vibemk_modify_downtime"]["end_time"])
            )
            >= 120
        ),
    ),
    ("vibemk_delete_downtime", {"downtime_id": "$DT"}, "ok"),
    until(
        lambda: listed("downtime", LAST["vibemk_delete_downtime"]["downtime_id"]) is None,
        30,
    ),
    # An acknowledgement is refused while the service has never been checked, which made
    # this step depend on timing on 2.3: wait for CheckMK's own first check instead.
    until(lambda: (service(H, "PING") or {}).get("has_been_checked") == 1, 240),
    (
        "vibemk_acknowledge_problem",
        {"acknowledge_type": "host", "host_name": H, "comment": "vmk ack"},
        "ok",
    ),
    (
        "vibemk_acknowledge_service_problem",
        {"host_name": H, "service_description": "PING", "comment": "vmk ack svc"},
        "ok",
    ),
    until(lambda: ext(f"objects/host/{H}?columns=acknowledged")["acknowledged"] == 1, 30),
    until(lambda: service(H, "PING")["acknowledged"] == 1, 30),
    ("vibemk_list_acknowledgements", {}, "ok"),
    ("vibemk_remove_acknowledgement", {"host_name": H}, "ok" if CAN_REMOVE_ACK else "err"),
    until(lambda: ext(f"objects/host/{H}?columns=acknowledged")["acknowledged"] == ACK_AFTER_REMOVE, 30),
    (
        "vibemk_remove_acknowledgement",
        {"host_name": H, "service_description": "PING"},
        "ok" if CAN_REMOVE_ACK else "err",
    ),
    until(lambda: service(H, "PING")["acknowledged"] == ACK_AFTER_REMOVE, 30),
    (
        "vibemk_acknowledge_problem",
        {
            "acknowledge_type": "service",
            "host_name": H,
            "service_description": "PING",
            "comment": "vmk ack sticky",
            "sticky": True,
            "persistent": True,
            "notify": False,
        },
        "ok",
    ),
    until(lambda: service(H, "PING")["acknowledged"] == 1, 30),
    ("vibemk_acknowledge_host_problem", {"host_name": H, "comment": "vmk ack 2"}, "ok"),
    until(lambda: ext(f"objects/host/{H}?columns=acknowledged")["acknowledged"] == 1, 30),
    ("vibemk_acknowledge_event", {"event_id": 999999}, "err"),
    ("vibemk_change_event_state", {"event_id": 999999, "new_state": "ok"}, "err"),
    ("vibemk_delete_events", {"event_ids": [999999]}, "ok"),
    (
        "vibemk_login_site",
        {"site_id": SITE, "username": "cmkadmin", "password": "x"},
        "err",
    ),
    ("vibemk_logout_site", {"site_id": SITE}, "ok"),
    ("vibemk_bake_agents", {}, "err"),
    ("vibemk_download_agent_by_host", {"host_name": H}, "ok"),
    ("vibemk_get_audit_log", {"object_type": "Host", "object_id": H, "limit": 5}, "ok"),
    # A rename refuses to start while changes are pending.
    ("vibemk_activate_changes", {}, "ok"),
    until(activated, 90),
    ("vibemk_rename_host", {"host_name": H2 + "b", "new_name": H2 + "c"}, "ok"),
    until(
        lambda: exists(f"objects/host_config/{H2}c") and gone(f"objects/host_config/{H2}b"),
        120,
    ),
    verify(
        "renamed host kept its alias",
        lambda: ext(f"objects/host_config/{H2}c")["attributes"].get("alias") == "bulk",
    ),
    ("vibemk_get_audit_log", {}, "ok"),
    (
        "vibemk_show_ruleset",
        {"ruleset_name": "checkgroup_parameters:temperature"},
        "ok",
    ),
    verify(
        "show_ruleset shows the real title",
        lambda: (
            "**Title:** No title" not in SEEN["vibemk_show_ruleset"] and "Temperature" in SEEN["vibemk_show_ruleset"]
        ),
    ),
    ("vibemk_get_rulesets", {}, "ok"),
    verify(
        "get_rulesets never prints Help: None",
        lambda: "Help: None" not in SEEN["vibemk_get_rulesets"],
    ),
]

CLEANUP = [
    (n, a, "ok")
    for n, a in [
        ("vibemk_activate_changes", {}),
        ("vibemk_delete_rule", {"rule_id": "$RULE"}),
        ("vibemk_delete_notification_rule", {"rule_id": "$NOTIF"}),
        ("vibemk_delete_rule", {"rule_id": "$CG_RULE"}),
        ("vibemk_delete_rule", {"rule_id": "$HG_RULE"}),
        # The active checks OPS left on H (tcp was deleted there already).
        *(
            ("vibemk_delete_active_check", {"hostname": H, "check_type": t})
            for t in ("http", "icmp", "custom", "dns", "smtp", "ftp", "ldap", "disk_smb", "mkevents", "cmk_inv")
        ),
        ("vibemk_delete_rule", {"rule_id": "$IF_RULE"}),
        ("vibemk_delete_rule", {"rule_id": "$MEM_RULE"}),
        ("vibemk_delete_host", {"host_name": CL}),
        ("vibemk_delete_host", {"host_name": H2}),
        ("vibemk_delete_host", {"host_name": H2 + "c"}),
        ("vibemk_delete_host", {"host_name": H}),
        ("vibemk_delete_folder", {"folder": FOLDER}),
        ("vibemk_delete_service_group", {"name": "vmksg"}),
        ("vibemk_bulk_delete_service_groups", {"entries": ["vmksg2", "vmksg3"]}),
        ("vibemk_delete_user", {"username": "vmkuser"}),
        ("vibemk_delete_user_role", {"role_id": "vmkrole"}),
        ("vibemk_delete_contact_group", {"name": "vmkcg"}),
        ("vibemk_delete_host_group", {"name": "vmkhg"}),
        ("vibemk_delete_host_tag", {"tag_id": "vmktag"}),
        ("vibemk_delete_aux_tag", {"tag_id": "vmkaux"}),
        ("vibemk_delete_timeperiod", {"name": "vmktp"}),
        ("vibemk_delete_password", {"ident": "vmkpw"}),
        ("vibemk_create_folder", {"folder": "vmkmove", "parent": "/", "title": "move me"}),
        ("vibemk_create_folder", {"folder": "vmkdest", "parent": "/", "title": "dest"}),
        ("vibemk_move_folder", {"folder": "/vmkmove", "destination": "/vmkdest"}),
        ("vibemk_delete_folder", {"folder": "/vmkdest/vmkmove"}),
        ("vibemk_delete_folder", {"folder": "/vmkdest"}),
        ("vibemk_activate_changes", {}),
    ]
] + [
    verify(
        "every test object is gone",
        lambda: all(
            gone(p)
            for p in [f"objects/host_config/{h}" for h in (H, H2, H2 + "c", CL)]
            + [
                "objects/folder_config/~vmktest",
                "objects/folder_config/~vmkdest",
                "objects/host_group_config/vmkhg",
                "objects/contact_group_config/vmkcg",
                "objects/service_group_config/vmksg",
                "objects/service_group_config/vmksg2",
                "objects/service_group_config/vmksg3",
                "objects/user_config/vmkuser",
                "objects/user_role/vmkrole",
                "objects/host_tag_group/vmktag",
                "objects/aux_tag/vmkaux",
                "objects/time_period/vmktp",
                "objects/password/vmkpw",
                f"objects/rule/{LAST['vibemk_update_rule']['rule_id']}",
                f"objects/notification_rule/{LAST['vibemk_update_notification_rule']['rule_id']}",
            ]
        ),
    ),
    verify("no rule names a test host any more", no_rule_names_a_test_host),
]


class Server:
    def __init__(self):
        self.p = subprocess.Popen(
            CMD,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=open(
                os.path.join(os.path.dirname(os.path.abspath(__file__)), f"live_sweep_{LABEL}.stderr.log"), "a"
            ),
            text=True,
            encoding="utf-8",
            bufsize=1,
            env=dict(os.environ, PYTHONUTF8="1"),
        )
        self.n = 0
        self.rpc(
            "initialize",
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "sweep", "version": "0"}},
        )
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise EOFError(f"server exited (code {self.p.poll()})")
            msg = json.loads(line)
            if msg.get("id") == self.n:
                return msg


def main():
    s = Server()
    listed = {t["name"] for t in s.rpc("tools/list", {})["result"]["tools"]}
    rows = []

    def run(steps, phase):
        nonlocal s
        for tool, args, expected in steps:
            if tool == "sleep":  # CheckMK applies commands and activations asynchronously
                time.sleep(args)
                continue
            t = time.perf_counter()
            if tool in ("verify", "until"):
                check, deadline = args, t + (expected if tool == "until" else 0)
                label = expected if tool == "verify" else f"waited for {rows[-1]['tool']} to take effect"
                while True:
                    try:
                        held, why = bool(check()), ""
                    except Exception as e:  # a missing object or field is a failed check, not a crash
                        held, why = False, repr(e)
                    if held or time.perf_counter() >= deadline:
                        break
                    time.sleep(2)
                rows.append(
                    dict(
                        phase=phase,
                        tool=f"{tool}:{label}",
                        args={},
                        expected="ok",
                        got="ok" if held else "err",
                        ok=held,
                        ms=round((time.perf_counter() - t) * 1e3),
                        text=why,
                    )
                )
                print(f"{'PASS' if held else 'FAIL'} {tool}: {label} {why}", flush=True)
                continue
            try:
                a = resolve(args)
            except Exception as e:  # an unresolvable $id: an earlier step did not create it
                rows.append(
                    dict(phase=phase, tool=tool, args=args, expected=expected, got="unresolved", ok=False, text=repr(e))
                )
                print(f"FAIL {tool}: unresolved {e!r}", flush=True)
                continue
            try:
                msg = s.rpc("tools/call", {"name": tool, "arguments": a})
            except Exception as e:  # the server died on this call: record it, start a fresh one, go on
                rows.append(
                    dict(phase=phase, tool=tool, args=a, expected=expected, got="server-died", ok=False, text=repr(e))
                )
                print(f"FAIL {tool}: server died {e!r}", flush=True)
                s = Server()
                continue
            if "error" in msg:
                got, text = "protocol-error", json.dumps(msg["error"])
            else:
                r = msg["result"]
                got = "err" if r.get("isError") else "ok"
                text = "\n".join(c.get("text", "") for c in r.get("content", []))
            SEEN[tool], LAST[tool] = text, a
            ok = got == expected
            rows.append(
                dict(
                    phase=phase,
                    tool=tool,
                    args=a,
                    expected=expected,
                    got=got,
                    ok=ok,
                    ms=round((time.perf_counter() - t) * 1e3),
                    placeholders=(
                        sorted({m.group(0).lower() for m in PLACEHOLDER.finditer(text)}) if got == "ok" else []
                    ),
                    text=text[:1500],
                )
            )
            print(f"{'PASS' if ok else 'FAIL'} {tool} [{got}] {text[:160].replace(chr(10), ' | ')}", flush=True)

    run(READS, "read")
    run(SETUP, "setup")
    print("... waiting 100 s for the first checks of the new hosts", flush=True)
    time.sleep(100)
    run(OPS, "ops")
    run(CLEANUP, "cleanup")
    called = {r["tool"] for r in rows}
    out = dict(
        label=LABEL,
        listed=len(listed),
        called=len(called & listed),
        not_called=sorted(listed - called),
        passed=sum(r["ok"] for r in rows),
        steps=len(rows),
        verified=sum(r["tool"].startswith(("verify:", "until:")) for r in rows),
        placeholders={r["tool"]: r["placeholders"] for r in rows if r.get("placeholders")},
        rows=rows,
    )
    json.dump(
        out,
        open(
            os.path.join(os.path.dirname(os.path.abspath(__file__)), f"live_sweep_{LABEL}.json"), "w", encoding="utf-8"
        ),
        indent=1,
        ensure_ascii=False,
    )
    print(
        f"\n{LABEL}: {out['passed']}/{out['steps']} steps pass ({out['verified']} read-backs), "
        f"{out['called']}/{out['listed']} tools called, not called: {out['not_called']}"
    )
    for tool, words in out["placeholders"].items():
        print(f"placeholder? {tool}: {', '.join(words)}")


if __name__ == "__main__":
    main()
