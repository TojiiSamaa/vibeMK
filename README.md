# vibeMK 🚀

## Python vs Rust: measured comparison

Same machine (Windows 11), same fake CheckMK, both servers over stdio, release build for Rust.
Script: `scripts/bench.py`. Median of 10 starts, 300 sequential `tools/call`.

| | Python (0.6.4) | Rust (`rust/`) | Rust vs Python |
|---|---|---|---|
| Start until `initialize` answered | 1217 ms | 259 ms | 4.7x faster |
| `tools/list` (154 tools) | 8.9 ms | 4.7 ms | 1.9x faster |
| `tools/call`, median | 4.86 ms | 1.05 ms | 4.6x faster |
| `tools/call`, p99 | 75 ms | 44 ms | 1.7x faster |
| Sequential throughput | 53 calls/s | 251 calls/s | 4.7x |
| Peak memory (process tree) | 75 MB | 54 MB | 28 % less |
| Install footprint | 75.5 MB of dependencies (31 packages) + a Python interpreter | one 11.1 MB binary | |
| Source lines (src) | 16,941 | 14,657 | |
| Tests | 464 pytest cases, 6,496 lines | 537 tests, 12,131 lines | |
| Tools offered | 154 | 154 (identical `tools.json`) | |

Limits: a local fake server answers in microseconds, so these numbers show the server's own overhead, not
what you will see against a real CheckMK, where the network and CheckMK dominate. One machine, one run.
The Rust port is tested against the same fake CheckMK, never against a real instance. See `AUDIT.md`.

**CheckMK Monitoring via LLM - Professional MCP Server**

<!-- mcp-name: io.github.chexma/vibemk -->

