<!--
TITLES (pick one when publishing):
  · Blog/Twitter (recommended):  I reverse-engineered the 2 Solana prop AMMs nobody published
  · Gist / technical archive:    The Complete Solana Prop-AMM Mint Dictionary

OPEN TODOs before publishing:
  1. Fill in humidifi mint1/mint2 offsets   (run scripts/reverse-mint-offsets.py)
  2. Fill in aquifier mint1/mint2 offsets   (run scripts/reverse-mint-offsets.py)
  3. Replace docs/assets/radar-panel.png    (capture via visualization/terminal.html → RADAR drawer)
  4. Verify each new offset across 3+ pools
  5. Strip this comment block before publishing
-->

# I reverse-engineered the 2 Solana prop AMMs nobody published

*All 10 prop-AMM program IDs + every byte offset you need to enumerate their pools.*

> **TL;DR** — There are at least 10 closed-source proprietary AMMs (prop AMMs) live on Solana mainnet. None of them have IDLs. But because every Solana account is just bytes, you can decode all of them in ~150 lines of bash + 30 of Python. [@mubarizkyc](https://github.com/mubarizkyc) [published 8 of the 10 mint offsets in a gist](https://gist.github.com/mubarizkyc/959ac9b33dae4f3a86c6e00c331a9901); this post finishes the table.

## Why bother

Prop AMMs are where a lot of Solana's flow actually clears — quietly, off Jupiter's headline screen. If you're running a market-making bot, building a swap aggregator, doing competitor intel, or just trying to understand where SOL/USDC volume goes, the first question is always: *which pairs are they trading right now?*

That question reduces to:

1. List the prop-AMM program IDs.
2. For each, call `getProgramAccounts` with a `dataSize` filter to enumerate pool accounts.
3. For each pool, decode the two mint pubkeys at known byte offsets.

Step 1 and 3 are pure static knowledge — once published, anyone can reuse them. This is that publication.

## The 10 programs

| AMM       | Program ID                                        | Account size |
|-----------|---------------------------------------------------|-------------:|
| humidifi  | `9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp`    | 1728 |
| solfi     | `SoLFiHG9TfgtdUXUjWAxi3LtvYuFyDLVhBWxdMZxyCe`     | 2800 |
| solfiv2   | `SV2EYYJyRz2YhfXwXnhNAevDEui5Q6yrfyo13WtupPF`     | 1728 |
| zerofi    | `ZERor4xhbUycZ6gb9ntrhqscUcZmAbQDjEAtCf4hbZY`     | 7456 |
| bisonfi   | `BiSoNHVpsVZW2F7rx2eQ59yQwKxzU5NvBcmKshCSUypi`    | 2048 |
| obricv2   | `obriQD1zbpyLz95G5n7nJe6a4DPjpFwa5XYPoNm113y`     |  666 |
| goonfiv2  | `goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE`     | 2048 |
| aquifier  | `AQU1FRd7papthgdrwPTTq5JacJh8YtwEXaBfKU3bTz45`    | 1056 |
| alphaQ    | `ALPHAQmeA7bjrVuccPsYPiCvsi428SNwte66Srvs4pHA`    |  672 |
| tessera   | `TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH`     | 1264 |

## The complete mint offset table

Byte offsets into raw account data for `mint1` and `mint2` (each 32 bytes, raw pubkey, little-endian on chain).

| AMM       | `mint1` offset | `mint2` offset | Source |
|-----------|---:|---:|---|
| bisonfi   | 184  | 216  | mubarizkyc gist |
| tessera   | 56   | 24   | mubarizkyc gist |
| alphaQ    | 272  | 240  | mubarizkyc gist |
| solfi     | 2696 | 2664 | mubarizkyc gist |
| solfiv2   | 88   | 56   | mubarizkyc gist |
| goonfiv2  | 112  | 80   | mubarizkyc gist |
| zerofi    | 104  | 72   | mubarizkyc gist |
| obricv2   | 202  | 234  | mubarizkyc gist |
| **humidifi** | **`<TODO>`** | **`<TODO>`** | **this post — see below** |
| **aquifier** | **`<TODO>`** | **`<TODO>`** | **this post — see below** |

Drop-in JSON:

```json
{
  "humidifi": { "mint1": <TODO>, "mint2": <TODO> },
  "solfi":    { "mint1": 2696, "mint2": 2664 },
  "solfiv2":  { "mint1": 88,   "mint2": 56   },
  "zerofi":   { "mint1": 104,  "mint2": 72   },
  "bisonfi":  { "mint1": 184,  "mint2": 216  },
  "obricv2":  { "mint1": 202,  "mint2": 234  },
  "goonfiv2": { "mint1": 112,  "mint2": 80   },
  "aquifier": { "mint1": <TODO>, "mint2": <TODO> },
  "alphaQ":   { "mint1": 272,  "mint2": 240  },
  "tessera":  { "mint1": 56,   "mint2": 24   }
}
```

## How I recovered the missing two

mubarizkyc's gist left `humidifi` and `aquifier` unfilled. The recovery method is dead simple:

1. Pick a known pool from each — easiest is to call `getProgramAccounts` once with their program ID + account size, take the first result, look it up on Birdeye/Solscan to identify the mint pair.
2. base58-decode each mint into 32 raw bytes.
3. Search the pool's raw account data for both byte strings.
4. The position(s) where they appear *consistently across multiple pools* is the offset.

The script:

```python
# scripts/reverse-mint-offsets.py — abridged
import base64, base58, json, sys, urllib.request

def fetch(rpc, pool):
    req = json.dumps({"jsonrpc":"2.0","id":1,"method":"getAccountInfo",
                      "params":[pool,{"encoding":"base64"}]}).encode()
    r = urllib.request.urlopen(urllib.request.Request(
        rpc, data=req, headers={"content-type":"application/json"}))
    return base64.b64decode(json.loads(r.read())["result"]["value"]["data"][0])

def find_all(hay, needle):
    out, i = [], 0
    while True:
        j = hay.find(needle, i)
        if j == -1: return out
        out.append(j); i = j + 1

pool, m1, m2, rpc = sys.argv[1:5]
data = fetch(rpc, pool)
print("mint1 offsets:", find_all(data, base58.b58decode(m1)))
print("mint2 offsets:", find_all(data, base58.b58decode(m2)))
```

Verify across **3+ different pools per AMM** before trusting the offset — sometimes a mint coincidentally appears at a different location in one pool.

### What I actually saw

<!-- TODO: fill in after reversal -->

```
$ python3 scripts/reverse-mint-offsets.py \
    --pool <HUMIDIFI_POOL> \
    --mint1 <MINT_A> --mint2 <MINT_B> \
    --rpc-url "$HELIUS_RPC"

account size: 1728 bytes
mint1 (<MINT_A>) offsets: [<TODO>]
mint2 (<MINT_B>) offsets: [<TODO>]

Candidate layouts (verified across 3 pools):
  { "mint1": <TODO>, "mint2": <TODO> }
```

Same for `aquifier`:

```
$ python3 scripts/reverse-mint-offsets.py --pool <AQUIFIER_POOL> ...
account size: 1056 bytes
mint1 offsets: [<TODO>]
mint2 offsets: [<TODO>]
```

## RPC note

Public RPCs throttle `getProgramAccounts` hard — most return empty. You need one of:

- [Helius](https://helius.dev) (what I used)
- [Ironforge](https://www.ironforge.network/) (mubarizkyc's original choice; cheap)
- QuickNode / Triton

## What this unlocks

![Prop AMM Radar drawer — 10 AMMs, contested pairs, NEW-pair badges](assets/radar-panel.png)
<!-- TODO: replace placeholder with real screenshot of visualization/terminal.html → click RADAR -->

With the full offset table, you can build:

| What | Loop |
|---|---|
| **Competitive radar** | Scan every 30s → diff pairs vs yesterday → "humidifi just added BONK/SOL" |
| **Flow-aware fees** | Count how many AMMs are on a pair → fee goes up when 5+ pile in (concentrated toxic flow) |
| **Quote benchmarking** | For each pair you swap, see which prop AMM had the tightest quote |
| **TVL leaderboard** | Add reserve offsets (still TODO — same reversal trick, search for known u64 reserve amounts) |

Full reference implementation (Solana strategy in Rust, off-chain bot in TypeScript, dashboard in plain HTML) is here: <https://github.com/lilaclilac09/pamm-a>

## Credits

- Original gist + first 8 offsets: [@mubarizkyc](https://github.com/mubarizkyc) — <https://gist.github.com/mubarizkyc/959ac9b33dae4f3a86c6e00c331a9901>
- `humidifi` + `aquifier` reversal: this post
