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

import base64, datetime as dt, json, os, re, subprocess, sys, time, urllib.request

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


def iso(minutes):
    return (dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=minutes)).strftime("%Y-%m-%dT%H:%M:%SZ")


H, H2, CL, FOLDER = "vmk-h1", "vmk-h2", "vmk-cl", "/vmktest"
SEEN = {}  # text of the last answer per tool, for ids that only the answer carries


def first(values, pred=lambda v: True):
    return next((v for v in values if pred(v)), None)


def vmk_rule(ruleset):
    rules = rest(f"domain-types/rule/collections/all?ruleset_name={ruleset}")["value"]
    return first(r["id"] for r in rules if "vmk" in json.dumps(r["extensions"]))


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
    "$PROC_RULE": lambda: first(
        r["id"] for r in rest("domain-types/rule/collections/all?ruleset_name=inventory_processes_rules")["value"]
    ),
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
            c["id"] for c in rest("domain-types/comment/collections/all")["value"] if c["extensions"]["host_name"] == H
        )
    ),
    "$JOB": lambda: re.search(
        r"(bulk_discovery[\w-]*|[0-9a-f-]{8,})", SEEN.get("vibemk_start_bulk_discovery", "")
    ).group(1),
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
] + [("vibemk_baking_status", {}, "err")]  # Raw/Community has no agent bakery

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
]

