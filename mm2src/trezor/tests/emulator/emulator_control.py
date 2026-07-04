#!/usr/bin/env python3
"""Controller helper for the Trezor emulator (trezor-user-env).

This is a *test utility only* — it is committed alongside the `trezor` crate's
integration tests and is never built into the shipped crate. It has two jobs:

  1. Start + seed the emulator (via the trezor-user-env controller WebSocket).
  2. Auto-approve on-device confirmations while the Rust test performs the actual
     wire-protocol (UDP) exchange with the emulator.

Two channels are used, and here is *why*:

Controller WebSocket protocol (trezor-user-env, default ws://localhost:9001)
---------------------------------------------------------------------------
Used only for `setup` (start/wipe/seed), which the emulator wire protocol cannot
do itself.
* On connect the controller sends an unsolicited greeting
  ({"type": "client", ...}) which carries no "success" field and is skipped.
* Every request is a JSON object with a "type" field plus command params, e.g.
      {"type": "emulator-start", "version": "2-main", "model": "T2T1", "wipe": true}
* Every command reply is a JSON object with a boolean "success" field, e.g.
      {"response": "pong", "id": "unknown", "success": true}
* Commands used: emulator-start {version, model, wipe},
  emulator-setup {mnemonic, pin, passphrase_protection, label}, background-check.

DebugLink over UDP (default 127.0.0.1:21325)
--------------------------------------------
Used for `press-yes` and `confirm-loop`. The controller's own `emulator-press-yes`
opens a *fresh* debug connection per call and never enables `DebugLinkWatchLayout`.
Empirically, T2T1 (Trezor Core) confirmation flows only advance when the
`DebugLinkDecision` is delivered on a connection that currently holds
`DebugLinkWatchLayout(watch=True)` — otherwise the firmware aborts the workflow
with "Communication with your connected device failed." (This matches how the
canonical trezorlib client keeps one persistent debug link with watch-layout
active for the whole signing flow.) So this helper speaks the DebugLink v1 wire
protocol directly over a single persistent UDP socket:
* Same v1 framing as the main wire: `##` + u16 type + u32 len, split into 64-byte
  chunks each prefixed with 0x3f.
* Messages used (message-type / field tags):
      DebugLinkWatchLayout = 9006  { watch: bool = field 1 }
      DebugLinkGetState    = 101   { (empty) }  -> DebugLinkState/Layout
      DebugLinkDecision    = 100   { button: DebugButton = field 1; YES = 1 }
  `watch_layout` and `get_state` read one reply; a `DebugLinkDecision` is sent
  fire-and-forget (no reply), mirroring trezorlib's `nowait` decision.

The debug UDP address defaults to the wire address (TREZOR_EMULATOR_UDP, default
127.0.0.1:21324) with its port incremented by 1, or the explicit
TREZOR_EMULATOR_DEBUG_UDP override.

Dependencies: `websockets` (for `setup` only) + Python stdlib.
"""

import argparse
import asyncio
import json
import os
import socket
import struct
import sys
import time

DEFAULT_WS = "ws://localhost:9001"
DEFAULT_WIRE = "127.0.0.1:21324"
SLIP14_MNEMONIC = "all all all all all all all all all all all all"

# DebugLink message wire types.
MT_DEBUG_DECISION = 100
MT_DEBUG_GET_STATE = 101
MT_DEBUG_WATCH_LAYOUT = 9006

# DebugButton.YES == 1 (field 1, varint).
_DECISION_YES = b"\x08\x01"
_WATCH_ON = b"\x08\x01"
_WATCH_OFF = b"\x08\x00"


# --------------------------------------------------------------------------- #
# Controller WebSocket (setup only)
# --------------------------------------------------------------------------- #
def ws_url() -> str:
    return os.environ.get("TREZOR_USER_ENV_WS", DEFAULT_WS)


async def ws_call(ws, obj, timeout=15.0):
    """Send one command and return the first reply carrying a "success" field."""
    await ws.send(json.dumps(obj))
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"no reply with 'success' for {obj.get('type')!r}")
        msg = await asyncio.wait_for(ws.recv(), timeout=remaining)
        try:
            data = json.loads(msg)
        except (ValueError, TypeError):
            continue
        if isinstance(data, dict) and "success" in data:
            return data


async def cmd_setup(args) -> int:
    import websockets  # imported lazily so press-yes/confirm-loop need only stdlib

    async with websockets.connect(ws_url(), max_size=None) as ws:
        start = await ws_call(
            ws,
            {"type": "emulator-start", "version": args.version, "model": args.model, "wipe": True},
            timeout=args.timeout,
        )
        if not start.get("success"):
            print(f"emulator-start failed: {start}", file=sys.stderr)
            return 1

        setup = await ws_call(
            ws,
            {
                "type": "emulator-setup",
                "mnemonic": args.mnemonic,
                "pin": args.pin,
                "passphrase_protection": args.passphrase_protection,
                "label": args.label,
            },
            timeout=args.timeout,
        )
        if not setup.get("success"):
            print(f"emulator-setup failed: {setup}", file=sys.stderr)
            return 1

        check = await ws_call(ws, {"type": "background-check"}, timeout=args.timeout)
        print(f"background-check: {json.dumps(check)}")
        emu = check.get("emulator_status", {})
        if not (check.get("background_check") and emu.get("is_running")):
            print("emulator not running after setup", file=sys.stderr)
            return 1
    return 0


