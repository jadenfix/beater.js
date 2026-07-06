# Observability

beater.js can export journaled agent runs after `beater agent run` or `beater agent resume` returns. Export is disabled by default and is best effort: failures are logged, but they do not change the agent run result.

## Native Beater Ingest

Enable it with:

```sh
export BEATER_TRACE_EXPORT_URL="http://127.0.0.1:8080"
export BEATER_TENANT_ID="tenant"
export BEATER_PROJECT_ID="project"
export BEATER_ENVIRONMENT_ID="local"
export BEATER_API_KEY="..." # optional when your Beater ingest requires it
```

When enabled, each run is projected into Beater native spans and posted to `/v1/traces/native`.

## OTLP HTTP Export

Enable OTLP/HTTP JSON export with either Beater's explicit env var:

```sh
export BEATER_OTLP_EXPORT_URL="http://127.0.0.1:4318"
```

or standard OpenTelemetry endpoint variables:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT="http://127.0.0.1:4318"
# or
export OTEL_EXPORTER_OTLP_TRACES_ENDPOINT="http://127.0.0.1:4318/v1/traces"
```

`BEATER_OTLP_EXPORT_URL` and `OTEL_EXPORTER_OTLP_ENDPOINT` are treated as collector base URLs and post to `/v1/traces`; `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is used as the full traces endpoint. `OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_EXPORTER_OTLP_TRACES_HEADERS` are forwarded as comma-separated `name=value` headers. `BEATER_API_KEY`, when set, is forwarded as `x-beater-api-key`.

The OTLP payload contains one resource span batch for the run:

- one `agent.run` span for the journal run
- one `llm.call` span per journaled LLM step
- one `tool.call` span per journaled tool step
- `agent.step` spans for other step kinds

Span attributes include the beater.js run id, agent name, step sequence, step kind/status/attempt, and tool name or tool use id when present. OTLP spans also include `beater.input` and `beater.output` events whose `beater.payload_json` attribute contains the journaled prompt, request, response, or tool result payload.

Run the local OTLP proof with:

```sh
scripts/otlp-trace-gate.cjs
```

The gate starts a mock Anthropic SSE server, runs the real `beater agent run` CLI against a temp app, captures the OTLP `/v1/traces` request, and verifies the run, LLM, and tool spans. This proves the OTLP/HTTP JSON collector contract; the Beater dashboard gate below proves the native Beater ingest/read/rendered-dashboard path.

Run the local Beater read/dashboard proof with:

```sh
cargo build --bin beater
scripts/beater-dashboard-trace-gate.cjs
```

That gate starts a local `beaterd` from `BEATERD_BIN` or a sibling `../beater` checkout, runs the real `beater agent run` CLI against a temp app, exports native spans to `/v1/traces/native`, and verifies the same Beater read endpoints used by the dashboard:

- `GET /v1/traces/demo?project_id=demo&environment_id=local&trace_id=...`
- `GET /v1/traces/demo/<trace_id>`
- `GET /v1/spans/demo/<trace_id>/<span_id>`
- `GET /v1/spans/demo/<trace_id>/<span_id>/io`

The script prints a dashboard URL for the exported run. To also require a rendered dashboard page, run the gate on a fixed API port and start the dashboard against that same port:

```sh
# terminal 1
cd ../beater/web/dashboard
NEXT_PUBLIC_BEATER_API_BASE_URL=http://127.0.0.1:18080 \
  BEATER_API_BASE_URL=http://127.0.0.1:18080 \
  npm run dev -- --hostname 127.0.0.1 --port 3100

# terminal 2
BEATERD_HTTP_PORT=18080 \
  BEATER_DASHBOARD_PROBE=1 \
  BEATER_DASHBOARD_URL=http://127.0.0.1:3100 \
  scripts/beater-dashboard-trace-gate.cjs
```

On 2026-07-06, the rendered probe passed against the sibling Beater dashboard with a 200 response for an exported run trace containing four spans.

Use `BEATERD_OTLP_GRPC_PORT` if the default random OTLP gRPC port is not acceptable in your environment.
