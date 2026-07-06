---
name: systems-engineering
description: Make high-leverage beater.js architecture, language, framework, algorithm, performance, reliability, and security decisions. Use when choosing an implementation approach, optimizing a hot path, refactoring, selecting Rust/TypeScript/Python/C++/Wasm boundaries, or deciding whether a change is materially better than the status quo.
---

# beater.js systems engineering

Use this skill before large implementation choices and after prototypes that risk becoming permanent. The standard is not "works"; the standard is a smaller, safer, more reliable system that makes beater.js meaningfully harder to replace.

## Decision workflow

1. Name the user-visible or operator-visible outcome. Avoid optimizing a mechanism before naming the outcome.
2. Name the invariant: latency bound, durability guarantee, auth boundary, compatibility rule, memory cap, or DX promise.
3. Pick the simplest architecture that proves the invariant. Prefer deleting duplicated paths over adding knobs.
4. Choose language and framework by boundary, not preference.
5. Choose the algorithm/data structure by workload shape, adversarial inputs, and failure mode.
6. Define the proof before or with the code: unit test, integration gate, CI check, benchmark, transcript, or runtime evidence.
7. Reject the change if it is only different. It must improve at least one measured axis without weakening security, durability, or clarity.

## Language and boundary choices

- Rust: host runtime, routing, journals, auth gates, network clients, build/deploy, concurrency, durability, and anything enforcing policy.
- TypeScript in V8: app routes, SSR/RSC/userland code, route-local actions, and developer-facing extension points.
- Python: trusted ML/data tools that need the Python ecosystem; keep inputs/outputs serialized and validate at the Rust boundary.
- C++: narrow native acceleration or existing library integration behind a Rust-owned safe wrapper and tests through the actual registry path.
- Wasm/Wasmtime: untrusted scalar tools needing hermetic execution, fuel/memory/wall limits, and denied filesystem/network capabilities.
- Shell scripts: deterministic e2e gates and local orchestration only; fail closed, quote variables, avoid destructive cleanup of user-supplied paths.

## Framework and dependency choices

- Prefer existing workspace primitives before adding a dependency.
- Add a dependency only when it removes more risk than it adds: maintenance, binary size, supply chain, compile time, platform support, and security surface all count.
- Keep model-facing and network-facing contracts self-contained; do not depend on callers knowing hidden Rust types or internal refs.
- For browser or MCP integrations, prefer real protocol paths over helper-only tests.

## Algorithm and performance choices

- Stream remote-driven data instead of fully materializing it when size is untrusted.
- Bound queues, live state, spawned tasks, request bodies, tool results, logs, screenshots, DOMs, and serialized envelopes.
- Use backpressure and cancellation before adding worker count.
- Prefer idempotency keys, journal checkpoints, and replay-safe state machines over best-effort retry flags.
- Benchmark only the path that users or CI actually exercise; synthetic helpers do not prove production behavior.
- Optimize the bottleneck after identifying it. More code is not optimization.

## Security and reliability floor

- Loopback, Host, and Origin checks are not authentication. Control-plane access needs an unguessable same-user capability.
- Network policy must bind to the concrete endpoint after DNS, proxies, redirects, and retries.
- Secrets must not enter journals, logs, bundles, screenshots, error pages, or fixtures as realistic literals.
- Durable storage changes need corruption detection or recovery, not just a recency argument.
- Cached verdicts about shared mutable files must be re-derived from content or scoped to an exclusive lock.

## Remarkably-better bar

A change is worth carrying only if it is clearly better on at least one axis and neutral or better on the rest:

- Capability: enables a real app/agent/operator workflow that was previously blocked.
- Correctness: removes a traced failure mode or closes a contract lie.
- Performance: improves measured latency, throughput, memory, startup, or build time on the actual path.
- Security: reduces authority, exposure, secret risk, or ambiguity at a trust boundary.
- Operability: improves evidence, logs, retries, cancellation, deployability, or recovery.
- Simplicity: deletes duplicated code, removes an abstraction, or makes the invariant obvious.

If the change cannot name its proof, it is not done. If it cannot name the invariant, it is not designed.

## Output standard

End with:

- `Decision`: chosen approach and rejected alternatives.
- `Why this is better`: measured or provable axis of improvement.
- `Invariant protected`: the exact guarantee.
- `Proof required`: the test/gate/benchmark/docs evidence needed before claiming done.
