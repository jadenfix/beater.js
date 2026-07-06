# Observability

beater.js can export journaled agent runs to the Beater native trace ingest API after `beater agent run` or `beater agent resume` returns. The exporter is disabled by default and is best effort: export failures are logged, but they do not change the agent run result.

Enable it with:

```sh
export BEATER_TRACE_EXPORT_URL="http://127.0.0.1:8080"
export BEATER_TENANT_ID="tenant"
export BEATER_PROJECT_ID="project"
export BEATER_ENVIRONMENT_ID="local"
export BEATER_API_KEY="..." # optional when your Beater ingest requires it
```

When enabled, each run is projected into Beater native spans and posted to `/v1/traces/native`:

- one `agent.run` span for the journal run
- one `llm.call` span per journaled LLM step
- one `tool.call` span per journaled tool step
- `agent.step` spans for other step kinds

Span attributes include the beater.js run id, agent name, step sequence, step kind/status/attempt, and tool name or tool use id when present. Span `input` and `output` fields are populated from the journaled prompt, request, and result payloads.

This is the first native Beater bridge. Full OTLP export and dashboard proof remain future work for the observability milestone.
