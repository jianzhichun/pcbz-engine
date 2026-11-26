"""M2 proof for pcbcraft-render-server: a scripted host (stdlib only) that speaks the wire protocol
(PROTOCOL.md) on leg A, drives the editor with pointer, key and text input, and drives the same
session through the control channel with `pcbcraft-cli mcp --connect`.

Usage: python stream_proof.py --server BIN --cli BIN --project FILE --out DIR [--adapter software|auto]
                              [--size 960x600] [--mac] [--strace]

Two sessions: an editable one (frames, click on the Board workspace, key + text + IME + copy/paste, project.rename,
snapshot_project, edit.undo, the app bar's Undo of a model edit, a UI edit that the control channel sees as one revision
stream, New/Ctrl+N/Connect AI assistants refused) and a read-only one (policy at start: an edit through the control channel
is refused, UI edits leave the snapshot byte for byte, Ctrl+O asks nothing, Open ZIP asks the host). `--strace` (Linux):
the server must start no other program and connect to nothing beyond loopback. Writes frames/*.png, protocol.log (one JSON line per message), timings.json,
report.json, server-*.log, mcp-*.log into DIR. Exit 0 only when every required check passed.
"""
import argparse
import base64
import io
import json
import os
import pathlib
import queue
import secrets
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
import zlib

PNG_SIG = b"\x89PNG\r\n\x1a\n"


# --------------------------------------------------------------------------------------- PNG
def png_encode(w, h, rgba):
    rows = b"".join(b"\x00" + bytes(rgba[y * w * 4:(y + 1) * w * 4]) for y in range(h))

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    return PNG_SIG + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows, 3)) + chunk(b"IEND", b"")


