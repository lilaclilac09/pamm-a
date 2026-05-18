"""
Notebook manager for the agent client side.

Talks to the bridge server's /notes/* endpoints.
Also maintains a local in-memory mirror so the agent can read
its own prior entries without a round-trip.
"""

from __future__ import annotations

import json
from typing import Any

import requests


class Notebook:
    def __init__(self, bridge_url: str):
        self.base = bridge_url.rstrip("/")
        self._mirror: dict[str, Any] = {
            "facts": [],
            "hypotheses": [],
            "unknowns": [],
            "todos": [],
            "timeline": [],
            "function_notes": {},
            "objective": "",
        }

    # ── writes ────────────────────────────────────────────────────────────────

    def write(
        self,
        kind: str,
        text: str,
        subject: str | None = None,
        scope: str = "binary",
        evidence: list[str] | None = None,
        confidence: float | None = None,
        next_checks: list[str] | None = None,
    ) -> dict:
        payload: dict[str, Any] = {
            "kind": kind,
            "scope": scope,
            "subject": subject or "binary",
            "text": text,
        }
        if evidence:
            payload["evidence"] = evidence
        if confidence is not None:
            payload["confidence"] = confidence
        if next_checks:
            payload["next_checks"] = next_checks

        resp = requests.post(f"{self.base}/notes/write", json=payload, timeout=10)
        result = resp.json()

        # mirror update
        note = payload.copy()
        if scope == "function" and subject:
            self._mirror["function_notes"].setdefault(subject, []).append(note)
        else:
            self._mirror.setdefault(kind + "s", []).append(note)

        return result

    def fact(self, text: str, subject: str | None = None,
             evidence: list[str] | None = None, confidence: float = 1.0) -> dict:
        return self.write("fact", text, subject=subject,
                          evidence=evidence, confidence=confidence)

    def hypothesis(self, text: str, subject: str | None = None,
                   evidence: list[str] | None = None, confidence: float = 0.7,
                   next_checks: list[str] | None = None) -> dict:
        return self.write("hypothesis", text, subject=subject,
                          evidence=evidence, confidence=confidence,
                          next_checks=next_checks)

    def unknown(self, text: str, subject: str | None = None) -> dict:
        return self.write("unknown", text, subject=subject)

    def todo(self, text: str, subject: str | None = None) -> dict:
        return self.write("todo", text, subject=subject)

    def fn_fact(self, fn_addr: str, text: str,
                evidence: list[str] | None = None, confidence: float = 1.0) -> dict:
        return self.write("fact", text, subject=fn_addr, scope="function",
                          evidence=evidence, confidence=confidence)

    def fn_hypothesis(self, fn_addr: str, text: str,
                      evidence: list[str] | None = None, confidence: float = 0.7,
                      next_checks: list[str] | None = None) -> dict:
        return self.write("hypothesis", text, subject=fn_addr, scope="function",
                          evidence=evidence, confidence=confidence,
                          next_checks=next_checks)

    def set_objective(self, objective: str) -> dict:
        self._mirror["objective"] = objective
        resp = requests.post(f"{self.base}/notes/objective",
                             json={"objective": objective}, timeout=10)
        return resp.json()

    # ── reads ─────────────────────────────────────────────────────────────────

    def get_all(self) -> dict:
        resp = requests.get(f"{self.base}/notes", timeout=10)
        data = resp.json().get("data", {})
        self._mirror.update(data)
        return data

    def export(self) -> dict:
        resp = requests.get(f"{self.base}/notes/export", timeout=10)
        return resp.json().get("data", {})

    # ── local mirror accessors ────────────────────────────────────────────────

    @property
    def facts(self) -> list[dict]:
        return self._mirror.get("facts", [])

    @property
    def hypotheses(self) -> list[dict]:
        return self._mirror.get("hypotheses", [])

    @property
    def unknowns(self) -> list[dict]:
        return self._mirror.get("unknowns", [])

    @property
    def objective(self) -> str:
        return self._mirror.get("objective", "")

    def function_notes(self, fn_addr: str) -> list[dict]:
        return self._mirror.get("function_notes", {}).get(fn_addr, [])

    def summary_text(self) -> str:
        """Compact text summary injected into agent context each turn."""
        lines = [f"OBJECTIVE: {self.objective or '(not set)'}"]
        lines.append(f"FACTS ({len(self.facts)}):")
        for f in self.facts[-5:]:
            lines.append(f"  [fact] {f['text']}")
        lines.append(f"HYPOTHESES ({len(self.hypotheses)}):")
        for h in self.hypotheses[-5:]:
            conf = h.get("confidence", "?")
            lines.append(f"  [hyp:{conf}] {h['text']}")
        if self.unknowns:
            lines.append(f"UNKNOWNS ({len(self.unknowns)}):")
            for u in self.unknowns[-3:]:
                lines.append(f"  [?] {u['text']}")
        return "\n".join(lines)
