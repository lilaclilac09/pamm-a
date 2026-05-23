"""
Read-only operations over a Binary Ninja BinaryView.

All functions accept a live `bv` (BinaryView) and return plain dicts
ready for JSON serialisation. Nothing here mutates analysis state.
"""

from __future__ import annotations

import hashlib
import re
from typing import Any

try:
    import binaryninja as bn
    from binaryninja import BinaryView, Function, SymbolType
    from binaryninja.enums import ReferenceType
    _BN_AVAILABLE = True
except ImportError:
    _BN_AVAILABLE = False
    BinaryView = Any
    Function = Any


# ── helpers ──────────────────────────────────────────────────────────────────

def _hex(addr: int) -> str:
    return hex(addr)


def _fn_id(f: "Function") -> str:
    return _hex(f.start)


def _import_tags(imports: list[dict]) -> set[str]:
    """Classify imports into capability buckets."""
    NET = {"connect", "send", "recv", "internetopen", "internetconnect",
           "httpopen", "httpsend", "wsaconnect", "gethostbyname", "getaddrinfo",
           "winhttp", "curl", "socket"}
    CRYPTO = {"cryptencrypt", "cryptdecrypt", "crypthashdata", "cryptderivekey",
               "bcrypt", "ncrypt", "sha", "md5", "aes", "rc4"}
    CRED = {"login data", "password", "credential", "dpapi", "cryptunprotectdata",
             "sqlite3_open", "keychain"}
    REG = {"regsetvalue", "regcreatekey", "regopenkey", "regqueryvalue"}
    PERSIST = {"createservice", "scopen", "taskscheduler", "runoncekey",
                "appdata", "startup"}
    ANTI = {"isdebuggerpresent", "checkremotedebugger", "ntsetinformationthread",
             "outputdebugstring", "rdtsc", "cpuid", "gettickcount"}

    tags: set[str] = set()
    for imp in imports:
        key = (imp.get("name", "") + imp.get("module", "")).lower()
        if any(t in key for t in NET):
            tags.add("network")
        if any(t in key for t in CRYPTO):
            tags.add("crypto")
        if any(t in key for t in CRED):
            tags.add("credential")
        if any(t in key for t in REG):
            tags.add("registry")
        if any(t in key for t in PERSIST):
            tags.add("persistence")
        if any(t in key for t in ANTI):
            tags.add("anti_debug")
    return tags


def _interesting_strings(strings: list[str]) -> list[str]:
    patterns = [
        r"https?://", r"%appdata%", r"%temp%", r"login data",
        r"password", r"user.agent", r"runonce", r"cmd\.exe",
        r"powershell", r"\\pipe\\", r"\\\.\\", r"gate\.php",
        r"\.onion", r"config", r"c2", r"beacon",
    ]
    out = []
    for s in strings:
        low = s.lower()
        if any(re.search(p, low) for p in patterns):
            out.append(s)
    return out[:40]


# ── session ───────────────────────────────────────────────────────────────────

def get_session(bv: BinaryView) -> dict:
    sha256 = hashlib.sha256(bv.read(bv.start, min(0x10000, len(bv)))).hexdigest()

    current_fn = None
    if hasattr(bv, "offset"):
        fns = bv.get_functions_containing(bv.offset)
        if fns:
            f = fns[0]
            current_fn = {"name": f.name, "start": _hex(f.start)}

    return {
        "binary_name": bv.file.filename.split("/")[-1],
        "binary_id": f"{bv.file.filename}::sha256:{sha256[:16]}",
        "arch": bv.arch.name if bv.arch else "unknown",
        "platform": bv.platform.name if bv.platform else "unknown",
        "entry_points": [_hex(ep) for ep in bv.entry_points],
        "current_address": _hex(bv.offset) if hasattr(bv, "offset") else None,
        "current_function": current_fn,
        "function_count": len(list(bv.functions)),
    }


# ── overview ──────────────────────────────────────────────────────────────────

