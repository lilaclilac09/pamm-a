"""
Malware triage loop — 5-phase static analysis workflow.

Phase 1: Entry scan     — imports, strings, sections, capability tags
Phase 2: Clustering     — group functions by capability
Phase 3: Path building  — reconstruct main behavior chains
Phase 4: Verification   — cross-check hypotheses with xrefs + lower IL
Phase 5: Conclusion     — structured report

Each phase issues a targeted message to the agent and waits for tool_stop.
"""

from __future__ import annotations

import json
import textwrap

from ..client import REAgent


PHASE_PROMPTS = {
    1: """\
PHASE 1 — ENTRY SCAN

Inspect the binary's overall capability profile.

Steps:
1. Call get_session to confirm what we're looking at.
2. Call get_overview with ALL fields: imports, sections, strings_stats, function_stats, capability_tags.
3. Identify capability clusters from imports: network / crypto / credential / registry / persistence / anti_debug.
4. Note any packing or obfuscation indicators (high entropy sections, tiny import table, self-extracting strings).
5. Write one note per cluster you identify (use kind='fact' for confirmed imports, kind='hypothesis' for suspected purpose).

Output your response in the standard format:
  OBJECTIVE:
  OBSERVATIONS:
  FACTS:
  HYPOTHESES:
  UNKNOWNS:
  NEXT_ACTION:
""",

    2: """\
PHASE 2 — FUNCTION CLUSTERING

Build a map of the most interesting functions before deep-diving any of them.

Steps:
1. Call rank_functions with the most relevant objective based on Phase 1 findings.
2. For the top 5 functions, call get_function_summary (not decompile yet — summary is enough).
3. Group them into clusters: init / config / crypto / network / persistence / anti_analysis / execution.
4. Write a hypothesis note for each suspected cluster.
5. Identify your top-priority inspection target (highest score + clearest cluster signal).

Output:
  OBJECTIVE:
  OBSERVATIONS:
  TOP CLUSTERS:
  PRIORITY TARGET:
  NEXT_ACTION:
""",

    3: """\
PHASE 3 — MAIN PATH RECONSTRUCTION

Deep-inspect the top-priority function and trace the main behavior chain.

Steps:
1. Call get_function_summary on the priority target.
2. Call decompile_function(view='hlil', max_lines=120).
3. If pseudocode is unclear or indirect calls dominate, retry with view='mlil'.
4. Pull callers and callees for the function.
5. Identify the next node in the chain (e.g., if this collects data, find who receives it).
6. Write facts for confirmed behaviors, hypotheses for suspected ones.
7. Use provisional naming: rename_symbol with 'maybe_' or 'likely_' prefix, preview=true.

Output:
  OBJECTIVE:
  CURRENT FUNCTION:
  BEHAVIOR:
  FACTS:
  HYPOTHESES:
  CHAIN SO FAR:
  NEXT_ACTION:
""",

    4: """\
PHASE 4 — HYPOTHESIS VERIFICATION

Validate the top hypotheses with cross-references and lower-level IL.

For each high-confidence hypothesis:
1. Use find_references to confirm that key strings/imports lead where you expect.
2. For functions with indirect calls or confusing pseudocode, retry decompile with view='mlil' or 'llil'.
3. Check constants for telltale values (port numbers, magic bytes, XOR keys).
4. Confirm or refute each hypothesis — update kind from 'hypothesis' to 'fact' via write_note.
5. Identify what CANNOT be resolved statically (needs dynamic analysis, unpacking, or network capture).

Output:
  OBJECTIVE:
  VERIFIED FACTS:
  REFUTED HYPOTHESES:
  REMAINING UNKNOWNS:
  WHAT NEEDS DYNAMIC ANALYSIS:
  NEXT_ACTION:
""",

    5: """\
PHASE 5 — STRUCTURED CONCLUSION

Produce the final triage report.

Write a concluding write_note with kind='fact' that summarizes the binary's behavior.
Then respond in this exact format:

TRIAGE REPORT
=============

BINARY: <name>
LIKELY TYPE: <dropper / stealer / loader / rat / ransomware / other>

CONFIRMED BEHAVIOR:
  • <behavior 1 with evidence>
  • <behavior 2 with evidence>
  ...

KEY FUNCTIONS:
  • <address> <name> — <role>
  ...

UNCERTAINTY:
  • <what is still unclear and why>
  ...

NEXT BEST ACTIONS:
  • <dynamic analysis step / dump target / hook point>
  ...

Do not write vague summaries. Every claim needs at least one piece of evidence.
""",
}


def run_triage(
    agent: REAgent,
    objective: str = "Determine what this binary does and classify its threat category.",
    phases: list[int] | None = None,
    max_turns_per_phase: int = 12,
) -> str:
    phases = phases or [1, 2, 3, 4, 5]

    agent.set_objective(objective)
    print(f"\n{'='*60}")
    print(f"RE AGENT — MALWARE TRIAGE")
    print(f"Objective: {objective}")
    print(f"{'='*60}\n")

    final_report = ""

    for phase_num in phases:
        print(f"\n{'━'*60}")
        print(f"PHASE {phase_num}")
        print(f"{'━'*60}")

        prompt = PHASE_PROMPTS[phase_num]
        result = agent.run(prompt, max_turns=max_turns_per_phase)

        if phase_num == 5:
            final_report = result

        # brief pause between phases so the user can read output
        print(f"\n[phase {phase_num} complete]")

    return final_report


def run_single_objective(
    agent: REAgent,
    objective: str,
    max_turns: int = 20,
) -> str:
    """
    One-shot investigation for a specific question.
    E.g.: "Determine whether this binary decrypts an embedded config before network comms."
    """
    agent.set_objective(objective)

    prompt = textwrap.dedent(f"""
    INVESTIGATION OBJECTIVE: {objective}

    Work step by step:
    1. Inspect the binary overview if not already done.
    2. Identify candidate functions using rank_functions or search_strings/search_imports.
    3. Inspect the top candidate with get_function_summary then decompile.
    4. Follow callers/callees to build the relevant code path.
    5. Write notes as you go — distinguish fact from hypothesis.
    6. Stop when the objective is answered with evidence, or when
       further resolution requires dynamic analysis.

    Respond in the structured format:
      OBJECTIVE:
      OBSERVATIONS:
      FACTS:
      HYPOTHESES:
      UNKNOWNS:
      CONCLUSION:
      NEXT_ACTION (if any):
    """).strip()

    return agent.run(prompt, max_turns=max_turns)
