"""Open a saved project in the pinned pcbcraft-cli and report it.

Usage: python3 open_check.py <pcbcraft-cli> <project.pc> [expected-name]

Runs the MCP handshake and `inspect_project` over stdio, so the check is what the plugin's own
headless session would do with the file. Exits non-zero when the archive does not open or its
project name is not the expected one.
"""

import json
import subprocess
import sys


def main():
    cli, archive = sys.argv[1], sys.argv[2]
    expected = sys.argv[3] if len(sys.argv) > 3 else None
    proc = subprocess.Popen(
        [cli, "mcp", "--project", archive],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    ids = iter(range(1, 100))

    def ask(method, params=None):
        message = {"jsonrpc": "2.0", "id": next(ids), "method": method}
        if params is not None:
            message["params"] = params
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()
        reply = json.loads(proc.stdout.readline())
        if "error" in reply:
            raise SystemExit(f"{method}: {reply['error']}")
        return reply["result"]

    ask("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "wasm-page-open-check", "version": "1"}})
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    proc.stdin.flush()
    result = ask("tools/call", {"name": "inspect_project", "arguments": {}})
    if result.get("isError"):
        raise SystemExit("inspect_project: " + result["content"][0]["text"])
    text = result["content"][0]["text"]
    proc.stdin.close()
    proc.wait(timeout=30)

    report = json.loads(text)
    name = report.get("project", {}).get("name")
    if expected is not None and name != expected:
        raise SystemExit(f"opened project name {name!r}, expected {expected!r}")
    print(json.dumps({"ok": True, "name": name, "revision": report.get("revision")}, ensure_ascii=False))


if __name__ == "__main__":
    main()