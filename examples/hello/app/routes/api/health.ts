// M1: runs inside beater's embedded V8 — no Node, no Deno.
interface HealthReport {
  ok: boolean;
  runtime: string;
  now: string;
}

export function GET() {
  const report: HealthReport = {
    ok: true,
    runtime: "beater.js",
    now: new Date().toISOString(),
  };
  return {
    status: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(report),
  };
}
