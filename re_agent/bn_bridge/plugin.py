"""
Binary Ninja plugin — starts a Flask HTTP bridge server on localhost:7734.

Drop this file into your Binary Ninja plugins directory:
  macOS:   ~/Library/Application Support/Binary Ninja/plugins/
  Linux:   ~/.binaryninja/plugins/
  Windows: %APPDATA%\Binary Ninja\plugins\

The server exposes all RE agent tools as REST endpoints.
The agent client connects to http://localhost:7734.

Usage inside BN: the server starts automatically when BN loads the plugin.
Stop it via Plugins → RE Agent → Stop Bridge Server.
"""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any
from urllib.parse import urlparse, parse_qs

import binaryninja as bn
from binaryninja import BinaryView
from binaryninja.plugin import PluginCommand

from . import inspectors as insp
from . import mutation as mut
from . import notes as nb_mod
from .ranking import score_function

PORT = 7734
_server: HTTPServer | None = None
_bv: BinaryView | None = None


# ── response helpers ──────────────────────────────────────────────────────────

def _ok(data: Any, meta: dict | None = None) -> bytes:
    payload = {
        "ok": True,
        "data": data,
        "error": None,
        "meta": meta or _meta(),
    }
    return json.dumps(payload).encode()


def _err(code: str, message: str) -> bytes:
    payload = {
        "ok": False,
        "data": None,
        "error": {"code": code, "message": message},
        "meta": _meta(),
    }
    return json.dumps(payload).encode()


def _meta() -> dict:
    global _bv
    return {
        "binary_id": (_bv.file.filename if _bv else None),
        "view": "hlil",
    }


# ── HTTP handler ──────────────────────────────────────────────────────────────

class BridgeHandler(BaseHTTPRequestHandler):

    def log_message(self, fmt, *args):
        pass  # silence default logging

    def _read_body(self) -> dict:
        length = int(self.headers.get("Content-Length", 0))
        if length:
            return json.loads(self.rfile.read(length))
        return {}

    def _send(self, body: bytes, status: int = 200) -> None:
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        global _bv
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/")
        qs = parse_qs(parsed.query)

        if not _bv:
            return self._send(_err("NO_BINARY", "no binary open in BN"), 503)

        try:
            if path == "/session":
                self._send(_ok(insp.get_session(_bv)))

            elif path == "/overview":
                include = qs.get("include", ["imports,sections,strings_stats,function_stats"])[0].split(",")
                self._send(_ok(insp.get_overview(_bv, include)))

            elif path == "/notes":
                scope = qs.get("scope", [None])[0]
                subject = qs.get("subject", [None])[0]
                self._send(_ok(nb_mod.list_notes(_bv, scope, subject)))

            elif path == "/notes/export":
                self._send(_ok(nb_mod.export_summary(_bv)))

            else:
                self._send(_err("NOT_FOUND", f"unknown endpoint: {path}"), 404)

        except Exception as exc:
            self._send(_err("INTERNAL_ERROR", str(exc)), 500)

    def do_POST(self):
        global _bv
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/")

        if not _bv:
            return self._send(_err("NO_BINARY", "no binary open in BN"), 503)

        body = self._read_body()

        try:
            # ── discovery ─────────────────────────────────────────────────────
            if path == "/functions":
                result = insp.list_functions(
                    _bv,
                    filter_opts=body.get("filter"),
                    sort_by=body.get("sort", {}).get("by", "suspicion_score"),
                    sort_order=body.get("sort", {}).get("order", "desc"),
                    limit=body.get("limit", 30),
                    cursor=body.get("cursor", 0),
                )
                self._send(_ok(result))

            elif path == "/strings/search":
                self._send(_ok(insp.search_strings(
                    _bv,
                    query=body["query"],
                    use_regex=body.get("regex", False),
                    case_sensitive=body.get("case_sensitive", False),
                    limit=body.get("limit", 50),
                )))

            elif path == "/imports/search":
                self._send(_ok(insp.search_imports(
                    _bv,
                    query=body["query"],
                    use_regex=body.get("regex", True),
                )))

            elif path == "/references":
                self._send(_ok(insp.find_references(
                    _bv,
                    target=body["target"],
                    limit=body.get("limit", 20),
                )))

            # ── inspection ────────────────────────────────────────────────────
            elif path == "/function/summary":
                addr = int(body["function"], 16)
                self._send(_ok(insp.get_function_summary(_bv, addr)))

            elif path == "/function/decompile":
                addr = int(body["function"], 16)
                self._send(_ok(insp.decompile_function(
                    _bv, addr,
                    view=body.get("view", "hlil"),
                    max_lines=body.get("max_lines", 120),
                )))

            elif path == "/function/callers":
                addr = int(body["function"], 16)
                self._send(_ok(insp.get_callers(_bv, addr, body.get("limit", 20))))

            elif path == "/function/callees":
                addr = int(body["function"], 16)
                self._send(_ok(insp.get_callees(_bv, addr, body.get("limit", 20))))

            elif path == "/xrefs":
                addr = int(body["address"], 16)
                self._send(_ok(insp.get_xrefs(_bv, addr, body.get("limit", 20))))

            elif path == "/bytes":
                addr = int(body["address"], 16)
                self._send(_ok(insp.read_bytes(_bv, addr, body.get("length", 64))))

            # ── ranking ───────────────────────────────────────────────────────
            elif path == "/functions/rank":
                objective = body.get("objective", "")
                limit = body.get("limit", 15)
                result = insp.list_functions(
                    _bv,
                    sort_by="suspicion_score",
                    sort_order="desc",
                    limit=limit,
                )
                self._send(_ok(result))

            # ── mutation ──────────────────────────────────────────────────────
            elif path == "/rename":
                addr = int(body["address"], 16)
                self._send(_ok(mut.rename_symbol(
                    _bv, addr,
                    new_name=body["new_name"],
                    preview=body.get("preview", True),
                )))

            elif path == "/comment":
                addr = int(body["address"], 16)
                self._send(_ok(mut.set_comment(
                    _bv, addr,
                    text=body["text"],
                    preview=body.get("preview", True),
                )))

            elif path == "/tag":
                addr = int(body["function"], 16)
                self._send(_ok(mut.tag_function(
                    _bv, addr,
                    tag=body["tag"],
                    preview=body.get("preview", True),
                )))

            elif path == "/patch/preview":
                addr = int(body["address"], 16)
                self._send(_ok(mut.apply_patch_preview(
                    _bv, addr, patch_hex=body["patch"],
                )))

            elif path == "/patch/commit":
                addr = int(body["address"], 16)
                self._send(_ok(mut.apply_patch_commit(
                    _bv, addr, patch_hex=body["patch"],
                )))

            # ── notebook ──────────────────────────────────────────────────────
            elif path == "/notes/write":
                self._send(_ok(nb_mod.write_note(
                    _bv,
                    scope=body.get("scope", "binary"),
                    subject=body.get("subject", _bv.file.filename),
                    kind=body["kind"],
                    text=body["text"],
                    evidence=body.get("evidence"),
                    confidence=body.get("confidence"),
                    next_checks=body.get("next_checks"),
                )))

            elif path == "/notes/objective":
                self._send(_ok(nb_mod.set_objective(_bv, body["objective"])))

            else:
                self._send(_err("NOT_FOUND", f"unknown endpoint: {path}"), 404)

        except KeyError as exc:
            self._send(_err("MISSING_PARAM", f"missing required param: {exc}"), 400)
        except Exception as exc:
            self._send(_err("INTERNAL_ERROR", str(exc)), 500)


