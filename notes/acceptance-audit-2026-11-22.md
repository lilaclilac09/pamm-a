# Acceptance audit — 2026-11-22

Acceptance criteria come only from this repository's [ACCEPTANCE.md](../ACCEPTANCE.md). Final audit; simulated/local evidence is not deployed trading acceptance.

## Findings and disposition

| Item | Source rule | Baseline evidence | This PR / remaining limitation |
|---|---|---|---|
| Historical PRs #1–#6 lack closers | §8 | Current checker rejects those bodies; #7 passes | Not fixable after merge by this branch. New PR supplies the complete closer. |
| PRs #3–#5 lack required mobile interaction artifacts | §§2,4 | Textual checklists and a desktop still do not deliver 390×844 plus interaction proof | Fixed prospectively: five current page stills and simulated terminal animation covering #radar, drawer, pin, sort and resume. Historical merge-time evidence remains incomplete. |
| PR #6 research lacks delivered trace/decoder proof | §2 | Unchecked reproduction plan accompanies numeric/replication findings | Fixed as truthful qualification in scripts README and research document: claims are explicitly unverified. Research itself remains unverified; no paid RPC run. |
| Root Cargo instructions use nonexistent root package | §6 | No root Cargo.toml | Fixed: real program/framework directories, reference-only sources separated; SBF prerequisite documented. |
| Missing product-status lines / unverified live URL | §5 | READMEs lacked required line; advertised host failed local DNS resolution | Fixed: root and product/research sub-READMEs use LOCAL ONLY plus next action. Visualization URL qualified as unverified, not globally down. |
| Requested 390px interaction blocked by clipped panels | §§2,4 | Ordinary Playwright row click could not hit fills; fixed-width grids exceeded viewport under hidden overflow. Geometry showed exec chart height 1364px inside 281px panel and zero-height fills viewport | Fixed minimally: narrow viewports can pan the unchanged desktop layout; chart flex item gets min-height:0 so fills remain clickable. No redesigned dashboard or new feature. |

## Existing work

Baseline `5edcc6cc27ea0820c6a1c35cf1b8d5e12f70e0c4`. Returned PRs #1–#7 were merged; each merge SHA passed ancestry check against origin/main. Work includes source/Anchor/Quasar/Pinocchio research implementations, bots, Binary Ninja/Claude RE tooling, execution terminal, RADAR, EWMA and Humidifi research utilities. Reviewed root README, product sub-READMEs, testing guide, visualization sources and research notes; framework and credentialed agent functionality not exhaustively validated. GitHub deployments empty; Pages HTTP 404. Neither Vercel rewrite configuration nor a localhost run proves hosting.

## Current verification

- In `pinocchio-prop-amm/programs/pinocchio-prop-amm`, `cargo test` compiled but failed: `test sim_market_30_rounds ... FAILED`; `test sim_parameter_combos ... FAILED`; `test result: FAILED. 0 passed; 2 failed`. Log: `/home/ubuntu/audits/pamm-a-cargo-test.log`.
- First panic at `tests/sim_market.rs:294` is `InvalidPath(Os { code: 2, kind: NotFound, message: "No such file or directory" })`; second at line 635 is `InvalidProgramForExecution`. Both test paths call `add_program_from_file(..., "target/deploy/pinocchio_prop_amm.so")`; no such artifact/deploy directory exists. The first unwrap exposes the missing file; the second ignores add-program failure before attempting execution. SBF build tool is absent; host compilation does not create this artifact. No AMM test success claimed and no binary committed.
- `curl -IL --max-time 20 https://pamm.aileena.xyz`: `curl: (6) Could not resolve host: pamm.aileena.xyz`. This VM result is not proof of a global outage.
- Playwright Chromium at 390×844: still per `visualization/{terminal,risk-terminal,sim-viz,dashboard}.html` and `pinocchio-prop-amm/bot/dashboard.html`. Simulated terminal flow: #radar opens → close → click fill/pin → sort mk5 → resume LIVE → reopen drawer. Ordinary clicks, no forced clicks/DOM-dispatched events; asserted PINNED, sorted class, resumed LIVE and SIM MODE. RADAR uses bundled sample JSON when the absent latest snapshot returns 404. LIVE here means advancing simulation, not real trading.
- Stills and animated WebP: `/home/ubuntu/audits/shots/pamm-a/`, embedded in PR body with simulated-data labels. Terminal still requires horizontal panning, not a new stacked mobile layout. Other pages have stills only; no claim of their functional acceptance.
- Inline terminal JavaScript: `node --check` exit 0. Final PR body: `python scripts/check_acceptance.py <body>`, output in PR body.

## Coverage of all acceptance sections

- §1: inspect/plan/scoped patch/check; narrow responsive accessibility fix only after reproduced failure. Historical approval/redesign intent is not assessable from source alone.
- §2: actual checks, failure output and artifacts disclosed; research statements qualified rather than invented evidence. No numeric benchmark/production support claim accepted from inspection.
- §3: mobile click repro, geometry diagnosis, minimal overflow/flex fix, same interaction rerun passed; existing handlers/data semantics unchanged.
- §4: all five page stills plus current simulated terminal interaction WebP delivered prospectively; historical evidence gap remains.
- §5: LOCAL ONLY, human next actions and qualified deployment claim. Historical ancestry checked; new branch is not merged or deployed. No duplicate product implementation.
- §6: README paths corrected and acceptance/skill-routing pointers reviewed. No OKC job-ID rule imported; modeled simulation priors are not production findings.
- §7: limitations below disclosed; no paid/credentialed bypass.
- §8: historical #1–#6 failures retained; #7 passes; new PR supplies closer.

## Remaining limitations / blocker report (§7)

| blocked_on | evidence | impact | human_action | not_doing |
|---|---|---|---|---|
| Verified deployment/data feed | local DNS curl error above; only local simulation captured | degraded: no production acceptance | Owner confirms intended deployment/feed and authorizes its validation | No localhost-as-production or live-trading claim |
| Research evidence | No supplied underlying trace/decoder output for PR #6 claims | degraded: findings remain unverified | Owner supplies independently reviewable artifacts or authorizes a separate scoped reproduction | No paid RPC or invented measurements |
| SBF integration prerequisite | Expected .so absent; host cargo test fails as quoted | degraded: AMM correctness unaccepted | Owner provisions approved Solana SBF toolchain/build environment for separate program validation | No ignored test failure or committed generated binary |

Not fixable after merge: original PR bodies' missing contemporaneous closer and QA evidence. Not run: SBF build, Anchor/Quasar tests, actual bot/chain transactions, real feed, Binary Ninja/credentialed RE agent, paid RPC or deployment. This branch does not fix or accept AMM implementation correctness.
