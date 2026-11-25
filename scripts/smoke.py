"""Smoke-test a built pcbcraft-cli: MCP handshake, the patched snapshot tool, and reopening
the snapshot it returns. Usage: python smoke.py <pcbcraft-cli> <scratch dir>. Stdlib only."""
import base64
import json
import pathlib
import subprocess
import sys


def session(cli, *args):
    proc = subprocess.Popen([cli, "mcp", *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8")
    ids = iter(range(1, 1000))

    def ask(method, params=None):
        msg = {"jsonrpc": "2.0", "id": next(ids), "method": method}
        if params is not None:
            msg["params"] = params
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
        reply = json.loads(proc.stdout.readline())
        if "error" in reply:
            raise SystemExit(f"{method}: {reply['error']}")
        return reply["result"]

    def tool(name, arguments=None):
        result = ask("tools/call", {"name": name, "arguments": arguments or {}})
        if result.get("isError"):
            raise SystemExit(f"{name}: {result['content'][0]['text']}")
        return json.loads(result["content"][0]["text"])

    ask("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}})
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    return proc, ask, tool


def main():
    cli, scratch = sys.argv[1], pathlib.Path(sys.argv[2])
    scratch.mkdir(parents=True, exist_ok=True)
    proc, ask, tool = session(cli)
    names = {t["name"] for t in ask("tools/list")["tools"]}
    missing = {"execute", "list_commands", "inspect_project", "snapshot_project"} - names
    if missing:
        raise SystemExit(f"tools missing: {sorted(missing)}")
    tool("execute", {"command": "project.new", "params": {"name": "Smoke", "create_board": True}, "inspect": False})
    snap = tool("snapshot_project")
    data = base64.b64decode(snap["archive_base64"], validate=True)
    if len(data) != snap["bytes"] or data[:2] != b"PK":
        raise SystemExit("snapshot is not the zip it says it is")
    path = scratch / "smoke.pc"
    path.write_bytes(data)
    proc.stdin.close()
    proc.wait(timeout=30)

    proc, ask, tool = session(cli, "--project", str(path))
    project = tool("inspect_project")
    text = json.dumps(project)
    if "Smoke" not in text:
        raise SystemExit("reopened snapshot lost the project name")
    again = tool("snapshot_project")
    proc.stdin.close()
    proc.wait(timeout=30)
    print(f"ok: {len(names)} tools, snapshot {snap['bytes']} bytes at revision {snap['revision']}, reopened at revision {again['revision']}")


if __name__ == "__main__":
    main()
