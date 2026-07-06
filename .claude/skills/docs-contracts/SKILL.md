---
name: docs-contracts
description: Keep beater.js documentation, public contracts, final.md evidence, OpenAPI/action metadata, and ecosystem-facing scope honest. Use when changing routes, status codes, schemas, MCP/tool/action surfaces, build/deploy behavior, docs, milestones, or completion claims.
---

# beater.js docs and contracts

Use this skill whenever a code or behavior change affects what app developers, agents, operators, SDKs, or ecosystem projects can rely on. The job is not to write more docs; it is to keep public promises synchronized with the runtime.

## Contract inventory

Before editing docs, identify which contract changed:

- `README.md`: current user-facing status, quickstart, limits, and milestone claims.
- `ARCHITECTURE.md`: durable thesis, tier boundaries, runtime model, and deferred work.
- `final.md`: evidence-backed completion ledger; update only when the proof exists.
- `docs/security.md`: trust boundaries, auth, secrets, network exposure, and local/remote operator model.
- `docs/runtime-limits.md`: concurrency, queues, isolate/worker behavior, cancellation, and scaling limits.
- `docs/tools.md`: tool schema, idempotency, side effects, and model-facing tool contract.
- `docs/integrations.md`: first-party tools, remote MCP, browser providers, SaaS/API integrations, retries, sessions, and egress policy.
- Generated/client-facing surfaces: OpenAPI, action metadata, MCP catalog schemas, llms.txt, sitemap, robots, and well-known manifests.

## Required workflow

1. State the changed behavior in one sentence using precise nouns: route, status code, field, schema, gate, auth rule, build artifact, or runtime limit.
2. Find every public surface that mentions that behavior. Do not update only the nearest README sentence.
3. Keep status claims evidence-based. Use present tense only after a test, script, transcript, CI check, or merged PR proves the claim.
4. If a runtime-visible contract changes, update generated-client-facing docs or tests in the same slice.
5. If a model-facing tool/action schema changes, keep one canonical shape. Do not publish unresolved `$ref`, opaque object placeholders, or aliases for the same argument.
6. If a completion checkbox changes in `final.md`, include the command, transcript, PR, or CI evidence that proves it. If the evidence is pending, say pending.
7. Keep docs short and operational. Prefer one exact invariant over broad marketing language.

## Honesty checks

- Does a clean user know what to run, what credentials are required, and what will fail without them?
- Does an agent know the exact tool schema, side effect, idempotency, and auth model?
- Does an operator know what is exposed on the network and what protects it?
- Does an SDK/client generator see the same fields and statuses the runtime actually emits?
- Does the docs change survive a reverted-code thought experiment, or is it only aspirational?

## Ecosystem communication

When the change affects beater.js as part of the broader ecosystem, document both sides:

- Standalone behavior: what beater.js can do without sibling projects.
- Integration lane: how it communicates with Beater, beater-memory, beatbox, tempo, beaterOS, remote MCP providers, or browser providers.
- Trust boundary: which credentials, origins, hosts, sessions, and idempotency keys cross that lane.
- Evidence: the test, script, trace, or gate proving the lane works.

## Output standard

End with:

- `Docs updated`: exact files and contracts covered.
- `Evidence`: command/CI/PR/transcript, or `pending` with the missing proof named.
- `Remaining public promises`: anything still aspirational or intentionally deferred.