# ── server lifecycle ──────────────────────────────────────────────────────────

def _start_server() -> None:
    global _server
    if _server:
        return
    _server = HTTPServer(("127.0.0.1", PORT), BridgeHandler)
    t = threading.Thread(target=_server.serve_forever, daemon=True)
    t.start()
    bn.log_info(f"[RE Agent] bridge server running on http://127.0.0.1:{PORT}")


def _stop_server() -> None:
    global _server
    if _server:
        _server.shutdown()
        _server = None
        bn.log_info("[RE Agent] bridge server stopped")


def _set_bv(bv: BinaryView) -> None:
    global _bv
    _bv = bv
    bn.log_info(f"[RE Agent] active binary: {bv.file.filename}")


# ── BN plugin registration ────────────────────────────────────────────────────

def _cmd_start(bv: BinaryView):
    _set_bv(bv)
    _start_server()
    bn.show_message_box(
        "RE Agent Bridge",
        f"Bridge server running on http://127.0.0.1:{PORT}\n"
        "Run the agent client: python run_agent.py --bridge http://127.0.0.1:7734",
        bn.MessageBoxButtonSet.OKButtonSet,
    )


def _cmd_stop(bv: BinaryView):
    _stop_server()


PluginCommand.register(
    "RE Agent\\Start Bridge Server",
    "Start the RE Agent HTTP bridge on localhost:7734",
    _cmd_start,
)

PluginCommand.register(
    "RE Agent\\Stop Bridge Server",
    "Stop the RE Agent HTTP bridge",
    _cmd_stop,
)

# Auto-start when a binary is opened
def _on_bv_open(bv: BinaryView):
    _set_bv(bv)
    _start_server()

bn.BinaryViewType.add_binaryview_finalized_event(_on_bv_open)
