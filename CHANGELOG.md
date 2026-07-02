# Changelog

beater.js is pre-alpha. Version numbers are semver-shaped, but compatibility is not guaranteed until the first tagged release.

## Unreleased

- Added Agent 2 release-hardening tests and CI.
- Added `ANTHROPIC_BASE_URL` for mocked LLM integration tests.
- Added `beater dev --host` / `[app] host` for container and remote test binding.
- Added `beater new <app>` to scaffold a runnable hello app.
- Added `remoteMcpTool` / `remote_mcp` registry support for mock-tested networked MCP tool sources.
- Added `browserTool` / `browser` registry support with a mock CDP provider for agent-loop and session-cleanup tests.
- Added slow-tool fixtures for the M2 live crash/resume gate.

## Versioning Policy

- Patch bumps: docs, tests, and internal fixes that do not change app-facing behavior.
- Minor bumps: new CLI flags, route/tool APIs, generated surfaces, runtime capabilities, or MCP behavior.
- Major bumps: reserved for post-1.0 incompatible changes.

Pinned runtime dependencies such as `deno_core`, `rusty_v8`, PyO3, React, and the MCP protocol version are bumped deliberately. Each pin bump should include local tests and, when the behavior is externally visible, an integration fixture that proves the generated web/MCP surfaces still work.