def get_overview(bv: BinaryView, include: list[str]) -> dict:
    result: dict[str, Any] = {}

    if "imports" in include:
        result["imports"] = _get_imports(bv)

    if "sections" in include:
        result["sections"] = _get_sections(bv)

    if "strings_stats" in include:
        strs = [s.value for s in bv.get_strings()]
        interesting = _interesting_strings(strs)
        result["strings_stats"] = {
            "count": len(strs),
            "interesting": interesting[:20],
        }

    if "function_stats" in include:
        fns = sorted(bv.functions, key=lambda f: f.total_bytes, reverse=True)
        result["function_stats"] = {
            "count": len(fns),
            "largest": [
                {"name": f.name, "start": _hex(f.start), "size": f.total_bytes}
                for f in fns[:5]
            ],
        }

    if "capability_tags" in include:
        imports = result.get("imports") or _get_imports(bv)
        result["capability_tags"] = sorted(_import_tags(imports))

    return result


def _get_imports(bv: BinaryView) -> list[dict]:
    out = []
    seen = set()
    for sym in bv.get_symbols_by_type(SymbolType.ImportedFunctionSymbol):
        if sym.name in seen:
            continue
        seen.add(sym.name)
        module = ""
        if "::" in sym.full_name:
            module = sym.full_name.split("::")[0]
        out.append({
            "module": module,
            "name": sym.name,
            "address": _hex(sym.address),
        })
    for sym in bv.get_symbols_by_type(SymbolType.ImportAddressSymbol):
        if sym.name in seen:
            continue
        seen.add(sym.name)
        module = ""
        if "::" in sym.full_name:
            module = sym.full_name.split("::")[0]
        out.append({
            "module": module,
            "name": sym.name,
            "address": _hex(sym.address),
        })
    return out


def _get_sections(bv: BinaryView) -> list[dict]:
    out = []
    for name, section in bv.sections.items():
        length = section.end - section.start
        entropy = 0.0
        if length > 0:
            try:
                data = bv.read(section.start, min(length, 0x10000))
                if data:
                    import math
                    freq = [0] * 256
                    for b in data:
                        freq[b] += 1
                    n = len(data)
                    entropy = -sum(
                        (c / n) * math.log2(c / n)
                        for c in freq if c > 0
                    )
            except Exception:
                pass
        out.append({
            "name": name,
            "start": _hex(section.start),
            "end": _hex(section.end),
            "size": length,
            "entropy": round(entropy, 2),
        })
    return out


# ── discovery ─────────────────────────────────────────────────────────────────

def list_functions(
    bv: BinaryView,
    filter_opts: dict | None = None,
    sort_by: str = "suspicion_score",
    sort_order: str = "desc",
    limit: int = 30,
    cursor: int = 0,
) -> dict:
    from .ranking import score_function

    filter_opts = filter_opts or {}
    fns = list(bv.functions)

    # apply filters
    name_q = filter_opts.get("name_contains", "").lower()
    if name_q:
        fns = [f for f in fns if name_q in f.name.lower()]

    min_sz = filter_opts.get("min_size")
    if min_sz is not None:
        fns = [f for f in fns if f.total_bytes >= min_sz]

    max_sz = filter_opts.get("max_size")
    if max_sz is not None:
        fns = [f for f in fns if f.total_bytes <= max_sz]

    tag_q = filter_opts.get("tag")
    if tag_q:
        fns = [f for f in fns if tag_q in (t.data for t in f.address_tags)]

    has_import = filter_opts.get("has_import", "").lower()
    ref_string = filter_opts.get("referencing_string", "")

    scored = []
    for f in fns:
        entry = _fn_summary_light(bv, f)
        if has_import and not any(has_import in i.lower() for i in entry["imports_used"]):
            continue
        if ref_string and not any(ref_string.lower() in s.lower() for s in entry["string_refs"]):
            continue
        s = score_function(entry)
        entry["score"] = s
        scored.append(entry)

    # sort
    reverse = sort_order == "desc"
    if sort_by == "suspicion_score":
        scored.sort(key=lambda x: x["score"]["suspicion"], reverse=reverse)
    elif sort_by == "size":
        scored.sort(key=lambda x: x["size"], reverse=reverse)
    elif sort_by == "callers":
        scored.sort(key=lambda x: x["callers"], reverse=reverse)

    total = len(scored)
    page = scored[cursor: cursor + limit]

    return {
        "total": total,
        "cursor": cursor + len(page),
        "has_more": cursor + len(page) < total,
        "functions": page,
    }


