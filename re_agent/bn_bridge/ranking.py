"""
Suspicion scoring for functions.

Used by both the live BN bridge (inspectors.list_functions) and the mock.
Input is the light summary dict produced by _fn_summary_light.
"""

from __future__ import annotations

import re

NET_IMPORTS = {"connect", "send", "recv", "internetopen", "internetconnect",
               "httpopen", "httpsend", "wsaconnect", "gethostbyname",
               "getaddrinfo", "winhttp", "curl_easy"}

CRYPTO_IMPORTS = {"cryptencrypt", "cryptdecrypt", "crypthashdata", "cryptderivekey",
                  "cryptunprotectdata", "bcrypt", "ncrypt", "sha", "md5", "rc4"}

CRED_STRINGS = {"login data", "password", "credential", "keychain",
                "sqlite3_open", "chrome", "firefox", "os_crypt"}

PERSIST_STRINGS = {"runonce", "startup", "appdata", "taskscheduler", "createservice"}

ANTI_IMPORTS = {"isdebuggerpresent", "checkremotedebugger", "ntsetinformationthread",
                "outputdebugstring", "gettickcount"}

REG_IMPORTS = {"regsetvalue", "regcreatekey", "regopenkey", "regqueryvalue"}


def score_function(fn: dict) -> dict:
    """
    Return { "suspicion": float, "reasons": [str] }.
    fn must have: imports_used, string_refs, callers, callees, size, basic_blocks, tags.
    """
    score = 0.0
    reasons = []

    imports_lower = {i.lower() for i in fn.get("imports_used", [])}
    strings_lower = {s.lower() for s in fn.get("string_refs", [])}

    # network imports: +3
    if any(ni in imp for imp in imports_lower for ni in NET_IMPORTS):
        score += 3.0
        reasons.append("network_import_hit")

    # crypto imports: +3
    if any(ci in imp for imp in imports_lower for ci in CRYPTO_IMPORTS):
        score += 3.0
        reasons.append("crypto_import_hit")

    # credential strings: +2
    if any(cs in s for s in strings_lower for cs in CRED_STRINGS):
        score += 2.0
        reasons.append("credential_string_hit")

    # persistence strings: +2
    if any(ps in s for s in strings_lower for ps in PERSIST_STRINGS):
        score += 2.0
        reasons.append("persistence_string_hit")

    # anti-debug/anti-analysis imports: +2
    if any(ai in imp for imp in imports_lower for ai in ANTI_IMPORTS):
        score += 2.0
        reasons.append("anti_debug_import")

    # registry imports: +1.5
    if any(ri in imp for imp in imports_lower for ri in REG_IMPORTS):
        score += 1.5
        reasons.append("registry_import")

    # high out-degree (many callees): +2
    callees = fn.get("callees", 0)
    if isinstance(callees, list):
        callees = len(callees)
    if callees >= 6:
        score += 2.0
        reasons.append("high_out_degree")

    # high in-degree (many callers = central): +1
    callers = fn.get("callers", 0)
    if isinstance(callers, list):
        callers = len(callers)
    if callers >= 3:
        score += 1.0
        reasons.append("high_centrality")

    # large function: +1
    if fn.get("size", 0) >= 400:
        score += 1.0
        reasons.append("large_function")

    # complex control flow: +0.5
    if fn.get("basic_blocks", 0) >= 15:
        score += 0.5
        reasons.append("complex_cfg")

    # tagged suspicious: +4
    if any("suspicious" in t or "malware" in t for t in fn.get("tags", [])):
        score += 4.0
        reasons.append("tagged_suspicious")

    # network + credential combo: bonus +1
    if "network_import_hit" in reasons and "credential_string_hit" in reasons:
        score += 1.0
        reasons.append("network_credential_combo")

    return {"suspicion": round(score, 2), "reasons": reasons}
