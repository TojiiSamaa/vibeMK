"""Dump the advertised MCP tool list (name, schemas, annotations) to JSON.

The Rust port embeds this file, so tool schemas stay identical to the Python
reference. Re-run after any change to src/vibemk/server/tools.py:
    python scripts/export_tools.py rust/tools.json
"""
import json
import sys

from vibemk.server.tools import get_all_tools
from vibemk.server.annotations import annotations_for  # noqa: F401  (ensures import side effects)

out = sys.argv[1] if len(sys.argv) > 1 else "rust/tools.json"
tools = get_all_tools()
with open(out, "w", encoding="utf-8", newline="\n") as f:
    json.dump(tools, f, ensure_ascii=False, indent=1)
    f.write("\n")
print(f"{len(tools)} tools -> {out}")