def _fn_summary_light(bv: BinaryView, f: "Function") -> dict:
    """Lightweight summary for ranking — no decompile."""
    callers = list(bv.get_callers(f.start))
    callees = list(f.callees)

    import_syms = {s.name.lower() for s in bv.get_symbols_by_type(SymbolType.ImportedFunctionSymbol)}
    import_syms |= {s.name.lower() for s in bv.get_symbols_by_type(SymbolType.ImportAddressSymbol)}

    imports_used = []
    for callee in callees:
        name = callee.name.lower()
        if name in import_syms:
            imports_used.append(callee.name)

    string_refs = []
    for s in bv.get_strings():
        refs = bv.get_code_refs(s.start)
        for ref in refs:
            if ref.function and ref.function.start == f.start:
                string_refs.append(s.value)
                break

    tags_data = [t.data for t in f.address_tags]

    return {
        "name": f.name,
        "start": _hex(f.start),
        "size": f.total_bytes,
        "basic_blocks": len(list(f.basic_blocks)),
        "callers": len(callers),
        "callees": len(callees),
        "imports_used": imports_used,
        "string_refs": string_refs[:10],
        "tags": tags_data,
    }


def search_strings(
    bv: BinaryView,
    query: str,
    use_regex: bool = False,
    case_sensitive: bool = False,
    limit: int = 50,
) -> dict:
    strs = bv.get_strings()
    matches = []
    flags = 0 if case_sensitive else re.IGNORECASE

    for s in strs:
        val = s.value
        hit = re.search(query, val, flags) if use_regex else (
            (query in val) if case_sensitive else (query.lower() in val.lower())
        )
        if hit:
            refs = [_hex(r.address) for r in bv.get_code_refs(s.start)][:10]
            matches.append({"value": val, "address": _hex(s.start), "refs": refs})
        if len(matches) >= limit:
            break

    return {"matches": matches, "count": len(matches)}


def search_imports(bv: BinaryView, query: str, use_regex: bool = True) -> dict:
    imports = _get_imports(bv)
    flags = re.IGNORECASE
    matches = []
    for imp in imports:
        key = imp["module"] + imp["name"]
        if use_regex:
            if re.search(query, key, flags):
                matches.append(imp)
        else:
            if query.lower() in key.lower():
                matches.append(imp)
    return {"matches": matches, "count": len(matches)}