# --------------------------------------------------------------------------- #
# DebugLink over UDP (press-yes / confirm-loop)
# --------------------------------------------------------------------------- #
def debug_addr():
    explicit = os.environ.get("TREZOR_EMULATOR_DEBUG_UDP")
    if explicit:
        host, port = explicit.rsplit(":", 1)
        return host, int(port)
    wire = os.environ.get("TREZOR_EMULATOR_UDP", DEFAULT_WIRE)
    host, port = wire.rsplit(":", 1)
    return host, int(port) + 1  # emulator debug port is wire port + 1


def _frame(mtype: int, payload: bytes):
    data = b"\x23\x23" + struct.pack(">H", mtype) + struct.pack(">I", len(payload)) + payload
    chunks = []
    cur = 0
    while cur < len(data):
        chunk = b"\x3f" + data[cur:cur + 63]
        cur += 63
        chunk += b"\x00" * (64 - len(chunk))
        chunks.append(chunk)
    return chunks


class DebugLink:
    """Minimal persistent DebugLink client over UDP (v1 framing, stdlib only)."""

    def __init__(self, addr, read_timeout=2.0):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.connect(addr)
        self.sock.settimeout(read_timeout)

    def _send(self, mtype: int, payload: bytes):
        for chunk in _frame(mtype, payload):
            self.sock.send(chunk)

    def _recv(self):
        chunk = self.sock.recv(64)
        if len(chunk) < 9 or chunk[0] != 0x3f or chunk[1] != 0x23 or chunk[2] != 0x23:
            raise ValueError("bad debug chunk header")
        mtype = struct.unpack(">H", chunk[3:5])[0]
        length = struct.unpack(">I", chunk[5:9])[0]
        data = chunk[9:]
        while len(data) < length:
            data += self.sock.recv(64)[1:]
        return mtype, data[:length]

    def watch_layout(self, on: bool):
        self._send(MT_DEBUG_WATCH_LAYOUT, _WATCH_ON if on else _WATCH_OFF)
        try:
            self._recv()  # drain the reply (Success)
        except socket.timeout:
            pass

    def get_state(self):
        """Return the raw DebugLinkState/Layout payload, or None on timeout."""
        self._send(MT_DEBUG_GET_STATE, b"")
        try:
            _mtype, payload = self._recv()
            return payload
        except socket.timeout:
            return None

    def press_yes(self):
        # Fire-and-forget decision (no reply expected), like trezorlib's nowait.
        self._send(MT_DEBUG_DECISION, _DECISION_YES)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def _is_homescreen(payload) -> bool:
    return payload is not None and b"Homescreen" in payload


def cmd_press_yes(_args) -> int:
    dl = DebugLink(debug_addr())
    try:
        dl.watch_layout(True)
        dl.press_yes()
        print("press-yes: sent DebugLinkDecision(YES)")
    finally:
        dl.watch_layout(False)
        dl.close()
    return 0


def cmd_confirm_loop(args) -> int:
    try:
        dl = DebugLink(debug_addr())
    except OSError as e:
        print(f"confirm-loop: cannot open debug link: {e!r}", file=sys.stderr)
        return 2

    deadline = time.monotonic() + args.timeout
    try:
        dl.watch_layout(True)
        while time.monotonic() < deadline:
            if args.stop and os.path.exists(args.stop):
                print("confirm-loop: stop sentinel found, exiting")
                return 0
            payload = dl.get_state()
            # Approve anything that is not the idle Homescreen. Unknown/blank
            # screens are treated as "maybe a confirmation" and pressed, keeping
            # the loop robust to layout changes across firmware versions.
            if not _is_homescreen(payload):
                dl.press_yes()
                print("confirm-loop: pressed yes", flush=True)
            time.sleep(args.interval)
    finally:
        dl.watch_layout(False)
        dl.close()
    print("confirm-loop: timeout elapsed, exiting")
    return 0


# --------------------------------------------------------------------------- #
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Trezor emulator controller helper")
    sub = parser.add_subparsers(dest="command", required=True)

    p_setup = sub.add_parser("setup", help="start + wipe + seed the emulator")
    p_setup.add_argument("--model", default="T2T1")
    p_setup.add_argument("--version", default="2-main")
    p_setup.add_argument("--mnemonic", default=SLIP14_MNEMONIC)
    p_setup.add_argument("--pin", default="")
    p_setup.add_argument("--passphrase-protection", dest="passphrase_protection",
                         action="store_true", default=False)
    p_setup.add_argument("--label", default="Test")
    p_setup.add_argument("--timeout", type=float, default=60.0)
    p_setup.set_defaults(func=cmd_setup, is_async=True)

    p_yes = sub.add_parser("press-yes", help="send a single DebugLinkDecision(YES)")
    p_yes.set_defaults(func=cmd_press_yes, is_async=False)

    p_loop = sub.add_parser("confirm-loop", help="auto-approve on-device confirmations")
    p_loop.add_argument("--timeout", type=float, default=30.0)
    p_loop.add_argument("--interval", type=float, default=0.3)
    p_loop.add_argument("--stop", default=None, help="path to a sentinel file that stops the loop")
    p_loop.set_defaults(func=cmd_confirm_loop, is_async=False)

    return parser


def main() -> int:
    args = build_parser().parse_args()
    if getattr(args, "is_async", False):
        try:
            import websockets  # noqa: F401
        except ImportError:
            print("the 'websockets' package is required for 'setup'", file=sys.stderr)
            return 2
        try:
            return asyncio.run(args.func(args))
        except OSError as e:
            print(f"controller connection error: {e!r}", file=sys.stderr)
            return 2
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