def png_decode(data):
    """8-bit RGB/RGBA, no interlace -> (w, h, tight RGBA bytes)."""
    if data[:8] != PNG_SIG:
        raise ValueError("not a PNG")
    pos, idat, header = 8, [], None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.append(body)
    w, h, depth, color, _, _, interlace = header
    channels = {2: 3, 6: 4}.get(color)
    if depth != 8 or channels is None or interlace:
        raise ValueError(f"unsupported PNG ({depth}, {color}, {interlace})")
    raw = zlib.decompress(b"".join(idat))
    stride = w * channels
    out = bytearray(w * h * 4)
    prev = bytearray(stride)
    for y in range(h):
        base = y * (stride + 1)
        ft = raw[base]
        row = bytearray(raw[base + 1:base + 1 + stride])
        if ft == 1:
            for i in range(channels, stride):
                row[i] = (row[i] + row[i - channels]) & 255
        elif ft == 2:
            for i in range(stride):
                row[i] = (row[i] + prev[i]) & 255
        elif ft == 3:
            for i in range(stride):
                left = row[i - channels] if i >= channels else 0
                row[i] = (row[i] + ((left + prev[i]) >> 1)) & 255
        elif ft == 4:
            for i in range(stride):
                a = row[i - channels] if i >= channels else 0
                b = prev[i]
                c = prev[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                row[i] = (row[i] + pred) & 255
        if channels == 4:
            out[y * w * 4:(y + 1) * w * 4] = row
        else:
            for x in range(w):
                out[(y * w + x) * 4:(y * w + x) * 4 + 4] = bytes(row[x * 3:x * 3 + 3]) + b"\xff"
        prev = row
    return w, h, bytes(out)


# ----------------------------------------------------------------------------------- leg A
class Leg:
    """One loopback connection carrying envelopes (u32 length, u32 header length, header, payload)."""

    def __init__(self, sock, log):
        self.sock, self.buf, self.log, self.t0 = sock, bytearray(), log, time.perf_counter()

    def _note(self, direction, header, payload):
        entry = {"t": round((time.perf_counter() - self.t0) * 1000, 1), "dir": direction, "msg": header}
        if payload:
            entry["payload"] = len(payload)
        if header.get("k") == "input":
            entry["msg"] = dict(header, events=[e.get("e") for e in header.get("events", [])])
        self.log.write(json.dumps(entry, ensure_ascii=False) + "\n")

    def send(self, header, payload=b""):
        head = json.dumps(header).encode()
        body = struct.pack("<I", len(head)) + head + payload
        self.sock.sendall(struct.pack("<I", len(body)) + body)
        self._note("host->server", header, payload)

    def poll(self, timeout):
        """The next (header, payload), or None when nothing complete arrives within `timeout`. A
        closed connection raises EOFError."""
        deadline = time.perf_counter() + timeout
        while True:
            if len(self.buf) >= 4:
                n = struct.unpack("<I", self.buf[:4])[0]
                if len(self.buf) >= 4 + n:
                    env = bytes(self.buf[4:4 + n])
                    del self.buf[:4 + n]
                    hl = struct.unpack("<I", env[:4])[0]
                    header = json.loads(env[4:4 + hl])
                    payload = env[4 + hl:]
                    self._note("server->host", header, payload)
                    return header, payload
            left = deadline - time.perf_counter()
            if left <= 0:
                return None
            self.sock.settimeout(left)
            try:
                chunk = self.sock.recv(1 << 20)
            except socket.timeout:
                return None
            if not chunk:
                raise EOFError("server closed leg A")
            self.buf += chunk


class Display:
    """The page's picture: the last frame with every rectangle drawn over it. JPEG pieces cannot be
    decoded with the standard library, so they are painted magenta until the lossless refinement
    (PNG, the same rectangle) replaces them; `pending` says how many still wait."""

    MAGENTA = b"\xff\x00\xff\xff"

    def __init__(self):
        self.w = self.h = 0
        self.px = bytearray()
        self.pending = []
        self.frames = 0

    def apply(self, head, payload):
        w, h = head["w"], head["h"]
        if head["full"] or (w, h) != (self.w, self.h):
            self.w, self.h, self.px, self.pending = w, h, bytearray(w * h * 4), []
        for r in head["rects"]:
            blob = payload[r["offset"]:r["offset"] + r["length"]]
            if r["codec"] == "rgba":
                self._blit(r, blob, r["ew"])
                self._drop_pending(r)
            elif r["codec"] == "png":
                pw, ph, rgba = png_decode(blob)
                assert (pw, ph) == (r["w"], r["h"]), "png size differs from its rectangle"
                self._blit(r, rgba, pw)
                self._drop_pending(r)
            else:
                self._blit(r, self.MAGENTA * (r["w"] * r["h"]), r["w"])
                self.pending.append((r["x"], r["y"], r["w"], r["h"]))
        self.frames += 1

    def _blit(self, r, rgba, src_w):
        for row in range(r["h"]):
            d = ((r["y"] + row) * self.w + r["x"]) * 4
            self.px[d:d + r["w"] * 4] = rgba[row * src_w * 4:row * src_w * 4 + r["w"] * 4]

    def _drop_pending(self, r):
        self.pending = [p for p in self.pending
                        if not (r["x"] <= p[0] and r["y"] <= p[1] and p[0] + p[2] <= r["x"] + r["w"] and p[1] + p[3] <= r["y"] + r["h"])]

    def colours(self):
        return len(set(memoryview(bytes(self.px)).cast("I")))

    def differs_from(self, other):
        a, b = memoryview(bytes(self.px)).cast("I"), memoryview(bytes(other)).cast("I")
        return sum(1 for x, y in zip(a, b) if x != y) if len(a) == len(b) else len(a)

    def save(self, path):
        pathlib.Path(path).write_bytes(png_encode(self.w, self.h, self.px))


# ----------------------------------------------------------------------------------- MCP bridge
class Bridge:
    """`pcbcraft-cli mcp --connect PORT` on stdio. Calls run on a thread so the caller can keep
    answering leg A while the editor works (it needs a frame pass to serve each request)."""

    def __init__(self, cli, port, secret, log_path):
        env = dict(os.environ, PCBCRAFT_CONTROL_TOKEN=secret)
        self.log = open(log_path, "w", encoding="utf-8")
        self.p = subprocess.Popen([cli, "mcp", "--connect", str(port), "--wait", "120"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=self.log, env=env, text=True, encoding="utf-8")
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.n = 0

    def _read(self):
        for line in self.p.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def ask(self, method, params=None):
        self.n += 1
        msg = {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()
        line = self.lines.get(timeout=180)
        if line is None:
            raise RuntimeError("the bridge closed")
        self.log.write(f"> {method} {json.dumps(params)[:300]}\n< {line[:600]}\n")
        return json.loads(line)

    def start(self):
        self.ask("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "stream-proof", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def tools(self):
        return {t["name"] for t in self.ask("tools/list")["result"]["tools"]}

    def tool(self, name, args=None):
        """(is_error, parsed JSON or text)."""
        result = self.ask("tools/call", {"name": name, "arguments": args or {}})["result"]
        text = result["content"][0]["text"]
        try:
            return bool(result.get("isError")), json.loads(text)
        except ValueError:
            return bool(result.get("isError")), text

    def execute(self, command, params=None):
        return self.tool("execute", {"command": command, "params": params or {}, "inspect": False})

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(15)
        except Exception:
            self.p.kill()


# ------------------------------------------------------------------------- raw control channel
def control_once(port, secret, method, params, timeout=10.0):
    """One authenticated request on its own control connection (the token on the first JSON
    frame), read one reply line, close. The token never reaches a log."""
    sock = socket.create_connection(("127.0.0.1", port), timeout)
    try:
        sock.sendall((json.dumps({"id": 1, "method": method, "params": params, "token": secret}) + "\n").encode("utf-8"))
        buffered = b""
        deadline = time.perf_counter() + timeout
        while b"\n" not in buffered:
            left = deadline - time.perf_counter()
            if left <= 0:
                raise TimeoutError(f"no reply to {method} within {timeout} s")
            sock.settimeout(left)
            chunk = sock.recv(65536)
            if not chunk:
                break
            buffered += chunk
        line = buffered.split(b"\n", 1)[0]
        return json.loads(line.decode("utf-8")) if line else {}
    finally:
        sock.close()


def headless_name(cli, archive, scratch):
    """The project name in a snapshot, read the way a host would: reopen it headless."""
    path = pathlib.Path(scratch) / f"snap-{secrets.token_hex(3)}.pc"
    path.write_bytes(archive)
    p = subprocess.Popen([cli, "mcp", "--read-only", "--project", str(path)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8")
    ids = iter(range(1, 50))

    def ask(method, params=None):
        msg = {"jsonrpc": "2.0", "id": next(ids), "method": method}
        if params is not None:
            msg["params"] = params
        p.stdin.write(json.dumps(msg) + "\n")
        p.stdin.flush()
        return json.loads(p.stdout.readline())

    ask("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "name", "version": "1"}})
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    reply = ask("tools/call", {"name": "inspect_project", "arguments": {}})["result"]
    p.stdin.close()
    p.wait(20)
    path.unlink()
    return json.loads(reply["content"][0]["text"])


def find_key(value, key):
    """First value of `key` anywhere in nested JSON."""
    if isinstance(value, dict):
        if key in value:
            return value[key]
        for v in value.values():
            found = find_key(v, key)
            if found is not None:
                return found
    elif isinstance(value, list):
        for v in value:
            found = find_key(v, key)
            if found is not None:
                return found
    return None


# ------------------------------------------------------------------------------------ session
MODS0 = {"alt": False, "ctrl": False, "shift": False, "meta": False}
# Test-only: hold each deferred design check this long so a cancel can land mid-job (patch 0013
# reads PCBZ_RENDER_TEST_CHECK_MS; the product never sets it). The check is otherwise fast.
TEST_CHECK_MS = 8000


class Session:
    def __init__(self, args, out, name, read_only):
        self.args, self.out, self.name, self.read_only = args, pathlib.Path(out), name, read_only
        self.checks, self.timings, self.events = [], [], []
        self.seq = 0
        self.display = Display()
        self.revision = None
        self.revisions = []
        self.asks = []
        self.log = open(self.out / f"protocol-{name}.log", "w", encoding="utf-8")
        self.nframe = 0

    # ------------------------------------------------------------------ plumbing
    def check(self, name, ok, detail="", required=True):
        self.checks.append({"session": self.name, "check": name, "ok": bool(ok), "required": required, "detail": str(detail)[:400]})
        print(f"[{self.name}] {'ok  ' if ok else 'FAIL' if required else 'warn'} {name} {detail if not ok else ''}", flush=True)
        return bool(ok)

    def start(self):
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        token = secrets.token_hex(32)
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY")}
        env.update(PCBZ_RENDER_TOKEN=token, PCBZ_RENDER_LOG="info", PCBZ_RENDER_TEST_CHECK_MS=str(TEST_CHECK_MS))
        self.config_dir = pathlib.Path(tempfile.mkdtemp(prefix="pcbz-cfg-", dir=self.out)).resolve()
        import shutil
        self.project_file = self.out / f"project-{self.name}.pc"
        shutil.copyfile(self.args.project, self.project_file)
        trace = [] if not getattr(self.args, "strace", False) else ["strace", "-f", "-qq", "-e", "trace=execve,connect", "-o", str(self.out / f"strace-{self.name}.log")]
        cmd = trace + [self.args.server, "--stream", f"127.0.0.1:{listener.getsockname()[1]}", "--session", self.name, "--size", self.args.size,
               "--adapter", self.args.adapter, "--project", str(self.project_file), "--config-dir", str(self.config_dir)]
        self.errlog = open(self.out / f"server-{self.name}.log", "w", encoding="utf-8")
        t0 = time.perf_counter()
        self.proc = subprocess.Popen(cmd, env=env, stdout=self.errlog, stderr=subprocess.STDOUT)
        listener.settimeout(60)
        conn, _ = listener.accept()
        listener.close()
        self.leg = Leg(conn, self.log)
        head, _ = self.expect("hello", 60)
        self.hello = head
        self.check("hello names the protocol and echoes the token", head["v"] == 1 and head["token"] == token and head.get("pid"), head)
        # What the host sends after a good hello: policy, resize, visible.
        self.leg.send({"k": "policy", "readOnly": "licence expired" if self.read_only else None})
        w, h = (int(x) for x in self.args.size.split("x"))
        self.leg.send({"k": "resize", "w": w, "h": h, "dpr": 1.0})
        self.leg.send({"k": "visible", "visible": True})
        head, _ = self.expect("ready", 150)
        self.ready = head
        self.timings.append({"step": "start-to-ready", "ms": round((time.perf_counter() - t0) * 1000)})
        self.check("ready carries the control port and secret", head.get("controlPort", 0) > 0 and len(head.get("controlSecret", "")) >= 32, {k: v for k, v in head.items() if k != "controlSecret"})
        self.take_frames(1)
        return self

    def handle(self, head, payload):
        kind = head["k"]
        if kind == "frame":
            self.display.apply(head, payload)
            self.leg.send({"k": "ack", "seq": head["seq"], "decodeMs": 1.0})
            self.last_frame = (head, time.perf_counter())
        elif kind == "design_changed":
            self.revision = head["revision"]
            self.revisions.append(head["revision"])
        elif kind in ("ask_open", "ask_save", "ask_done", "notice", "copied", "paste_request"):
            self.asks.append(head)
        elif kind == "bye":
            self.events.append(head)

    def expect(self, kind, timeout):
        """Read until a message of `kind` (handling the others), within `timeout` seconds."""
        deadline = time.perf_counter() + timeout
        while True:
            left = deadline - time.perf_counter()
            if left <= 0:
                raise TimeoutError(f"no {kind} within {timeout} s")
            got = self.leg.poll(left)
            if got is None:
                continue
            self.handle(*got)
            if got[0]["k"] == kind:
                return got

    def pump(self, seconds):
        end = time.perf_counter() + seconds
        while time.perf_counter() < end:
            got = self.leg.poll(max(0.0, end - time.perf_counter()))
            if got:
                self.handle(*got)

    def pump_count(self, seconds):
        """Pump leg A for `seconds`, returning how many frames arrived."""
        end = time.perf_counter() + seconds
        frames = 0
        while time.perf_counter() < end:
            got = self.leg.poll(max(0.0, end - time.perf_counter()))
            if got:
                self.handle(*got)
                frames += int(got[0]["k"] == "frame")
        return frames

    def take_frames(self, n):
        for _ in range(n):
            self.expect("frame", 120)

    def settle(self, quiet=0.8, limit=60):
        """Frames until none arrives for `quiet` seconds, and no JPEG piece waits for its refinement."""
        end = time.perf_counter() + limit
        while time.perf_counter() < end:
            got = self.leg.poll(quiet)
            if got is None:
                if not self.display.pending:
                    return True
                continue
            self.handle(*got)
        return False

    def snap(self, label):
        self.settle()
        self.nframe += 1
        path = self.out / "frames" / f"{self.name}-{self.nframe:02d}-{label}.png"
        self.display.save(path)
        return bytes(self.display.px)

    # ------------------------------------------------------------------ input
    def send_input(self, events, label, wait_frame=True):
        self.seq += 1
        seq = self.seq
        t0 = time.perf_counter()
        self.leg.send({"k": "input", "seq": seq, "events": events})
        if not wait_frame:
            return None
        deadline = t0 + 20
        while time.perf_counter() < deadline:
            got = self.leg.poll(deadline - time.perf_counter())
            if not got:
                break
            self.handle(*got)
            if got[0]["k"] == "frame" and got[0]["inputSeq"] >= seq:
                t = got[0]["timing"]
                row = {"step": label, "ms": round((time.perf_counter() - t0) * 1000, 1), **{k: round(v, 1) for k, v in t.items()}}
                self.timings.append(row)
                return row
        return None

    def click(self, x, y, label, double=False):
        row = self.send_input([{"e": "move", "x": x, "y": y}], label + "-move", wait_frame=False)
        time.sleep(0.15)
        for i in range(2 if double else 1):
            self.send_input([{"e": "button", "x": x, "y": y, "button": "primary", "pressed": True, "mods": MODS0}], label + "-down", wait_frame=False)
            row = self.send_input([{"e": "button", "x": x, "y": y, "button": "primary", "pressed": False, "mods": MODS0}], label)
            if double and i == 0:
                time.sleep(0.08)
        return row

    def press(self, key, physical, label, mods=MODS0):
        self.send_input([{"e": "key", "key": key, "physical": physical, "pressed": True, "repeat": False, "mods": mods}], label + "-down", wait_frame=False)
        return self.send_input([{"e": "key", "key": key, "physical": physical, "pressed": False, "repeat": False, "mods": mods}], label)

    def type_text(self, text, label):
        return self.send_input([{"e": "text", "text": text}], label)

    # ------------------------------------------------------------------ control channel
    def bridge_call(self, fn):
        """Run `fn` (a bridge call) on a thread while leg A keeps being answered."""
        box = {}

        def run():
            try:
                box["v"] = fn()
            except Exception as e:  # reported by the caller
                box["e"] = e

        t = threading.Thread(target=run, daemon=True)
        t.start()
        while t.is_alive():
            self.pump(0.05)
        t.join()
        if "e" in box:
            raise box["e"]
        return box["v"]

    def open_bridge(self):
        self.bridge = Bridge(self.args.cli, self.ready["controlPort"], self.ready["controlSecret"], self.out / f"mcp-{self.name}.log")
        self.bridge_call(self.bridge.start)
        names = self.bridge_call(self.bridge.tools)
        self.check("the bridge lists snapshot_project, execute and inspect_editor", {"snapshot_project", "execute", "inspect_editor"} <= names, sorted(names))

    def snapshot_name(self):
        err, snap = self.bridge_call(lambda: self.bridge.tool("snapshot_project"))
        if err:
            return None, snap
        data = base64.b64decode(snap["archive_base64"], validate=True)
        assert data[:2] == b"PK" and len(data) == snap["bytes"], "the snapshot is not the zip it says"
        info = headless_name(self.args.cli, data, self.out)
        return info.get("project", {}).get("name"), snap

    def workspaces(self):
        err, editor = self.bridge_call(lambda: self.bridge.tool("inspect_editor"))
        views = editor.get("views", []) if isinstance(editor, dict) else []
        return [v.get("workspace") for v in views], editor

    def stop(self):
        try:
            self.leg.send({"k": "stop", "why": "proof done"})
            self.expect("bye", 20)
        except Exception as e:
            self.check("bye after stop", False, e, required=False)
        try:
            self.proc.wait(20)
        except Exception:
            self.proc.kill()
        self.check("the server exits 0 after stop", self.proc.returncode == 0, self.proc.returncode)
        self.errlog.close()
        self.log.close()
        if getattr(self, "bridge", None):
            self.bridge.close()


# ------------------------------------------------------------------------------------- flows
def sha256(path):
    import hashlib
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def cancel_running_check(s, label):
    """Patch 0013: stop a real control-channel job while the editor draws, then prove nothing moved.

    The job is `pcb.check_design`, deferred to its own worker by the editor; PCBZ_RENDER_TEST_CHECK_MS
    holds it long enough for the cancel to arrive. The bridge's single connection carries the call,
    so the cancel uses a separate raw control connection, as the plugin's stop does."""
    err0, snap0 = s.bridge_call(lambda: s.bridge.tool("snapshot_project"))
    rev0 = s.revision
    pid = s.proc.pid
    box = {}

    def run():
        try:
            box["r"] = s.bridge.execute("pcb.check_design", {})
        except Exception as e:  # reported by the checks below
            box["e"] = e

    t0 = time.perf_counter()
    t = threading.Thread(target=run, daemon=True)
    t.start()
    # Keep the interface busy the way a person would while the model's step computes: move the
    # pointer, and count the frames the server still draws and encodes.
    frames = 0
    for i in range(10):
        s.leg.send({"k": "input", "seq": s.seq + 1, "events": [{"e": "move", "x": 300 + i * 40, "y": 380 + i}]})
        s.seq += 1
        frames += s.pump_count(0.1)
    reply = control_once(s.ready["controlPort"], s.ready["controlSecret"], "ui.operations", {"cancel": "agent"})
    cancelled = reply.get("ok") is True and (reply.get("result") or {}).get("cancelled") == "agent"
    s.check(f"[{label}] ui.operations cancel is answered by the control socket, not a frame pass", cancelled, reply)
    t.join(timeout=30)
    elapsed = time.perf_counter() - t0
    result = box.get("r")
    text = json.dumps(result, ensure_ascii=False) if result is not None else str(box.get("e"))
    stopped = bool(result) and result[0] is True and "stopped:" in text and "nothing was applied" in text
    s.check(f"[{label}] the in-flight engine.execute answers stopped / nothing was applied", stopped, text[:300])
    s.check(f"[{label}] the stop returned before the scripted hold ended", elapsed < TEST_CHECK_MS / 1000.0, round(elapsed, 2))
    s.check(f"[{label}] the editor drew frames while the job computed", frames >= 3, frames)
    s.check(f"[{label}] the editor never restarted", s.proc.poll() is None and s.proc.pid == pid, (s.proc.poll(), pid))
    s.settle()
    err1, snap1 = s.bridge_call(lambda: s.bridge.tool("snapshot_project"))
    same = not err0 and not err1 and snap0.get("archive_base64") == snap1.get("archive_base64")
    s.check(f"[{label}] the project snapshot is byte for byte unchanged", same, (err0, err1))
    s.check(f"[{label}] the revision did not move", s.revision == rev0, (rev0, s.revision))
    # The same process still serves the control channel afterwards.
    try:
        err, info = s.bridge_call(lambda: s.bridge.tool("inspect_project"))
    except Exception as e:
        err, info = True, e
    s.check(f"[{label}] the control channel still answers after the stop", not err, str(info)[:200])
    return reply


def editable_session(args, out):
    s = Session(args, out, "edit", False).start()
    cmd = {"alt": False, "ctrl": not args.mac, "shift": False, "meta": bool(args.mac)}
    project_hash = sha256(s.project_file)
    try:
        before = s.snap("before")
        s.check("first frame is the whole pane", s.display.w > 0 and (s.display.w, s.display.h) == tuple(int(x) for x in args.size.split("x")), (s.display.w, s.display.h))
        s.check("first frame is not flat", s.display.colours() >= 500, s.display.colours())
        s.check("no JPEG piece is left unrefined", not s.display.pending, s.display.pending[:3])
        s.check("design_changed announced the opened project", s.revision is not None, s.revision)
        s.open_bridge()
        original, _ = s.snapshot_name()
        s.check("snapshot_project works against the live editor", bool(original), original)
        ws, _ = s.workspaces()
        s.check("the editor starts in the schematic workspace", "schematic" in ws, ws)

        # --- the product name and the File menu; Save must not write the file the project came from
        s.click(124, 16, "file-menu")
        s.snap("file-menu")
        s.press("Escape", "Escape", "escape-1")
        s.click(156, 16, "save-icon")
        s.settle()
        s.check("a plain Save writes nothing (the host keeps the file)", sha256(s.project_file) == project_hash, "project file changed")

        # --- key, click and text: the net label dialog (key L), its Net name field, a replacement name, Place
        rev_ui = s.revision
        s.press("l", "KeyL", "key-l")
        s.snap("label-dialog")
        s.click(400, 264, "label-name-field")
        s.press("a", "KeyA", "select-all", cmd)
        row = s.type_text("NETZ1", "label-text")
        s.check("typing into the Net name field redraws the pane", row is not None, row)
        # IME: a composition, then its commit in Chinese (patch 0009 draws the glyphs)
        s.send_input([{"e": "preedit", "text": "wang", "range": [4, 4]}], "ime-preedit")
        s.send_input([{"e": "commit", "text": "网络"}], "ime-commit")
        # copy: the field's text goes to the host (`copied`); paste: the host's text lands in the field
        mark = len(s.asks)
        s.press("a", "KeyA", "select-all-2", cmd)
        s.send_input([{"e": "copy"}], "copy")
        s.pump(1.0)
        copied = [a for a in s.asks[mark:] if a["k"] == "copied"]
        s.check("copy in the field hands the text to the host (copied)", bool(copied) and copied[-1]["text"] == "NETZ1网络", s.asks[mark:])
        s.press("End", "End", "end")
        s.send_input([{"e": "paste", "text": "_P"}], "paste")
        s.snap("label-typed")
        s.check("what went out as JPEG is refined losslessly while a caret blinks", not s.display.pending, s.display.pending[:3])
        s.click(811, 378, "label-place")
        s.snap("label-after-place")
        s.click(330, 300, "label-click-sheet")
        s.snap("label-click")
        s.check("the UI edit moved the revision (design_changed)", s.revision is not None and rev_ui is not None and s.revision > rev_ui, (rev_ui, s.revision))
        s.press("Escape", "Escape", "escape-2")
        err, info = s.bridge_call(lambda: s.bridge.tool("inspect_project"))
        s.check("the placed label has the typed, IME-committed and pasted name", not err and "NETZ1网络_P" in json.dumps(info, ensure_ascii=False), str(info)[:300])

        # --- a click on the Board workspace: the workspace picker, then PCB Layout
        row = s.click(53, 135, "workspace-picker")
        s.snap("workspace-picker")
        row = s.click(190, 300, "board-open")
        s.check("the click redraws the pane", row is not None, row)
        after_click = s.snap("board")
        ws, _ = s.workspaces()
        s.check("a click on PCB Layout opens the Board workspace", "board" in ws, ws)
        s.check("the frame changed after the click", s.display.differs_from(before) > 2000, s.display.differs_from(before))

        # --- Save As: the encoding chooser. "Project folder..." names a new folder: cancelled with a notice.
        # "Packed project (.pc)..." is a file dialog: ask_save goes to the host, which answers with a place of its own.
        mark = len(s.asks)
        s.press("s", "KeyS", "save-as", dict(cmd, shift=True))
        s.snap("save-as-dialog")
        s.click(360, 289, "save-as-folder")
        s.settle(quiet=1.2)
        notices = [a for a in s.asks[mark:] if a["k"] == "notice"]
        s.check("a folder save is cancelled with a notice", bool(notices) and not any(a["k"] == "ask_save" for a in s.asks[mark:]), s.asks[mark:])
        s.press("s", "KeyS", "save-as-2", dict(cmd, shift=True))
        s.click(378, 320, "save-as-pc")
        s.settle(quiet=1.5)
        saves = [a for a in s.asks[mark:] if a["k"] == "ask_save"]
        s.check("Save As > Packed project asks the host where (ask_save)", bool(saves), s.asks[mark:])
        if saves:
            s.check("ask_save names a .pc file and its type", saves[0]["name"].endswith(".pc") and saves[0]["exts"] == ["pc"], saves[0])
            staging = out / "staging"
            staging.mkdir(exist_ok=True)
            target = staging / "exported.pc"
            s.leg.send({"k": "answer", "ask": saves[0]["ask"], "path": str(target.resolve())})
            s.settle(quiet=2.0)
            s.check("the editor wrote the archive where the host said", target.exists() and target.read_bytes()[:2] == b"PK", target.exists())
            s.check("ask_done followed the save", any(a["k"] == "ask_done" and a["ask"] == saves[0]["ask"] and a["ok"] for a in s.asks[mark:]), s.asks[mark:][-3:])
            exported = headless_name(args.cli, target.read_bytes(), out) if target.exists() else {}
            s.check("the exported archive is the edited project", bool(exported.get("project", {}).get("name")), exported.get("project", {}).get("name"))
        s.snap("after-save-as")
        s.press("Escape", "Escape", "escape-3")

        # --- control channel: rename (with Chinese text, for the font), snapshot, undo, on the revision stream the pane shares
        s.settle()
        rev0 = s.revision
        new_name = "Renamed 中文网络 over MCP"
        err, reply = s.bridge_call(lambda: s.bridge.execute("project.rename", {"name": new_name}))
        s.check("execute project.rename through the bridge", not err, reply)
        s.settle()
        s.check("design_changed announced the rename with a higher revision", s.revision is not None and rev0 is not None and s.revision > rev0, (rev0, s.revision))
        renamed, _ = s.snapshot_name()
        s.check("snapshot_project returns an archive whose project name changed", renamed == new_name, (original, renamed))
        s.snap("renamed")
        s.check("the title in the pane was redrawn", s.display.differs_from(after_click) > 100, s.display.differs_from(after_click))
        rev1 = s.revision
        err, reply = s.bridge_call(lambda: s.bridge.execute("edit.undo"))
        s.check("execute edit.undo through the bridge", not err, reply)
        s.settle()
        undone, _ = s.snapshot_name()
        s.check("edit.undo gave the old name back", undone == original, (original, undone))
        s.check("undo moved the revision too", s.revision is not None and rev1 is not None and s.revision != rev1, (rev1, s.revision))
        s.snap("undone")

        # --- one history: the model's edit, undone from the pane (the app bar's Undo)
        err, reply = s.bridge_call(lambda: s.bridge.execute("project.rename", {"name": "Model edit 模型"}))
        s.settle()
        rev2 = s.revision
        s.click(182, 16, "undo-icon")
        s.settle()
        name, _ = s.snapshot_name()
        s.check("the app bar's Undo undid the model's edit", not err and name == original and s.revision != rev2, (original, name, rev2, s.revision))

        # --- what the host owns (patch 0010): New Project, Ctrl+N, Help > Connect AI assistants
        s.click(124, 16, "file-menu-2")
        s.settle()
        s.click(177, 52, "file-new-project")
        s.settle()
        s.press("n", "KeyN", "ctrl-n", cmd)
        s.settle()
        name, _ = s.snapshot_name()
        s.check("File > New Project and Ctrl+N leave the host's project in place", name == original, (original, name))
        s.snap("new-refused")
        s.press("Escape", "Escape", "escape-4")
        s.click(940, 16, "help-menu")
        s.snap("help-menu")
        s.click(817, 151, "connect-ai")
        s.settle()
        s.snap("connect-refused")
        s.click(498, 325, "disable-connection")
        s.settle()
        try:
            err, info = s.bridge_call(lambda: s.bridge.tool("inspect_project"))
        except Exception as e:
            err, info = True, e
        s.check("the control channel survives Help > Connect AI assistants", not err, str(info)[:200])
        # --- patch 0013: stop the model's step from the control channel, nothing applied
        cancel_running_check(s, "edit")
        s.check("the editor wrote nothing to the project file", sha256(s.project_file) == project_hash, "project file changed")
    finally:
        s.stop()
    return s


def read_only_session(args, out):
    s = Session(args, out, "readonly", True).start()
    cmd = {"alt": False, "ctrl": not args.mac, "shift": False, "meta": bool(args.mac)}
    try:
        s.snap("readonly-before")
        s.open_bridge()
        original, _ = s.snapshot_name()
        s.check("snapshot_project still works read-only", bool(original), original)
        rev0 = s.revision
        err, reply = s.bridge_call(lambda: s.bridge.execute("project.rename", {"name": "Must not apply"}))
        s.check("a read-only editor refuses project.rename", err and "read-only" in json.dumps(reply), reply)
        s.settle()
        name, _ = s.snapshot_name()
        s.check("the project name did not change", name == original, (original, name))
        s.check("the revision did not move", s.revision == rev0, (rev0, s.revision))
        err, reply = s.bridge_call(lambda: s.bridge.execute("edit.undo"))
        s.check("a read-only editor refuses edit.undo", err, reply)
        # UI edits are refused too: a net label placed on the sheet, select all + Delete, the app bar's Undo.
        # The snapshot stays byte for byte.
        err, snap0 = s.bridge_call(lambda: s.bridge.tool("snapshot_project"))
        s.press("l", "KeyL", "key-l")
        s.click(811, 378, "label-place")
        s.click(330, 300, "label-click-sheet")
        s.press("Escape", "Escape", "escape-label")
        s.click(480, 470, "sheet")
        s.press("a", "KeyA", "select-all", cmd)
        s.press("Delete", "Delete", "delete")
        s.click(182, 16, "undo-icon")
        s.settle()
        s.check("UI edits do not move the revision either", s.revision == rev0, (rev0, s.revision))
        err1, snap1 = s.bridge_call(lambda: s.bridge.tool("snapshot_project"))
        same = not err and not err1 and snap0.get("archive_base64") == snap1.get("archive_base64")
        s.check("the snapshot is byte for byte the same after the UI edits", same, (err, err1))
        s.snap("readonly-after")
        # File > Open (Ctrl+O) would replace the host's project: refused, no dialog (patch 0010)
        asks_before = len(s.asks)
        s.press("Escape", "Escape", "escape-select")
        s.press("o", "KeyO", "open", cmd)
        s.settle()
        s.click(380, 289, "open-project-file")
        s.settle()
        asked = [a for a in s.asks[asks_before:] if a["k"] == "ask_open"]
        for a in asked:
            s.leg.send({"k": "answer", "ask": a["ask"], "path": None})
        s.settle()
        name, _ = s.snapshot_name()
        s.check("Ctrl+O asks nothing and leaves the host's project", name == original and not asked, (original, name, asked))
        # a file the editor reads is asked of the host: Manufacturing > Open ZIP (review is reading, so read-only allows it)
        s.click(53, 135, "workspace-picker")
        s.snap("readonly-workspace-picker")
        s.click(190, 300, "board-open")
        s.settle()
        s.click(321, 57, "manufacture-tab")
        s.settle()
        s.click(230, 105, "manufacturing")
        s.snap("readonly-manufacturing")
        for answer in (None, str(pathlib.Path(args.project2).resolve())):
            asks_before = len(s.asks)
            s.click(152, 105, "open-zip")
            s.settle(quiet=1.5)
            opens = [a for a in s.asks[asks_before:] if a["k"] == "ask_open"]
            s.check("Open ZIP asks the host for a file (ask_open, zip)", bool(opens) and opens[0].get("exts") == ["zip"], s.asks[asks_before:])
            if opens:
                s.leg.send({"k": "answer", "ask": opens[0]["ask"], "path": answer})
                s.settle(quiet=1.5)
                done = [a for a in s.asks[asks_before:] if a["k"] == "ask_done" and a["ask"] == opens[0]["ask"]]
                s.check(f"ask_done follows ({'cancel' if answer is None else 'a file'}): ok {answer is not None}", bool(done) and done[0]["ok"] is (answer is not None), s.asks[asks_before:])
        s.snap("readonly-open-zip")
        # --- patch 0013 read-only: nothing is computing, so the cancel is a clean refusal; a running
        # read-only check is still stoppable and leaves the same bytes.
        idle = control_once(s.ready["controlPort"], s.ready["controlSecret"], "ui.operations", {"cancel": "agent"})
        s.check("a cancel with nothing computing is refused cleanly (read-only)",
                idle.get("ok") is False and "no operation" in json.dumps(idle), idle)
        try:
            err, info = s.bridge_call(lambda: s.bridge.tool("inspect_project"))
        except Exception as e:
            err, info = True, e
        s.check("the read-only editor still answers after the refusal", not err, str(info)[:200])
        cancel_running_check(s, "readonly")
    finally:
        s.stop()
    return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True)
    ap.add_argument("--cli", required=True)
    ap.add_argument("--project", required=True)
    ap.add_argument("--project2", required=True, help="another project, offered when the editor asks for a file")
    ap.add_argument("--out", required=True)
    ap.add_argument("--adapter", default="auto")
    ap.add_argument("--size", default="960x600")
    ap.add_argument("--mac", action="store_true", help="the command key is Meta (macOS)")
    ap.add_argument("--strace", action="store_true", help="Linux: run the server under strace; it must start no other program")
    args = ap.parse_args()
    out = pathlib.Path(args.out)
    (out / "frames").mkdir(parents=True, exist_ok=True)
    sessions, failed = [], None
    for flow in (editable_session, read_only_session):
        try:
            sessions.append(flow(args, out))
        except Exception as e:  # a crash in one flow must not hide the other's artifacts
            import traceback
            traceback.print_exc()
            failed = f"{flow.__name__}: {e!r}"
            break
    if args.strace:
        for s in sessions:
            log = (out / f"strace-{s.name}.log").read_text(encoding="utf-8", errors="replace").splitlines()
            started = [line for line in log if "execve(" in line and "= 0" in line]
            inet = [line for line in log if "connect(" in line and "AF_INET" in line and "127.0.0.1" not in line]
            s.check("the server started no other program (strace execve)", len(started) == 1, started[:4])
            s.check("the server connected to nothing beyond loopback (strace connect)", not inet, inet[:4])
    checks = [c for s in sessions for c in s.checks]
    timings = [dict(session=s.name, **t) for s in sessions for t in s.timings]
    steps = [t for t in timings if t["step"] not in ("start-to-ready",) and "ms" in t]
    summary = {
        "size": args.size,
        "adapter": sessions[0].hello.get("adapter") if sessions else None,
        "backend": sessions[0].hello.get("backend") if sessions else None,
        "software": sessions[0].hello.get("software") if sessions else None,
        "frame_ms_median": round(statistics.median(t["ms"] for t in steps), 1) if steps else None,
        "frame_ms_max": max((t["ms"] for t in steps), default=None),
        "steps": timings,
    }
    (out / "timings.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    report = {"failed": failed, "checks": checks}
    (out / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    bad = [c for c in checks if c["required"] and not c["ok"]]
    print(f"{len(checks) - len(bad)}/{len(checks)} checks ok; frame latency median {summary['frame_ms_median']} ms, max {summary['frame_ms_max']} ms")
    if failed or bad:
        print("FAILED:", failed or [c["check"] for c in bad])
        sys.exit(1)


if __name__ == "__main__":
    main()
