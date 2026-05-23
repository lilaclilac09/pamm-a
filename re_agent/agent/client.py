"""
Claude-powered RE agent client.

Connects to the bridge server (real BN plugin or mock), exposes all bridge
endpoints as Claude tools, and runs a tool-calling loop until the agent
decides it's done or the turn budget is exhausted.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import anthropic
import requests

from .notebook import Notebook

MODEL = "claude-opus-4-7"
MAX_TURNS = 40


# ── tool definitions (sent to Claude) ────────────────────────────────────────

TOOLS: list[dict] = [
    {
        "name": "get_session",
        "description": "Return current binary analysis session: name, arch, platform, entry points, current function.",
        "input_schema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "get_overview",
        "description": (
            "High-level binary capability map. "
            "Pass include as a comma-separated list of: imports, sections, strings_stats, "
            "function_stats, capability_tags. Call this early to orient."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "include": {
                    "type": "string",
                    "description": "Comma-separated fields: imports,sections,strings_stats,function_stats,capability_tags",
                    "default": "imports,sections,strings_stats,function_stats,capability_tags",
                }
            },
            "required": [],
        },
    },
    {
        "name": "list_functions",
        "description": (
            "List functions sorted by suspicion score. "
            "Use filter.name_contains, filter.has_import, filter.referencing_string to narrow. "
            "Returns score.suspicion and score.reasons for each function."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "filter": {
                    "type": "object",
                    "properties": {
                        "name_contains": {"type": "string"},
                        "min_size": {"type": "integer"},
                        "max_size": {"type": "integer"},
                        "has_import": {"type": "string"},
                        "referencing_string": {"type": "string"},
                        "tag": {"type": "string"},
                    },
                },
                "sort": {
                    "type": "object",
                    "properties": {
                        "by": {"type": "string", "enum": ["suspicion_score", "size", "callers"]},
                        "order": {"type": "string", "enum": ["desc", "asc"]},
                    },
                },
                "limit": {"type": "integer", "default": 20},
                "cursor": {"type": "integer", "default": 0},
            },
            "required": [],
        },
    },
    {
        "name": "search_strings",
        "description": "Search all strings in the binary by keyword or regex.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "regex": {"type": "boolean", "default": False},
                "case_sensitive": {"type": "boolean", "default": False},
                "limit": {"type": "integer", "default": 30},
            },
            "required": ["query"],
        },
    },
    {
        "name": "search_imports",
        "description": "Search imported symbols by name/module keyword or regex.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Keyword or regex, e.g. 'crypt|net|sqlite'"},
                "regex": {"type": "boolean", "default": True},
            },
            "required": ["query"],
        },
    },
    {
        "name": "find_references",
        "description": (
            "Find all code references to a string value, import name, or address. "
            "target.type: 'string' | 'import' | 'address' | 'symbol'"
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "target": {
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "enum": ["string", "import", "address", "symbol"]},
                        "value": {"type": "string"},
                    },
                    "required": ["type", "value"],
                },
                "limit": {"type": "integer", "default": 20},
            },
            "required": ["target"],
        },
    },
    {
        "name": "get_function_summary",
        "description": (
            "Full metadata for one function: signature, stack vars, constants, "
            "string refs, callers, callees, imports used. "
            "Call this before decompile to decide if deeper inspection is warranted."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "function": {"type": "string", "description": "Hex address, e.g. '0x407820'"},
            },
            "required": ["function"],
        },
    },
    {
        "name": "decompile_function",
        "description": (
            "Decompile / disassemble one function. "
            "view: 'hlil' (default, pseudocode) | 'mlil' | 'llil' | 'disasm'. "
            "Use mlil/llil when hlil misleads. Always respect max_lines to avoid context explosion."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "function": {"type": "string", "description": "Hex address"},
                "view": {"type": "string", "enum": ["hlil", "mlil", "llil", "disasm"], "default": "hlil"},
                "max_lines": {"type": "integer", "default": 120},
            },
            "required": ["function"],
        },
    },
    {
        "name": "get_callers",
        "description": "List functions that call the given function.",
        "input_schema": {
            "type": "object",
            "properties": {
                "function": {"type": "string"},
                "limit": {"type": "integer", "default": 20},
            },
            "required": ["function"],
        },
    },
    {
        "name": "get_callees",
        "description": "List functions called by the given function.",
        "input_schema": {
            "type": "object",
            "properties": {
                "function": {"type": "string"},
                "limit": {"type": "integer", "default": 20},
            },
            "required": ["function"],
        },
    },
    {
        "name": "get_xrefs",
        "description": "All code/data references to a given address.",
        "input_schema": {
            "type": "object",
            "properties": {
                "address": {"type": "string"},
                "limit": {"type": "integer", "default": 20},
            },
            "required": ["address"],
        },
    },
    {
        "name": "rank_functions",
        "description": (
            "Return the top N highest-suspicion functions for a given objective. "
            "More targeted than list_functions — use when you know the goal."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "objective": {"type": "string", "description": "e.g. 'find config decode', 'find c2 comms'"},
                "limit": {"type": "integer", "default": 10},
            },
            "required": ["objective"],
        },
    },
    {
        "name": "write_note",
        "description": (
            "Record a finding in the notebook. "
            "kind: 'fact' | 'hypothesis' | 'unknown' | 'todo'. "
            "Use scope='function' + subject=address to attach to a specific function. "
            "Do NOT postpone — write notes immediately after every finding."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["fact", "hypothesis", "unknown", "todo"]},
                "text": {"type": "string"},
                "subject": {"type": "string", "description": "Function address or binary ID"},
                "scope": {"type": "string", "enum": ["binary", "function"], "default": "binary"},
                "evidence": {"type": "array", "items": {"type": "string"}},
                "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                "next_checks": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["kind", "text"],
        },
    },
    {
        "name": "rename_symbol",
        "description": (
            "Rename a function or symbol. Always use preview=true first. "
            "Naming discipline: 'maybe_' for weak evidence, 'likely_' for medium, "
            "clear semantic name only when confident."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "address": {"type": "string"},
                "new_name": {"type": "string"},
                "preview": {"type": "boolean", "default": True},
            },
            "required": ["address", "new_name"],
        },
    },
    {
        "name": "set_comment",
        "description": "Set a comment at an address. preview=true by default.",
        "input_schema": {
            "type": "object",
            "properties": {
                "address": {"type": "string"},
                "text": {"type": "string"},
                "preview": {"type": "boolean", "default": True},
            },
            "required": ["address", "text"],
        },
    },
    {
        "name": "tag_function",
        "description": "Tag a function with a label. preview=true by default.",
        "input_schema": {
            "type": "object",
            "properties": {
                "function": {"type": "string"},
                "tag": {"type": "string"},
                "preview": {"type": "boolean", "default": True},
            },
            "required": ["function", "tag"],
        },
    },
    {
        "name": "patch_preview",
        "description": "Preview a binary patch without applying it. Returns diff of original vs patched bytes.",
        "input_schema": {
            "type": "object",
            "properties": {
                "address": {"type": "string"},
                "patch": {"type": "string", "description": "Hex bytes to write, e.g. '9090'"},
            },
            "required": ["address", "patch"],
        },
    },
]


# ── bridge calls ──────────────────────────────────────────────────────────────

class BridgeClient:
    def __init__(self, base_url: str):
        self.base = base_url.rstrip("/")

    def get(self, path: str, params: dict | None = None) -> Any:
        r = requests.get(f"{self.base}{path}", params=params, timeout=15)
        r.raise_for_status()
        result = r.json()
        if not result.get("ok"):
            return {"error": result.get("error", {})}
        return result.get("data")

    def post(self, path: str, body: dict) -> Any:
        r = requests.post(f"{self.base}{path}", json=body, timeout=15)
        r.raise_for_status()
        result = r.json()
        if not result.get("ok"):
            return {"error": result.get("error", {})}
        return result.get("data")

    def dispatch(self, tool_name: str, tool_input: dict) -> Any:
        include = tool_input.get("include", "imports,sections,strings_stats,function_stats,capability_tags")
        if isinstance(include, str):
            include_list = [s.strip() for s in include.split(",")]
        else:
            include_list = include

        routes = {
            "get_session":           lambda i: self.get("/session"),
            "get_overview":          lambda i: self.get("/overview",
                                        {"include": ",".join(include_list)}),
            "list_functions":        lambda i: self.post("/functions", i),
            "search_strings":        lambda i: self.post("/strings/search", i),
            "search_imports":        lambda i: self.post("/imports/search", i),
            "find_references":       lambda i: self.post("/references", i),
            "get_function_summary":  lambda i: self.post("/function/summary", i),
            "decompile_function":    lambda i: self.post("/function/decompile", i),
            "get_callers":           lambda i: self.post("/function/callers", i),
            "get_callees":           lambda i: self.post("/function/callees", i),
            "get_xrefs":             lambda i: self.post("/xrefs", i),
            "rank_functions":        lambda i: self.post("/functions/rank", i),
            "write_note":            lambda i: self.post("/notes/write", i),
            "rename_symbol":         lambda i: self.post("/rename", i),
            "set_comment":           lambda i: self.post("/comment", i),
            "tag_function":          lambda i: self.post("/tag", i),
            "patch_preview":         lambda i: self.post("/patch/preview", i),
        }

        handler = routes.get(tool_name)
        if not handler:
            return {"error": f"unknown tool: {tool_name}"}
        try:
            return handler(tool_input)
        except requests.RequestException as exc:
            return {"error": str(exc)}


# ── agent loop ────────────────────────────────────────────────────────────────

class REAgent:
    def __init__(
        self,
        bridge_url: str,
        system_prompt: str,
        verbose: bool = True,
    ):
        self.bridge = BridgeClient(bridge_url)
        self.notebook = Notebook(bridge_url)
        self.system = system_prompt
        self.verbose = verbose
        self.client = anthropic.Anthropic()
        self.messages: list[dict] = []
        self._turn = 0

    def set_objective(self, objective: str) -> None:
        self.notebook.set_objective(objective)

    def run(self, user_message: str, max_turns: int = MAX_TURNS) -> str:
        """
        Run the agent loop with an initial user message.
        Returns the final text response.
        """
        self.messages.append({"role": "user", "content": user_message})

        while self._turn < max_turns:
            self._turn += 1

            if self.verbose:
                print(f"\n{'─'*60}", flush=True)
                print(f"[turn {self._turn}]", flush=True)

            # inject notebook summary into system for this call
            system_with_nb = self.system + "\n\n--- CURRENT NOTEBOOK ---\n" + self.notebook.summary_text()

            response = self.client.messages.create(
                model=MODEL,
                max_tokens=4096,
                system=system_with_nb,
                tools=TOOLS,
                messages=self.messages,
            )

            # collect assistant message
            assistant_content = []
            final_text = ""

            for block in response.content:
                if block.type == "text":
                    final_text = block.text
                    if self.verbose:
                        print(f"\n[agent]\n{block.text}", flush=True)
                    assistant_content.append({"type": "text", "text": block.text})

                elif block.type == "tool_use":
                    if self.verbose:
                        print(f"\n[tool] {block.name}({json.dumps(block.input, indent=2)[:200]})", flush=True)
                    assistant_content.append({
                        "type": "tool_use",
                        "id": block.id,
                        "name": block.name,
                        "input": block.input,
                    })

            self.messages.append({"role": "assistant", "content": assistant_content})

            # if no tool calls, agent is done
            if response.stop_reason == "end_turn":
                return final_text

            # execute tool calls and return results
            tool_results = []
            for block in response.content:
                if block.type != "tool_use":
                    continue

                result = self.bridge.dispatch(block.name, block.input)
                result_str = json.dumps(result, indent=2)

                if self.verbose:
                    preview = result_str[:400] + ("..." if len(result_str) > 400 else "")
                    print(f"\n[tool result]\n{preview}", flush=True)

                tool_results.append({
                    "type": "tool_result",
                    "tool_use_id": block.id,
                    "content": result_str,
                })

            self.messages.append({"role": "user", "content": tool_results})

        return f"[agent] turn budget ({max_turns}) exhausted."

    def reset(self) -> None:
        self.messages = []
        self._turn = 0