[![Python 3.10+](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![CheckMK 2.3+](https://img.shields.io/badge/CheckMK-2.3+-green.svg)](https://checkmk.com/)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![MCP Compatible](https://img.shields.io/badge/MCP-Compatible-purple.svg)](https://spec.modelcontextprotocol.io/)
[![MCP SDK](https://img.shields.io/badge/MCP%20SDK-official-purple.svg)](https://github.com/modelcontextprotocol/python-sdk)
[![Code style: black](https://img.shields.io/badge/code%20style-black-000000.svg)](https://github.com/psf/black)
[![Typed](https://img.shields.io/badge/typed-mypy-blue.svg)](https://mypy-lang.org/)

## 🎯 Overview

vibeMK enables complete management of your CheckMK monitoring environment directly through LLM interfaces using natural language.
This project is in the alpha stage and under development. I accept no liability for any damage resulting from the use of this software.

## vibeMK - Current Features

### Live Monitoring ✅
- **Host Status**: Real-time host state (UP/DOWN/UNREACHABLE) with hard state detection
- **Service Status**: Live service monitoring (OK/WARNING/CRITICAL/UNKNOWN)
- **Performance Metrics**: Retrieve metrics data with automatic discovery
- **Current Problems**: Auto-detect all active monitoring issues

### Downtime Management ✅
- **Schedule Downtimes**: Create host/service downtimes with flexible duration parsing ("2h", "1h30m")
- **List & Filter**: View all downtimes or filter for active ones only

### Problem Management ✅
- **Acknowledge Problems**: Set acknowledgements for host/service issues (sticky/persistent options)
- **List Acknowledgements**: View all current problem acknowledgements
- **Remove Acknowledgements**: Delete by pattern or individual removal

### Configuration Management ✅
- **Folders**: Create/delete monitoring folder structures
- **Rules**: Create rules for 2000+ CheckMK ruleset types with proper format handling
- **Time Periods**: Create custom notification schedules (business hours, 24/7, etc.)
- **Host Groups**: Organize hosts into logical groups

### User & Security ✅
- **User Accounts**: Create/manage user accounts with role assignment
- **Password Management**: Set passwords with policy enforcement
- **Contact Groups**: Manage notification groups
- **Host/Service Tags**: Comprehensive tagging system

## 🚀 Quick Start

```bash
1. pipx install vibemk   (or: uv tool install vibemk, or pip install vibemk into a virtual environment)
2. Edit the configuration file of your LLM Client, e.g. Claude Desktop - claude_desktop_config.json (See examples)
3. Start your LLM Client
4. CheckMK automation user setup (Administrator permissions or a customized role if changes are to be made, read-only if only analyses are to be performed.)
5. voila - configure checkmk using natural language
```
**Complete Installation Guide**: See [INSTALL.md](https://github.com/chexma/vibeMK/blob/main/INSTALL.md) for detailed step-by-step instructions.
**Upgrading from 0.3.x or older**: `git pull` is not enough, see [Upgrading](https://github.com/chexma/vibeMK/blob/main/INSTALL.md#%EF%B8%8F-upgrading).
**More Examples**: See `examples/llm_configs/` and [INSTALL.md](https://github.com/chexma/vibeMK/blob/main/INSTALL.md)  
**Visual Examples**: See `examples/Screenshots/` for example prompts and usage patterns  

## 🔌 Running it

**Locally (default).** The LLM client starts vibeMK itself over stdio. Nothing
to host, nothing to secure -- the client already owns the process.

**Centrally.** One instance can serve many clients over Streamable HTTP, so
they need neither Python nor vibeMK installed:

```bash
export VIBEMK_HTTP_TOKEN="$(python -c 'import secrets; print(secrets.token_urlsafe(32))')"
vibemk --transport http --port 8765
```

Clients connect to `http://<host>:8765/mcp` and send `Authorization: Bearer <token>`.
The token is mandatory, and vibeMK binds to localhost unless told otherwise:
the CheckMK account lives on the server, so whoever reaches the port inherits
it. See [INSTALL.md](https://github.com/chexma/vibeMK/blob/main/INSTALL.md) for what that means before you expose it.

## 💡 Practical Prompt Examples

```bash
# Add new server
"Create a new host 'web-server-05' in folder 'Servers' with IP 192.168.1.105 and discover all services"

# Schedule maintenance
"Schedule a 2-hour downtime for the service Check_MK on 'cephnode01' starting at 22:00 tomorrow for 'Debian Updates'"

# Downtimes 
"show me all current scheduled downtimes."

# Metric analysis
"Compare the “response_time” metric of the “HTTPS Webservice” service of the two hosts www.google.de and www.heise.de for the last ten minutes."

# Ruleset analysis
"analyze and compare the rulesets "Filesystems (used space and growth)" and see, if there are duplicates or if rules can be combined."
```
**Advanced Usage**: See `examples/ExamplePrompts.md` for complex scenarios and tips

## 📚 Checkmk version compatibility

| CheckMK Version | Compatibility | Features |
|-----------------|---------------|----------|
| **2.5.x** | ✅ Full     | All features available, tested against 2.5.0p14 (Raw and Ultimate) |
| **2.4.x** | ✅ Full     | All features available |
| **2.3.x** | ✅ Full     | All features available |
| **2.2.x and older** | 🔴 Unsupported | |

## Checkmk Edition Support

- **Raw Edition**: Fully supported, including the BI and Event Console endpoints
- **Enterprise Edition**: Adds the Agent Bakery and custom graphs
- **Cloud Edition**: All Enterprise features

Edition availability follows the `operationId` of each endpoint in the Checkmk
OpenAPI document: `cmk.gui.cee.*` is Enterprise-only, everything else is served
by every edition including Raw.

## Security considerations

- To let a model look without touching, start vibeMK read-only: `--read-only`
  or `VIBEMK_READ_ONLY=1`. Only the tools that read are offered, and a call to
  any other is refused before it reaches CheckMK
- Be aware of the potential security risks when you unleash AI on your checkmk
- Use at your own risk
- I accept no responsibility for actions performed by an AI

# 📄 License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

---

**Happy Monitoring with CheckMK and LLMs!** 🎉