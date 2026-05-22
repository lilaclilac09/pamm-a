#!/usr/bin/env bash
# scan-prop-amm.sh — enumerate active pools across 10 Solana prop AMMs.
#
# Reads metadata from references/solana/prop-amm/programs.json + account-layouts.json.
# Writes a single JSON snapshot to references/solana/prop-amm/snapshots/active-markets-<UTC>.json
# matching the shape sample-active-markets.json — so visualization/terminal.html and
# downstream consumers can read either interchangeably.
#
# Requires: bash 4+, jq, curl, python3 (with base58 installed: pip install base58).
# RPC: set IRONFORGE_KEY (or override RPC_URL directly) — public RPCs reject getProgramAccounts.
#
# Derived from mubarizkyc's prop_assets_today.sh
#   https://gist.github.com/mubarizkyc/959ac9b33dae4f3a86c6e00c331a9901

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INTEL="$ROOT/references/solana/prop-amm"
OUT_DIR="$INTEL/snapshots"
mkdir -p "$OUT_DIR"

RPC_URL="${RPC_URL:-https://rpc.ironforge.network/mainnet?apiKey=${IRONFORGE_KEY:-}}"
if [[ "$RPC_URL" == *"apiKey="*"" && "${IRONFORGE_KEY:-}" == "" ]]; then
  echo "error: set IRONFORGE_KEY or RPC_URL." >&2
  exit 1
fi

ACTIVE_WINDOW_SLOTS="${ACTIVE_WINDOW_SLOTS:-216000}"   # ~24h at 0.4s/slot
MAX_PARALLEL="${MAX_PARALLEL:-150}"
BATCH_SIZE="${BATCH_SIZE:-100}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_FILE="$OUT_DIR/active-markets-$STAMP.json"
LATEST_LINK="$OUT_DIR/active-markets-latest.json"

log() { printf '[scan] %s\n' "$*" >&2; }

# ── 1. current slot ─────────────────────────────────────────────────────────
log "fetching current slot…"
CURRENT_SLOT=$(curl -s "$RPC_URL" \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' | jq -r '.result')

# ── 2. for each AMM, getProgramAccounts(dataSize) → list of pool pubkeys ────
PROGRAMS_JSON="$INTEL/programs.json"
LAYOUTS_JSON="$INTEL/account-layouts.json"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

scan_one_amm() {
  local name="$1" pid="$2" size="$3"
  local accounts_file="$TMP/$name.accounts.json"
  log "  $name (pid=$pid, size=$size) — getProgramAccounts"
  curl -s "$RPC_URL" -H 'content-type: application/json' -d @- > "$accounts_file" <<EOF
{"jsonrpc":"2.0","id":1,"method":"getProgramAccounts","params":[
  "$pid",
  { "encoding":"base64",
    "filters":[{"dataSize":$size}],
    "withContext":false
  }
]}
EOF
  if jq -e '.error' "$accounts_file" >/dev/null 2>&1; then
    log "    ERR $(jq -r '.error.message // .error' "$accounts_file")"
    echo '[]' > "$TMP/$name.pools.json"
    return
  fi
  jq -c '.result[] | {pubkey: .pubkey, data: .account.data[0]}' "$accounts_file" > "$TMP/$name.pools.ndjson" || true
}

mapfile -t AMM_LINES < <(jq -c '.programs[]' "$PROGRAMS_JSON")
log "scanning ${#AMM_LINES[@]} AMMs…"

active=0
for line in "${AMM_LINES[@]}"; do
  name=$(jq -r '.name'        <<<"$line")
  pid=$(jq -r '.program_id'   <<<"$line")
  size=$(jq -r '.account_size' <<<"$line")
  scan_one_amm "$name" "$pid" "$size" &
  active=$((active+1))
  if (( active >= MAX_PARALLEL )); then wait -n; active=$((active-1)); fi
done
wait

# ── 3. activity filter: signature lookup → recent slot? ─────────────────────
# (Optional: signatures-for-address is rate-heavy; we trust the dataSize match
# as "exists" and let the dashboard mark which mints are seen elsewhere.
# To filter by 24h activity strictly, uncomment the block below.)
#
# for f in "$TMP"/*.pools.ndjson; do
#   while read -r row; do
#     pk=$(jq -r '.pubkey' <<<"$row")
#     last_slot=$(curl -s "$RPC_URL" -H 'content-type: application/json' \
#       -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getSignaturesForAddress\",\"params\":[\"$pk\",{\"limit\":1}]}" \
#       | jq -r '.result[0].slot // 0')
#     age=$(( CURRENT_SLOT - last_slot ))
#     if (( age < ACTIVE_WINDOW_SLOTS )); then echo "$row"; fi
#   done < "$f" > "$f.active"
#   mv "$f.active" "$f"
# done

# ── 4. extract mints via offsets ────────────────────────────────────────────
log "decoding mints via offset table…"
python3 - "$LAYOUTS_JSON" "$TMP" <<'PYEOF' > "$TMP/by_amm.json"
import base64, base58, json, os, sys, glob

layouts = json.load(open(sys.argv[1]))["layouts"]
tmpdir  = sys.argv[2]

out = {}
for path in glob.glob(os.path.join(tmpdir, "*.pools.ndjson")):
    name = os.path.basename(path).split(".")[0]
    layout = layouts.get(name)
    pools = []
    if layout is None:
        # mints offset unknown — still record pool existence with null mints
        with open(path) as f:
            for line in f:
                try: o = json.loads(line)
                except: continue
                pools.append({"pubkey": o["pubkey"], "mint1": None, "mint2": None})
    else:
        m1, m2 = layout["mint1"], layout["mint2"]
        with open(path) as f:
            for line in f:
                try: o = json.loads(line)
                except: continue
                try:
                    raw = base64.b64decode(o["data"])
                    mint1 = base58.b58encode(raw[m1:m1+32]).decode()
                    mint2 = base58.b58encode(raw[m2:m2+32]).decode()
                    pools.append({"pubkey": o["pubkey"], "mint1": mint1, "mint2": mint2})
                except Exception:
                    continue
    out[name] = pools

print(json.dumps(out))
PYEOF

# ── 5. emit final JSON ──────────────────────────────────────────────────────
jq -n \
  --arg gen "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson slot "$CURRENT_SLOT" \
  --argjson win "$ACTIVE_WINDOW_SLOTS" \
  --slurpfile by_amm "$TMP/by_amm.json" \
  '{ generated_at: $gen, current_slot: $slot, active_window_slots: $win, by_amm: $by_amm[0] }' \
  > "$OUT_FILE"

ln -sf "$(basename "$OUT_FILE")" "$LATEST_LINK"

total=$(jq '[.by_amm[] | length] | add' "$OUT_FILE")
log "wrote $OUT_FILE ($total pools across ${#AMM_LINES[@]} AMMs)"
log "symlinked $LATEST_LINK"
