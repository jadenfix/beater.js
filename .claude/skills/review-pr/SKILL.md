---
name: review-pr
description: High-recall, high-precision independent review of a beater.js PR. Use when asked to review a PR in jadenfix/beater.js (e.g. "/review-pr 110"). Reviews must be done by an agent that did NOT author the PR.
---

# beater.js PR review

You are an independent, non-author reviewer for `jadenfix/beater.js`. The argument is a PR number: `$ARGUMENTS`. Several agents work this repo concurrently — assume nothing about freshness, and never rubber-stamp. This rubric teaches you *how* to find bugs on any PR; it is deliberately not a list of past bugs to grep for.

## Ground rules

- **Non-author only.** Check `gh pr view <N> -R jadenfix/beater.js --json commits -q '.commits[].messageHeadline'` — if you recognize any commit as your own work from this session, stop and hand the review to another agent.
- Read-only: do not modify the main clone, do not run `cargo` in a directory another agent may be building in. CI already builds per-PR; review by reading.
- Precision: every **blocker** carries a concrete traced failure scenario (specific input/state → specific wrong behavior, with `file:line`). If you cannot trace one, it is a nit.
- Recall: read the ENTIRE diff, the referenced issues, and the surrounding code of every touched file at current `main`. Bugs live at the seams the diff doesn't show.

## Procedure

1. `gh pr view <N> -R jadenfix/beater.js --json title,body,author,files,mergeStateStatus,statusCheckRollup`
2. `gh pr diff <N> -R jadenfix/beater.js` — all of it.
3. `gh issue view <issue> -R jadenfix/beater.js` for every referenced issue; the issue defines the intended scope.
4. **Supersession check:** `git log origin/main --oneline -30` plus targeted `git log -p` on touched files → REJECT (superseded) if main already contains an equivalent fix.
5. **Freshness check:** after any wait, force-push, PR body edit, or CI rerun, re-read PR state, head SHA, base SHA, check rollup, and linked issue state.
6. **Overlap check:** `gh pr list -R jadenfix/beater.js --state open` — flag open PRs touching the same paths and whether merge order matters.
7. Hunt for bugs using the method below.
8. Post the review (format at the bottom) and return a structured verdict.

## How to find bugs (do this — don't just tick boxes)

- **Trace one path end to end.** Follow one request through route resolution and the V8 isolate, or one agent step through journal → tool call → journal — into the crash, timeout, and resume branches, not just the happy path.
- **Review from three seats.** beater.js serves an **app developer** (DX honesty: source-mapped errors, hot reload, `doctor` telling the truth), a **durable agent run** (a crash at ANY instruction must resume without lost or duplicated side effects), and an **operator** exposing `/mcp` or a built bundle (auth, origins, cold start). For the code in the diff, ask how it hurts each of the three.
- **Enumerate failure modes** for every new input, call, or state transition: empty · malformed · oversized · slow/hung · repeated/retried · concurrent · out-of-order · partial failure · adversarial/untrusted.
- **Follow the seams the diff hides:** callers of changed signatures, callees now leaned on, invariants elsewhere that assumed the old behavior — especially across the Rust/V8/Python tier boundaries.
- **Reverted-fix test:** would any test in the PR still pass if the fix were reverted? If yes, it proves nothing — a blocker for a bugfix PR.
- **Adversarially verify** each candidate blocker: try to refute it against the code. Survives → blocker. No concrete trace → nit.
- **Preserve durable lessons** under `Durable guidance`; a follow-up author lands accepted guidance in this file from a separate PR.

## What to look for (general bug classes)

Correctness & honesty of the contract:
- [ ] Return values, HTTP statuses, and tool envelopes tell the caller the truth — a failure or no-op is never reported as success; truncation and sampling are labeled.
- [ ] Docs and milestone claims match the code — no present-tense claims for gated/pending work (the M2/M8 pattern: code-complete-but-gate-pending is stated as such).
- [ ] Declared runtime limits (`docs/runtime-limits.md`) stay accurate when concurrency or isolate behavior changes.

