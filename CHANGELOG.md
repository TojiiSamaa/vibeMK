# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Rust port of the server (`rust/`): same tools, schemas, flags and
  environment variables, stdio and HTTP with bearer auth; `--read-only` lists only read tools and
  refuses the others;
  CI in `.github/workflows/ci-rust.yml`

### Fixed
- Audit findings, reported in the Rust port and tested there (ids in
  `AUDIT.md`): `Authorization` is not sent on a redirect to another host (S1);
  a non-ASCII bearer token is a 401 (S3); object paths are escaped (S4);
  header lookup ignores case (E1); null `data`/`extensions` bodies no longer
  crash the write report (E2, E11, N1); a retry keeps redirects off (E3);
  `Retry-After` is capped (E4); `update_host` gives the concurrency advice on
  412/428 (E6); the `*` ETag fallback is logged (E8); an activation is
  reported as started (E9a); `time_ago` of a future date is 0 (E10a)
- Whether a call failed no longer depends on how its answer is worded: a
  status shown with ❌, such as a failed background job, is reported as a
  result, not as a failed call
- The host group rule and debug tools show line breaks instead of a literal `\n`
- Found by running every tool against real CheckMK 2.3.0p52 and 2.5.0p18
  (`scripts/live_sweep.py`), fixed in Python and Rust:
  - `add_comment` sends the required `comment_type`; `delete_comment` by query
    uses `delete_type: params` and `service_descriptions`
  - `create_cluster_host` posts to `host_config/collections/clusters`
  - `modify_downtime` sends the body CheckMK expects (by id or by host/service,
    `end_time` as `{modify_type: absolute, value}`)
  - `start_bulk_discovery` reads the job id from the 303 on 2.5 (following it
    lost the credentials, 401); `get_bulk_discovery_status` falls back to
    `discovery_run` on 2.3
  - `search_metrics` uses `metric/actions/get/invoke`, the only metric route
    left; `service_filter` is now required
  - `move_host` and `move_folder` send `If-Match` (428 before)
  - `rename_host` treats the 302/303 that starts the job as success
  - `activate_changes` rereads the pending changes once after a 412 (a rename
    job activates on its own)
  - `logout_site` sends no body (2.5 rejects one)
  - `create_host_hostgroup_rule` creates one rule per group
  - `create_rule`/`update_rule` keep a `rule_config` that already is a Python
    literal, so tuples survive
  - `create_aux_tag` requires and sends `topic`
  - LDAP and SMB active checks send plain `(warn, crit)` levels on 2.3
- Found by the second run of the live tests and sweeps (6 Nov 2026), fixed in Python and Rust:
  - `remove_acknowledgement` uses `acknowledge/actions/delete` (deleting the
    comment left the problem acknowledged); CheckMK 2.3 has no such route and
    gets a clear message
  - ruleset searches (the ruleset list and the host group rule search) send `fulltext`,
    the only search field CheckMK knows; a ruleset without help no longer shows "None"
  - `modify_downtime` reads the downtime back and reports "Downtime not
    modified" when the Nagios core ignores the command (Raw and Community)
  - the discovery background job shows its state, activity and log lines instead of
    start and end times CheckMK does not send
  - notification rules show the description from `rule_properties`
  - `get_ruleset` reads the title where `objects/ruleset` puts it and no
    longer prints type, match type and help that only the collection holds
  - `activate_changes` on 2.3 waits for an activation already running; the
    site refused the second one ("Locked") and left changes pending. The
    activation that write tools run on their own (`activate_changes=true`,
    `set_process_thresholds`) waits the same way
  - HTTP, TCP, SMTP, LDAP and SMB active checks send the legacy shapes on 2.3
    (whole-day certificate age, response time in ms, `(user, ('password', secret))`,
    tuple `expect_regex`); 2.3 answered 400 to the 2.5 shapes
