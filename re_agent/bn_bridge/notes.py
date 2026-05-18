"""
Per-binary JSON notebook — facts, hypotheses, unknowns, todos, and timeline.

The sidecar file sits next to the binary: <binary_path>.re_notes.json
Everything here is pure Python / JSON, no BN dependency.
"""

from __future__ import annotations

import json
import os
import time
from typing import Any


def _notes_path(bv_or_path: Any) -> str:
    if isinstance(bv_or_path, str):
        base = bv_or_path
    else:
        base = bv_or_path.file.filename
    return base + ".re_notes.json"


def load_notes(bv_or_path: Any) -> dict:
    path = _notes_path(bv_or_path)
    if os.path.exists(path):
        with open(path) as f:
            return json.load(f)
    return _empty_notebook(bv_or_path)


def save_notes(bv_or_path: Any, nb: dict) -> None:
    path = _notes_path(bv_or_path)
    with open(path, "w") as f:
        json.dump(nb, f, indent=2)


def _empty_notebook(bv_or_path: Any) -> dict:
    name = bv_or_path if isinstance(bv_or_path, str) else bv_or_path.file.filename
    return {
        "binary_id": name,
        "objective": "",
        "facts": [],
        "hypotheses": [],
        "unknowns": [],
        "todos": [],
        "timeline": [],
        "function_notes": {},
    }


# ── public API ────────────────────────────────────────────────────────────────

def write_note(
    bv_or_path: Any,
    scope: str,          # "binary" | "function"
    subject: str,        # binary_id or function address hex string
    kind: str,           # "fact" | "hypothesis" | "unknown" | "todo"
    text: str,
    evidence: list[str] | None = None,
    confidence: float | None = None,
    next_checks: list[str] | None = None,
) -> dict:
    nb = load_notes(bv_or_path)

    note = {
        "kind": kind,
        "subject": subject,
        "scope": scope,
        "text": text,
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    if evidence:
        note["evidence"] = evidence
    if confidence is not None:
        note["confidence"] = confidence
    if next_checks:
        note["next_checks"] = next_checks

    # store in the right bucket
    if scope == "function":
        fn_notes = nb.setdefault("function_notes", {})
        fn_notes.setdefault(subject, []).append(note)
    else:
        bucket = nb.setdefault(kind + "s", [])  # facts, hypotheses, unknowns, todos
        bucket.append(note)

    # always append to timeline
    nb.setdefault("timeline", []).append({
        "step": len(nb["timeline"]) + 1,
        "action": f"write_note:{kind}",
        "subject": subject,
        "text": text[:120],
        "timestamp": note["timestamp"],
    })

    save_notes(bv_or_path, nb)
    return {"ok": True, "note": note}


def list_notes(bv_or_path: Any, scope: str | None = None, subject: str | None = None) -> dict:
    nb = load_notes(bv_or_path)

    if scope == "function" and subject:
        return {
            "function_notes": nb.get("function_notes", {}).get(subject, [])
        }

    return {
        "facts": nb.get("facts", []),
        "hypotheses": nb.get("hypotheses", []),
        "unknowns": nb.get("unknowns", []),
        "todos": nb.get("todos", []),
        "objective": nb.get("objective", ""),
        "timeline_length": len(nb.get("timeline", [])),
    }


def export_summary(bv_or_path: Any) -> dict:
    nb = load_notes(bv_or_path)
    fn_notes = nb.get("function_notes", {})

    # flatten function notes by kind
    fn_facts, fn_hypotheses = [], []
    for addr, notes in fn_notes.items():
        for n in notes:
            entry = {**n, "address": addr}
            if n["kind"] == "fact":
                fn_facts.append(entry)
            elif n["kind"] == "hypothesis":
                fn_hypotheses.append(entry)

    return {
        "binary_id": nb.get("binary_id"),
        "objective": nb.get("objective"),
        "facts": nb.get("facts", []) + fn_facts,
        "hypotheses": nb.get("hypotheses", []) + fn_hypotheses,
        "unknowns": nb.get("unknowns", []),
        "todos": nb.get("todos", []),
        "timeline": nb.get("timeline", []),
    }


def log_timeline(bv_or_path: Any, action: str, target: str, reason: str, outcome: str) -> dict:
    nb = load_notes(bv_or_path)
    step = len(nb.get("timeline", [])) + 1
    entry = {
        "step": step,
        "action": action,
        "target": target,
        "reason": reason,
        "outcome": outcome,
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    nb.setdefault("timeline", []).append(entry)
    save_notes(bv_or_path, nb)
    return entry


def set_objective(bv_or_path: Any, objective: str) -> dict:
    nb = load_notes(bv_or_path)
    nb["objective"] = objective
    save_notes(bv_or_path, nb)
    return {"ok": True, "objective": objective}