OPS = [
    ("vibemk_get_host_status", {"host_name": H}, "ok"),
    ("vibemk_get_host_details", {"host_name": H}, "ok"),
    ("vibemk_get_host_config", {"host_name": H}, "ok"),
    ("vibemk_get_host_effective_attributes", {"host_name": H}, "ok"),
    ("vibemk_validate_host_config", {"host_name": H}, "ok"),
    ("vibemk_compare_host_states", {"host_name": H, "desired_attributes": {"ipaddress": "192.0.2.10"}}, "ok"),
    ("vibemk_update_host", {"host_name": H, "attributes": {"alias": "vmk alias"}}, "ok"),
    ("vibemk_get_service_status", {"host_name": H, "service_description": "PING"}, "ok"),
    ("vibemk_get_host_metrics", {"host_name": H}, "ok"),
    ("vibemk_get_service_metrics", {"host_name": H, "service_description": "PING"}, "ok"),
    ("vibemk_list_available_metrics", {"host_name": H}, "ok"),
    ("vibemk_search_metrics", {"host_filter": H, "service_filter": "PING", "metric_id": "rta"}, "ok"),
    ("vibemk_get_custom_graph", {"custom_graph_id": "nope"}, "err"),
    (
        "vibemk_bulk_create_hosts",
        {"entries": [{"host_name": H2 + "b", "folder": "/", "attributes": {"ipaddress": "192.0.2.11", **NO_AGENT}}]},
        "ok",
    ),
    ("vibemk_bulk_update_hosts", {"entries": [{"host_name": H2 + "b", "attributes": {"alias": "bulk"}}]}, "ok"),
    ("vibemk_clone_host", {"source_hostname": H, "target_hostname": H2}, "ok"),
    ("vibemk_move_host", {"host_name": H2, "target_folder": FOLDER}, "ok"),
    ("vibemk_get_folder_hosts", {"folder": FOLDER}, "ok"),
    ("vibemk_update_folder", {"folder": FOLDER, "title": "vibeMK test 2"}, "ok"),
    ("vibemk_create_cluster_host", {"host_name": CL, "nodes": [H, H2]}, "ok"),
    ("vibemk_get_ruleset", {"ruleset_name": "checkgroup_parameters:temperature"}, "ok"),
    ("vibemk_update_rule", {"rule_id": "$RULE", "rule_config": "{'levels': (40.0, 45.0)}"}, "ok"),
    ("vibemk_move_rule", {"rule_id": "$RULE", "position": "top_of_folder"}, "ok"),
    ("vibemk_create_host_contactgroup_rule", {"contact_groups": ["vmkcg"], "comment": "vmk"}, "ok"),
    ("vibemk_create_host_hostgroup_rule", {"host_groups": ["vmkhg"], "comment": "vmk"}, "ok"),
    ("vibemk_update_host_group", {"name": "vmkhg", "alias": "vmk hg 2"}, "ok"),
    ("vibemk_update_contact_group", {"name": "vmkcg", "alias": "vmk cg 2"}, "ok"),
    ("vibemk_get_service_group", {"name": "vmksg"}, "ok"),
    ("vibemk_update_service_group", {"name": "vmksg", "alias": "vmk sg 2"}, "ok"),
    ("vibemk_bulk_update_service_groups", {"entries": [{"name": "vmksg2", "attributes": {"alias": "two b"}}]}, "ok"),
    ("vibemk_update_user", {"username": "vmkuser", "fullname": "vmk user 2"}, "ok"),
    ("vibemk_add_user_to_group", {"username": "vmkuser", "group_name": "vmkcg"}, "ok"),
    ("vibemk_remove_user_from_group", {"username": "vmkuser", "group_name": "vmkcg"}, "ok"),
    ("vibemk_update_user_role", {"role_id": "vmkrole", "alias": "vmk role 2"}, "ok"),
    ("vibemk_update_host_tag", {"tag_id": "vmktag", "title": "vmk tag 2"}, "ok"),
    ("vibemk_update_aux_tag", {"tag_id": "vmkaux", "title": "vmk aux 2"}, "ok"),
    ("vibemk_update_timeperiod", {"name": "vmktp", "alias": "vmk tp 2"}, "ok"),
    ("vibemk_update_password", {"ident": "vmkpw", "password": "s3cret2"}, "ok"),
    ("vibemk_get_notification_rule", {"rule_id": "$NOTIF"}, "ok"),
    (
        "vibemk_update_notification_rule",
        {"rule_id": "$NOTIF", "rule_config": "$NOTIF_CFG2"},
        "ok",
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
    ("vibemk_list_active_checks", {}, "ok"),
    ("vibemk_delete_active_check", {"hostname": H, "check_type": "tcp"}, "ok"),
    ("vibemk_set_process_thresholds", {"hostname": H, "process_name": "sshd", "warn_max": 5, "crit_max": 10}, "ok"),
    ("vibemk_set_interface_params", {"hostname": H, "interface_name": "eth0", "expected_speed_mbit": 1000}, "ok"),
    ("vibemk_set_memory_thresholds", {"hostname": H, "ram_warn_percent": 80, "ram_crit_percent": 90}, "ok"),
    ("vibemk_list_process_rules", {}, "ok"),
    ("vibemk_delete_service_param_rule", {"rule_id": "$PROC_RULE"}, "ok"),
    ("vibemk_activate_changes", {}, "ok"),
    ("vibemk_start_service_discovery", {"host_name": H, "mode": "refresh"}, "ok"),
    ("vibemk_wait_for_discovery", {"host_name": H}, "ok"),
    ("vibemk_get_discovery_status", {"host_name": H}, "ok"),
    ("vibemk_get_discovery_background_job", {"host_name": H}, "ok"),
    ("vibemk_start_bulk_discovery", {"hostnames": [H]}, "ok"),
    ("vibemk_get_bulk_discovery_status", {"job_id": "$JOB"}, "ok"),
    ("vibemk_add_comment", {"comment_type": "host", "host_name": H, "comment": "vmk comment"}, "ok"),
    (
        "vibemk_add_comment",
        {"comment_type": "service", "host_name": H, "service_description": "PING", "comment": "vmk svc"},
        "ok",
    ),
    ("vibemk_get_comments", {}, "ok"),
    ("vibemk_delete_comment", {"comment_id": "$COMMENT"}, "ok"),
    (
        "vibemk_schedule_downtime",
        {"downtime_type": "host", "host_name": H, "start_time": "$START", "end_time": "$END", "comment": "vmk dt"},
        "ok",
    ),
    ("vibemk_schedule_host_downtime", {"host_name": H, "duration": 30, "comment": "vmk dt2"}, "ok"),
    (
        "vibemk_schedule_service_downtime",
        {"host_name": H, "service_descriptions": ["PING"], "duration": 30, "comment": "vmk dt3"},
        "ok",
    ),
    ("vibemk_check_host_downtime_status", {"host_name": H}, "ok"),
    ("vibemk_modify_downtime", {"downtime_id": "$DT", "end_time": "$END2"}, "ok"),
    ("vibemk_delete_downtime", {"downtime_id": "$DT"}, "ok"),
    ("vibemk_acknowledge_problem", {"acknowledge_type": "host", "host_name": H, "comment": "vmk ack"}, "ok"),
    (
        "vibemk_acknowledge_service_problem",
        {"host_name": H, "service_description": "PING", "comment": "vmk ack svc"},
        "ok",
    ),
    ("sleep", 3, None),
    ("vibemk_list_acknowledgements", {}, "ok"),
    ("vibemk_remove_acknowledgement", {"host_name": H}, "ok"),
    ("vibemk_acknowledge_host_problem", {"host_name": H, "comment": "vmk ack 2"}, "ok"),
    ("vibemk_acknowledge_event", {"event_id": 999999}, "err"),
    ("vibemk_change_event_state", {"event_id": 999999, "new_state": "ok"}, "err"),
    ("vibemk_delete_events", {"event_ids": [999999]}, "ok"),
    ("vibemk_login_site", {"site_id": SITE, "username": "cmkadmin", "password": "x"}, "err"),
    ("vibemk_logout_site", {"site_id": SITE}, "ok"),
    ("vibemk_bake_agents", {}, "err"),
    ("vibemk_download_agent_by_host", {"host_name": H}, "ok"),
    # A rename refuses to start while changes are pending.
    ("vibemk_activate_changes", {}, "ok"),
    ("sleep", 20, None),
    ("vibemk_rename_host", {"host_name": H2 + "b", "new_name": H2 + "c"}, "ok"),
    ("sleep", 10, None),
    ("vibemk_get_audit_log", {}, "ok"),
]

CLEANUP = [
    (n, a, "ok")
    for n, a in [
        ("vibemk_activate_changes", {}),
        ("vibemk_delete_rule", {"rule_id": "$RULE"}),
        ("vibemk_delete_notification_rule", {"rule_id": "$NOTIF"}),
        ("vibemk_delete_rule", {"rule_id": "$CG_RULE"}),
        ("vibemk_delete_rule", {"rule_id": "$HG_RULE"}),
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
            SEEN[tool] = text
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
        f"\n{LABEL}: {out['passed']}/{out['steps']} steps pass, {out['called']}/{out['listed']} tools called, not called: {out['not_called']}"
    )


if __name__ == "__main__":
    main()
