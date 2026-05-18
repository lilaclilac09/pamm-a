#!/usr/bin/env python3
"""
RE Agent entry point.

Usage examples:

  # Test with mock (no BN needed)
  python run_agent.py --mock

  # Connect to a live BN bridge
  python run_agent.py --bridge http://127.0.0.1:7734

  # Run a single-objective investigation
  python run_agent.py --mock --objective "find the C2 communication path"

  # Run specific triage phases only
  python run_agent.py --mock --phases 1,2,3

  # Skip phases, run a one-shot question
  python run_agent.py --bridge http://127.0.0.1:7734 --oneshot \
    "Determine whether this binary decrypts an embedded config before network comms"
"""

from __future__ import annotations

import argparse
import os
import sys
import threading
from pathlib import Path


def _load_prompt(name: str) -> str:
    p = Path(__file__).parent / "prompts" / name
    if p.exists():
        return p.read_text()
    return ""


def _start_mock(port: int = 7734) -> None:
    from re_agent.bn_bridge.mock import run as mock_run
    t = threading.Thread(target=mock_run, args=(port,), daemon=True)
    t.start()
    import time
    time.sleep(0.3)


def main() -> None:
    parser = argparse.ArgumentParser(description="RE Agent — Binary Ninja analysis copilot")
    parser.add_argument("--bridge", default="http://127.0.0.1:7734",
                        help="Bridge server URL (default: http://127.0.0.1:7734)")
    parser.add_argument("--mock", action="store_true",
                        help="Start the mock server (no BN required)")
    parser.add_argument("--objective", default="",
                        help="Investigation objective (default: full malware triage)")
    parser.add_argument("--phases", default="1,2,3,4,5",
                        help="Comma-separated triage phases to run (default: 1,2,3,4,5)")
    parser.add_argument("--oneshot", metavar="QUESTION",
                        help="Run a single-objective investigation instead of full triage")
    parser.add_argument("--max-turns", type=int, default=12,
                        help="Max tool-calling turns per phase (default: 12)")
    parser.add_argument("--quiet", action="store_true",
                        help="Suppress per-turn output, show final report only")
    args = parser.parse_args()

    # start mock if requested
    if args.mock:
        print("[*] starting mock bridge server...")
        _start_mock()

    # load prompts
    system_base = _load_prompt("system.txt")
    triage_extra = _load_prompt("malware_triage.txt")
    system_prompt = system_base
    if triage_extra:
        system_prompt += "\n\n" + triage_extra

    if not system_prompt.strip():
        print("[!] no system prompt found — prompts/system.txt missing?", file=sys.stderr)
        sys.exit(1)

    # check API key
    if not os.environ.get("ANTHROPIC_API_KEY"):
        print("[!] ANTHROPIC_API_KEY not set", file=sys.stderr)
        sys.exit(1)

    from re_agent.agent.client import REAgent
    from re_agent.agent.loops.triage import run_triage, run_single_objective

    agent = REAgent(
        bridge_url=args.bridge,
        system_prompt=system_prompt,
        verbose=not args.quiet,
    )

    if args.oneshot:
        print(f"\n[*] one-shot investigation: {args.oneshot}\n")
        result = run_single_objective(agent, args.oneshot, max_turns=args.max_turns * 2)
        print("\n" + "="*60)
        print("RESULT")
        print("="*60)
        print(result)

    else:
        objective = args.objective or "Determine what this binary does and classify its threat category."
        phases = [int(p) for p in args.phases.split(",")]
        report = run_triage(
            agent,
            objective=objective,
            phases=phases,
            max_turns_per_phase=args.max_turns,
        )
        if args.quiet and report:
            print("\n" + "="*60)
            print("TRIAGE REPORT")
            print("="*60)
            print(report)


if __name__ == "__main__":
    main()