- Test bench (`scripts/live_sweep.py`, live tests):
  - the sweep deleted "the first rule" of `inventory_processes_rules` instead of
    its own; on 2.5, which ships default process rules, each run deleted one of
    CheckMK's and left its own behind. It now deletes the rule it wrote for its host
  - the sweep removes the active check, interface and memory rules it creates
    (deleting a host keeps the rules that name it) and its last step checks that
    no rule names a test host
  - the Rust integration test called a tool that does not exist
    (`vibemk_get_all_services`, now `vibemk_get_checkmk_services`)
  - the agent bakery live test recognises the free edition on 2.3 (`.cre`) as
    well as on 2.5 (`.community`)

## [0.6.4] - 2026-10-03

### Added
- Service status, current problems, downtimes and metrics also return
  machine-readable data
- Current problems say whether each one is acknowledged or in a downtime

### Fixed
- Listing a host's or service's available metrics no longer reports none
- Host metrics can be retrieved (every request was refused)
- A metric's latest value no longer reads "None", and a window without values
  no longer fails
- A CRITICAL service's status is no longer reported as a failed tool call
- `vibemk_list_downtimes` with `active_only` leaves out downtimes that have not
  started yet
- Active checks can be created with their options on CheckMK 2.5: thresholds,
  certificate lifetimes, passwords and host choices of the HTTP, TCP, ICMP,
  SMTP, LDAP, SMB and Event Console checks were refused
- FTP checks no longer offer a passive mode, which CheckMK does not have
- Event Console events can be acknowledged, changed and deleted again
- The agent download link points at the right address; the baking status
  shows the actual state, and Raw/Community says it has no agent bakery
- The bulk discovery status shows the job's state and per-host results
- A service group's alias is shown

## [0.6.3] - 2026-10-03

### Added
- Upgrade guide for source installations older than 0.5 (INSTALL.md → Upgrading)

### Changed
- Started with a Python older than 3.10 or without its dependencies, `main.py`
  says what to do instead of failing with a traceback

## [0.6.2] - 2026-10-03

### Added
- Listed in the official MCP Registry as `io.github.chexma/vibemk`
- Installation with `pipx`, `uv tool install` or `uvx` documented

### Changed
- The MCP SDK is held below 3.0 so an untested major version cannot break a fresh install

## [0.6.1] - 2026-10-03

### Added
- vibeMK is published on PyPI: `pip install vibemk`

## [0.6.0] - 2026-10-02

### Changed
- vibeMK installs as a single `vibemk` package and no longer adds `api`,
  `config`, `handlers` and `utils` to the Python environment, where they could
  clash with other packages. `python main.py` from a checkout keeps working

### Added
- Read-only mode: `--read-only` or `VIBEMK_READ_ONLY=1` offers only the tools
  that read and refuses every write
- Tool arguments are checked against each tool's schema; a missing or wrong
  argument is answered with every problem at once instead of reaching CheckMK
- An argument a tool does not know (such as `hostname` for `host_name`) is
  named and refused instead of being silently ignored
- Downtime scheduling takes a `force` option to add a downtime even when one
  with the same comment exists

### Security
- `vibemk_test_direct_url` only requests URLs under the configured CheckMK API
  and no longer follows redirects; it could send the CheckMK credentials to any
  address
- Host names, rule ids and other values are encoded in API paths, so a crafted
  value can no longer address a different object

### Fixed
- The `vibemk` command works after `pip install`; it previously exited
  without starting the server
- An installed vibeMK no longer places `examples`, `tmp` and stale build
  output into the Python environment
- The full GNU GPL v3 text is included; the file was previously abridged
- Over HTTP, a slow tool call no longer holds up other sessions
- Listing or deleting active checks across all types is faster
- The active check, Event Console, auxiliary tag and service parameter tools
  describe themselves and answer in English throughout
- Listing process rules no longer executes the rule values CheckMK returns
- `wait_for_discovery` waits for the job to finish instead of failing with a
  redirect error, and `start_service_discovery` no longer falls back to bulk
  discovery or starts a second job while one is running
- `start_service_discovery` accepts the `tabula_rasa` and
  `only_service_labels` modes it offered, and asks for confirmation since it
  can remove services