def find_references(
    bv: BinaryView,
    target: dict,
    limit: int = 20,
) -> dict:
    """Find all code references to a string value, import name, address, or symbol."""
    refs = []
    t_type = target.get("type", "address")
    t_value = target.get("value", "")

    if t_type == "string":
        for s in bv.get_strings():
            if t_value.lower() in s.value.lower():
                for ref in bv.get_code_refs(s.start):
                    fn_name = ref.function.name if ref.function else "?"
                    refs.append({
                        "kind": "string_ref",
                        "string": s.value,
                        "ref_address": _hex(ref.address),
                        "function": fn_name,
                        "function_start": _hex(ref.function.start) if ref.function else None,
                    })
                    if len(refs) >= limit:
                        break
            if len(refs) >= limit:
                break

    elif t_type in ("import", "symbol"):
        for sym in list(bv.get_symbols_by_type(SymbolType.ImportedFunctionSymbol)) + \
                   list(bv.get_symbols_by_type(SymbolType.ImportAddressSymbol)):
            if t_value.lower() in sym.name.lower():
                for ref in bv.get_code_refs(sym.address):
                    fn_name = ref.function.name if ref.function else "?"
                    refs.append({
                        "kind": "import_ref",
                        "symbol": sym.name,
                        "ref_address": _hex(ref.address),
                        "function": fn_name,
                        "function_start": _hex(ref.function.start) if ref.function else None,
                    })
                    if len(refs) >= limit:
                        break
            if len(refs) >= limit:
                break

    elif t_type == "address":
        addr = int(t_value, 16) if isinstance(t_value, str) else t_value
        for ref in bv.get_code_refs(addr):
            fn_name = ref.function.name if ref.function else "?"
            refs.append({
                "kind": "code_ref",
                "ref_address": _hex(ref.address),
                "function": fn_name,
                "function_start": _hex(ref.function.start) if ref.function else None,
            })
            if len(refs) >= limit:
                break
        for ref_addr in bv.get_data_refs(addr):
            refs.append({"kind": "data_ref", "ref_address": _hex(ref_addr)})
            if len(refs) >= limit:
                break

    return {"target": target, "refs": refs, "count": len(refs)}


# ── inspection ────────────────────────────────────────────────────────────────

def get_function_summary(bv: BinaryView, function_addr: int) -> dict:
    fns = bv.get_functions_containing(function_addr)
    if not fns:
        # try by start address
        f = bv.get_function_at(function_addr)
        if not f:
            return {"error": f"no function at {_hex(function_addr)}"}
    else:
        f = fns[0]

    callers = [{"name": c.name, "start": _hex(c.start)} for c in bv.get_callers(f.start)[:20]]
    callees = [{"name": c.name, "start": _hex(c.start)} for c in f.callees[:20]]

    import_syms = {s.name for s in bv.get_symbols_by_type(SymbolType.ImportedFunctionSymbol)}
    import_syms |= {s.name for s in bv.get_symbols_by_type(SymbolType.ImportAddressSymbol)}
    imports_used = [c["name"] for c in callees if c["name"] in import_syms]

    string_refs = []
    for s in bv.get_strings():
        for ref in bv.get_code_refs(s.start):
            if ref.function and ref.function.start == f.start:
                string_refs.append(s.value)
                break

    constants = _collect_constants(f)

    stack_vars = []
    if f.hlil:
        try:
            for var in f.vars:
                stack_vars.append({
                    "name": var.name,
                    "type": str(var.type) if var.type else "unknown",
                })
        except Exception:
            pass

    notes_data = _get_function_notes(bv, _hex(f.start))

    return {
        "name": f.name,
        "start": _hex(f.start),
        "end": _hex(f.start + f.total_bytes),
        "size": f.total_bytes,
        "basic_blocks": len(list(f.basic_blocks)),
        "stack_vars": stack_vars[:20],
        "constants": constants[:20],
        "string_refs": string_refs[:20],
        "imports_used": imports_used,
        "callees": callees,
        "callers": callers,
        "tags": [t.data for t in f.address_tags],
        "notes": notes_data,
    }


def _collect_constants(f: "Function") -> list[str]:
    """Extract interesting constants from LLIL."""
    consts = set()
    try:
        for block in f.llil:
            for instr in block:
                _walk_llil_consts(instr, consts)
    except Exception:
        pass
    # filter boring values
    return [_hex(c) for c in sorted(consts) if c > 0x100 and c not in (0xFFFFFFFF, 0xFFFF, 0x7FFFFFFF)][:20]


def _walk_llil_consts(instr: Any, out: set) -> None:
    try:
        from binaryninja.enums import LowLevelILOperation
        if instr.operation in (LowLevelILOperation.LLIL_CONST, LowLevelILOperation.LLIL_CONST_PTR):
            out.add(instr.constant)
        for op in instr.operands:
            if hasattr(op, "operation"):
                _walk_llil_consts(op, out)
    except Exception:
        pass


