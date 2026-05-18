"""
Write operations over a Binary Ninja BinaryView.

Every mutation returns a preview diff by default.
Nothing is committed to the analysis database until `commit=True` is passed.
"""

from __future__ import annotations

from typing import Any

try:
    import binaryninja as bn
    from binaryninja import BinaryView, Symbol, SymbolType
    _BN_AVAILABLE = True
except ImportError:
    _BN_AVAILABLE = False
    BinaryView = Any


def _hex(addr: int) -> str:
    return hex(addr)


# ── rename ────────────────────────────────────────────────────────────────────

def rename_symbol(
    bv: BinaryView,
    address: int,
    new_name: str,
    preview: bool = True,
) -> dict:
    """
    Rename the function / data symbol at `address`.

    With preview=True returns a diff without touching the analysis db.
    With preview=False (commit) applies the rename.
    """
    f = bv.get_function_at(address)
    if not f:
        fns = bv.get_functions_containing(address)
        f = fns[0] if fns else None

    old_name = f.name if f else _get_symbol_name(bv, address)

    diff = {
        "operation": "rename",
        "address": _hex(address),
        "old_name": old_name,
        "new_name": new_name,
        "committed": False,
    }

    if not preview:
        if f:
            f.name = new_name
        else:
            sym = bv.get_symbol_at(address)
            if sym:
                bv.define_user_symbol(
                    Symbol(sym.type, address, new_name)
                )
            else:
                bv.define_user_symbol(
                    Symbol(SymbolType.FunctionSymbol, address, new_name)
                )
        diff["committed"] = True

    return diff


def _get_symbol_name(bv: BinaryView, address: int) -> str | None:
    sym = bv.get_symbol_at(address)
    return sym.name if sym else None


# ── comment ───────────────────────────────────────────────────────────────────

def set_comment(
    bv: BinaryView,
    address: int,
    text: str,
    preview: bool = True,
) -> dict:
    existing = bv.get_comment_at(address)

    diff = {
        "operation": "comment",
        "address": _hex(address),
        "old_comment": existing or "",
        "new_comment": text,
        "committed": False,
    }

    if not preview:
        bv.set_comment_at(address, text)
        diff["committed"] = True

    return diff


# ── tag ───────────────────────────────────────────────────────────────────────

def tag_function(
    bv: BinaryView,
    function_addr: int,
    tag: str,
    preview: bool = True,
) -> dict:
    f = bv.get_function_at(function_addr)
    if not f:
        fns = bv.get_functions_containing(function_addr)
        f = fns[0] if fns else None

    current_tags = [t.data for t in f.address_tags] if f else []

    diff = {
        "operation": "tag",
        "function": f.name if f else _hex(function_addr),
        "address": _hex(function_addr),
        "existing_tags": current_tags,
        "new_tag": tag,
        "committed": False,
    }

    if not preview and f:
        # Get or create the tag type
        tag_type = None
        for tt in bv.tag_types.values():
            if tt.name == "RE_Agent":
                tag_type = tt
                break
        if tag_type is None:
            tag_type = bv.create_tag_type("RE_Agent", "🔍")

        f.add_user_address_tag(f.start, bv.create_tag(tag_type, tag))
        diff["committed"] = True

    return diff


# ── patch ─────────────────────────────────────────────────────────────────────

def apply_patch_preview(
    bv: BinaryView,
    address: int,
    patch_hex: str,
) -> dict:
    """Generate a patch diff without writing anything."""
    patch_bytes = bytes.fromhex(patch_hex.replace(" ", ""))
    length = len(patch_bytes)

    original = bv.read(address, length)
    if original is None:
        return {"error": f"cannot read {length} bytes at {_hex(address)}"}

    original_disasm = _disasm_range(bv, address, address + length)
    # Simulate what the patch would look like
    patch_disasm = "(apply commit to see patched disassembly)"

    return {
        "operation": "patch_preview",
        "address": _hex(address),
        "length": length,
        "original_hex": original.hex(),
        "patch_hex": patch_bytes.hex(),
        "original_disasm": original_disasm,
        "patch_disasm": patch_disasm,
        "committed": False,
    }


def apply_patch_commit(
    bv: BinaryView,
    address: int,
    patch_hex: str,
) -> dict:
    """Apply a patch. Should only be called after the user approves the preview."""
    patch_bytes = bytes.fromhex(patch_hex.replace(" ", ""))
    original = bv.read(address, len(patch_bytes))
    if original is None:
        return {"error": f"cannot read at {_hex(address)}"}

    written = bv.write(address, patch_bytes)
    bv.update_analysis_and_wait()

    return {
        "operation": "patch_commit",
        "address": _hex(address),
        "original_hex": original.hex(),
        "patch_hex": patch_bytes.hex(),
        "bytes_written": written,
        "committed": True,
    }


def _disasm_range(bv: BinaryView, start: int, end: int) -> str:
    lines = []
    addr = start
    while addr < end:
        text = bv.get_disassembly(addr)
        if not text:
            break
        lines.append(f"{_hex(addr)}: {text}")
        # advance by instruction length (approximation)
        instr_len = bv.get_instruction_length(addr)
        if instr_len <= 0:
            break
        addr += instr_len
    return "\n".join(lines)