- `get_pending_changes` shows what changed and by whom instead of "Unknown"
- Host status shows "Never" for a state that has not changed yet, instead of
  a time 56 years ago
- `recur` on downtimes takes effect (commercial editions); it was accepted but
  never sent, and offered a `month` value CheckMK does not have
- `create_smtp_check` applies the `port` it reports; it was never sent
- `activate_changes: true` takes effect on every write tool that offers it; on
  34 of them (rules, folders, groups, passwords, tags, time periods, roles,
  contact groups) it was ignored and the changes stayed pending

## [0.5.1] - 2026-10-02

### Fixed
- `activate_changes` works again on host, service parameter, active check and
  auxiliary tag writes; it raised an internal error instead of activating
- Custom graph and metric search results render again instead of raising an
  internal error
- Metric search works: it now takes the `graph_id` or `metric_id` CheckMK
  requires, instead of always answering HTTP 400
- A metric's latest value skips a still-empty final bucket instead of
  reporting "None"

### Changed
- Checkmk 2.5 is listed as supported; 2.2 and older are not

## [0.5.0] - 2026-10-02

### Added
- Host centrally: `--transport http` serves Streamable HTTP, so clients need
  neither Python nor a checkout. Requires a bearer token (`VIBEMK_HTTP_TOKEN`)
- Tools now say what they do before they run — read-only, writing or
  destructive — so a client knows what to confirm
- Notification rule management: list, show, create, update and delete
- Service params, agent bakery, active checks, audit log, aux tags, event
  console and site management
- Host status, host lists and pending changes also return machine-readable data

### Changed
- MCP is served through the official SDK: a current protocol revision is
  negotiated, and a failing tool now reports a failure the model can act on
- Requires Python 3.10 or newer, and one dependency (the MCP SDK)
- A server URL without `http://` or `https://` now assumes HTTPS
- Error messages include CheckMK's own explanation, not just the HTTP status
- Discovery offers all seven modes CheckMK documents

### Fixed
- Deleting one downtime no longer deletes others on the same host
- Acknowledgement lists show acknowledgements, not similar-looking comments
- Bulk discovery no longer removes services unless asked to
- A failed write is no longer retried, so it cannot take effect twice
- Concurrent edits are detected again: writes send the object's real ETag
- Acknowledgement tools are reachable (they were built but never offered)
- CheckMK 2.4 and 2.5 compatibility for monitoring data and host listings

### Removed
- Three tools whose CheckMK endpoints do not exist: `vibemk_reschedule_check`,
  `vibemk_discover_services` (use `vibemk_start_service_discovery`) and
  `vibemk_test_notification`

## [0.3.10] - 2025-08-23
### Added
- Enhanced Host Attribute Updates for ipaddress, alias and tags

## [0.3.9] - 2025-08-23
### Changed  
- Version increment

## [0.3.6] - 2025-08-23
### Fixed
- User roles management now works correctly across CheckMK 2.3 and 2.4

## [0.3.5] - 2025-08-23  
### Fixed
- Problem acknowledgements now work correctly across CheckMK 2.3 and 2.4
- Automatic version detection for better compatibility

## [0.3.3] - 2025-08-22
### Added
- Service group management - create, list, update, and delete service groups
- Bulk operations for managing multiple service groups at once

## [0.3.2] - 2025-08-22
### Added
- Advanced host management - validate configs, compare states, create cluster hosts
- Enhanced host updates with merge, overwrite, and remove modes

## [0.3.1] - 2025-08-21
### Added  
- Time period management - create and manage notification schedules
- Support for complex weekly schedules (e.g., Monday-Friday 8-17)

## [0.3.0] - 2025-08-21
### Added
- Complete downtime management - schedule, list, and delete maintenance windows
- Flexible duration parsing (e.g., "2h", "1h30m")

## [0.2.0] - 2025-08-21
### Added
- Performance metrics retrieval and analysis
- Live host and service status monitoring  
- Folder management for organizing hosts
- Rule creation and management

## [0.1.0] - 2025-08-20
### Added
- Initial CheckMK integration with MCP server
- Basic monitoring tools and authentication