Resource, lifecycle & availability:
- [ ] Everything that can grow is bounded: request bodies, journal entries, spawned tasks, isolate work queues, Python call payloads. Unbounded growth on remote-driven input is a blocker.
- [ ] Every model/tool/subprocess round-trip has a timeout **and** a recovery path; cleanup runs on all exit paths including error and cancel.
- [ ] Locks are narrow and never held across `.await`; blocking Python (GIL) or synchronous V8 work must not stall the journal or the agent loop's liveness.

Tests:
- [ ] Tests exercise the actual failure mode (survive the reverted-fix question); limits tested at, below, above the boundary.

Fit & simplicity:
- [ ] The change does exactly what its issue needs — no speculative abstraction, dead branch, or unused knob.
- [ ] It fits ARCHITECTURE.md: the four-tier model (V8 routes/SSR, CPython tools, native Rust agent loop, Wasmtime planned) stays intact; tiers communicate by serialization, not shared mutable state.

## beater.js-specific bug classes (check every one the diff touches)

Durability & resume (the core promise):
- [ ] Every agent step transition is journaled BEFORE its side effect becomes externally visible; `kill -9` between any two instructions must resume to a consistent state.
- [ ] Resume is idempotent: a step that completed before the crash is not re-executed with side effects; a step that half-ran is either safely retryable or surfaced as needing intervention — never silently duplicated.
- [ ] Journal schema changes read old journals (or migrate them); a resume that fails on a pre-change journal is a blocker.
- [ ] Journal writes are crash-safe (no torn committed state on restart) and the write path stays batched — no per-step full-fsync regression on the hot loop.

Tier boundaries:
- [ ] Data crossing Rust↔V8↔Python is serialized and validated at the boundary; errors from embedded runtimes carry source-mapped/typed context, not stringly-typed panics.
- [ ] The single-isolate constraint is respected honestly: changes that add per-request JS work don't silently serialize unrelated routes without the limits doc saying so.

MCP & network exposure:
- [ ] `/mcp` beyond localhost requires the bearer token; browser origins are allowlisted explicitly (`BEATER_MCP_TRUSTED_ORIGINS`); loopback/Host/Origin checks are not treated as authentication.
- [ ] Crawl surfaces (robots.txt, sitemap.xml, llms.txt, .well-known) are generated from the actual route table — a route change that leaves stale generated claims is a bug.
- [ ] Secrets (`ANTHROPIC_API_KEY`, tokens) never appear in journals, logs, error pages, or bundle output.

Module resolution & build:
- [ ] `exports`-condition resolution changes are tested against real `node_modules` fixtures (node/import/module/default precedence); unsupported cases (CommonJS `require`, Node built-ins) fail with a clear error, never silently resolve to the wrong file.
- [ ] `beater build` bundles run without the dev-tree present; `doctor` checks match what the runtime actually requires (PYO3_PYTHON, venv, V8).

## Verdict & posting

Post exactly one review:

```
gh pr review <N> -R jadenfix/beater.js --comment --body "<body>"
```

Body format — first line is the verdict, nothing above it:

```
VERDICT: APPROVE | REQUEST-CHANGES | REJECT (superseded | wrong-approach)

<one-paragraph summary: what the PR does, whether it fixes the traced failure>

Blockers:
- <file:line — traced failure scenario>   (or "none")

Nits:
- <file:line — suggestion>                (or "none")

Durable guidance: <candidate reusable invariant for follow-up docs, or "none">

Overlap: <open PRs touching same paths + merge-order note, or "none">

— independent review agent (non-author)
```

APPROVE only with zero blockers. REQUEST-CHANGES when fixable blockers exist. REJECT when superseded or the approach conflicts with ARCHITECTURE.md. Do not merge — merging is the coordinator's job after CI + mergeability recheck.

## Deep mode (optional)

If asked for a "deep" review, fan out three parallel non-author subagents with distinct lenses — (a) durability/resume correctness, (b) tier-boundary and network security, (c) DX honesty/over-engineering — then adversarially verify each candidate blocker yourself before posting.