def _get_function_notes(bv: BinaryView, fn_addr: str) -> list[dict]:
    """Pull notes from the sidecar notebook."""
    try:
        from .notes import load_notes
        nb = load_notes(bv)
        return nb.get("function_notes", {}).get(fn_addr, [])
    except Exception:
        return []


def decompile_function(
    bv: BinaryView,
    function_addr: int,
    view: str = "hlil",
    max_lines: int = 120,
) -> dict:
    fns = bv.get_functions_containing(function_addr)
    f = fns[0] if fns else bv.get_function_at(function_addr)
    if not f:
        return {"error": f"no function at {_hex(function_addr)}"}

    lines = []
    truncated = False

    if view == "hlil":
        il = f.hlil
        if il:
            for block in il:
                for instr in block:
                    lines.append(str(instr))
                    if len(lines) >= max_lines:
                        truncated = True
                        break
                if truncated:
                    break
    elif view == "mlil":
        il = f.mlil
        if il:
            for block in il:
                for instr in block:
                    lines.append(str(instr))
                    if len(lines) >= max_lines:
                        truncated = True
                        break
                if truncated:
                    break
    elif view == "llil":
        il = f.llil
        if il:
            for block in il:
                for instr in block:
                    lines.append(str(instr))
                    if len(lines) >= max_lines:
                        truncated = True
                        break
                if truncated:
                    break
    elif view == "disasm":
        for block in f.basic_blocks:
            for tok, addr in block.disassembly_text:
                lines.append(f"{_hex(addr)}: {''.join(str(t) for t in tok)}")
                if len(lines) >= max_lines:
                    truncated = True
                    break
            if truncated:
                break

    return {
        "function": f.name,
        "start": _hex(f.start),
        "view": view,
        "lines": len(lines),
        "text": "\n".join(lines),
        "truncated": truncated,
    }


def get_callers(bv: BinaryView, function_addr: int, limit: int = 20) -> dict:
    f = bv.get_function_at(function_addr)
    if not f:
        fns = bv.get_functions_containing(function_addr)
        f = fns[0] if fns else None
    if not f:
        return {"error": f"no function at {_hex(function_addr)}"}
    callers = [
        {"name": c.name, "start": _hex(c.start), "size": c.total_bytes}
        for c in list(bv.get_callers(f.start))[:limit]
    ]
    return {"function": f.name, "callers": callers, "count": len(callers)}


def get_callees(bv: BinaryView, function_addr: int, limit: int = 20) -> dict:
    f = bv.get_function_at(function_addr)
    if not f:
        fns = bv.get_functions_containing(function_addr)
        f = fns[0] if fns else None
    if not f:
        return {"error": f"no function at {_hex(function_addr)}"}
    callees = [
        {"name": c.name, "start": _hex(c.start), "size": c.total_bytes}
        for c in list(f.callees)[:limit]
    ]
    return {"function": f.name, "callees": callees, "count": len(callees)}


def get_xrefs(bv: BinaryView, address: int, limit: int = 20) -> dict:
    code_refs = [
        {
            "kind": "code",
            "from_address": _hex(r.address),
            "from_function": r.function.name if r.function else "?",
        }
        for r in list(bv.get_code_refs(address))[:limit]
    ]
    data_refs = [
        {"kind": "data", "from_address": _hex(a)}
        for a in list(bv.get_data_refs(address))[: limit - len(code_refs)]
    ]
    refs = code_refs + data_refs
    return {"address": _hex(address), "refs": refs, "count": len(refs)}


def read_bytes(bv: BinaryView, address: int, length: int) -> dict:
    data = bv.read(address, min(length, 0x400))
    if data is None:
        return {"error": f"cannot read at {_hex(address)}"}
    return {
        "address": _hex(address),
        "length": len(data),
        "hex": data.hex(),
        "printable": "".join(chr(b) if 32 <= b < 127 else "." for b in data),
    }
