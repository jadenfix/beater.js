//! The Agent Access Layer's crawl half (ARCHITECTURE.md §6b): robots.txt,
//! sitemap.xml, llms.txt, and the .well-known manifest — all generated from
//! the route table and agent registry, never hand-maintained.

use serde_json::{Map, Value, json};

use crate::worker::{RouteActionMeta, RouteMeta};

pub fn robots_txt(base_url: &str) -> String {
    format!(
        "User-agent: *\nAllow: /\n\nSitemap: {base_url}/sitemap.xml\n# agent-readable map: {base_url}/llms.txt\n# manifest: {base_url}/.well-known/beater.json\n"
    )
}

/// Crawlable routes (per their `agent` metadata) with lastmod from file mtime.
pub fn sitemap_xml(
    base_url: &str,
    routes: &[(String, std::path::PathBuf, Option<RouteMeta>)],
) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for (pattern, file, meta) in routes {
        if matches!(meta, Some(m) if !m.crawl) {
            continue;
        }
        let lastmod = std::fs::metadata(file)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| {
                chrono::DateTime::<chrono::Utc>::from(t)
                    .format("%Y-%m-%d")
                    .to_string()
            });
        out.push_str("  <url>\n");
        out.push_str(&format!("    <loc>{base_url}{pattern}</loc>\n"));
        if let Some(lastmod) = lastmod {
            out.push_str(&format!("    <lastmod>{lastmod}</lastmod>\n"));
        }
        out.push_str("  </url>\n");
    }
    out.push_str("</urlset>\n");
    out
}

/// llms.txt: a curated, agent-readable map of the site. Route entries are
/// enriched by each module's optional `export const agent = {...}` metadata.
pub fn llms_txt(
    app_name: &str,
    base_url: &str,
    routes: &[(String, Option<RouteMeta>)],
    actions: &[RouteActionMeta],
    agents: &[String],
    mcp_access: &crate::mcp::AccessConfig,
) -> String {
    let mut out = format!("# {app_name}\n\n> Served by beater.js — agent-first web framework.\n\n");
    out.push_str("## Routes\n\n");
    for (pattern, meta) in routes {
        match meta {
            Some(m) if !m.crawl => continue,
            Some(m) => {
                let title = m.title.clone().unwrap_or_else(|| pattern.clone());
                match &m.description {
                    Some(d) => out.push_str(&format!("- [{title}]({base_url}{pattern}): {d}\n")),
                    None => out.push_str(&format!("- [{title}]({base_url}{pattern})\n")),
                }
            }
            None => out.push_str(&format!("- [{pattern}]({base_url}{pattern})\n")),
        }
    }
    if !agents.is_empty() {
        out.push_str("\n## Agents\n\n");
        for agent in agents {
            out.push_str(&format!("- {agent}\n"));
        }
    }
    if !actions.is_empty() {
        out.push_str("\n## Actions\n\n");
        for action in actions {
            let auth = action_auth_type(&action.auth);
            out.push_str(&format!(
                "- `{}`: {} `{} {}`. Side effect: `{}`. Auth: `{}`. Confirm: `{}`. Dry run: `{}`.\n",
                action.id,
                action.description,
                action.method,
                action.path,
                action.side_effect,
                auth,
                action.confirm,
                action.dry_run
            ));
        }
    }
    let auth_note = if mcp_access.auth_required() {
        "requires Authorization: Bearer <token>"
    } else {
        "no bearer token configured"
    };
    out.push_str(&format!(
        "\n## For AI agents\n\n- MCP endpoint (tools): {base_url}/mcp (Streamable HTTP, spec {}; {auth_note})\n- Manifest: {base_url}/.well-known/beater.json\n",
        crate::mcp::PROTOCOL_VERSION,
    ));
    if !actions.is_empty() {
        out.push_str(&format!("- OpenAPI actions: {base_url}/openapi.json\n"));
    }
    out
}

pub fn well_known(
    app_name: &str,
    base_url: &str,
    agents: &[String],
    actions: &[RouteActionMeta],
    mcp_access: &crate::mcp::AccessConfig,
) -> serde_json::Value {
    let auth = if mcp_access.auth_required() {
        json!({"required": true, "schemes": ["bearer"]})
    } else {
        json!({"required": false, "schemes": []})
    };
    json!({
        "name": app_name,
        "framework": {"name": "beater.js", "version": env!("CARGO_PKG_VERSION")},
        "mcp": {
            "endpoint": format!("{base_url}/mcp"),
            "transport": "streamable-http",
            "protocolVersion": crate::mcp::PROTOCOL_VERSION,
            "auth": auth,
            "originPolicy": {
                "noOrigin": "allowed",
                "loopbackOrigins": true,
                "trustedOrigins": mcp_access.trusted_origins(),
            },
        },
        "openapi": format!("{base_url}/openapi.json"),
        "sitemap": format!("{base_url}/sitemap.xml"),
        "llms": format!("{base_url}/llms.txt"),
        "agents": agents,
        "actions": actions,
    })
}

pub fn openapi_json(app_name: &str, base_url: &str, actions: &[RouteActionMeta]) -> Value {
    let mut paths = Map::new();
    for action in actions {
        let path = paths
            .entry(action.path.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(methods) = path {
            let method = action.method.to_ascii_lowercase();
            if let Some(existing) = methods.get_mut(&method) {
                append_colliding_action(existing, action);
            } else {
                methods.insert(method, openapi_action_operation(action));
            }
        }
    }

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": app_name,
            "description": "Agent action surface generated by beater.js route metadata.",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "servers": [{"url": base_url}],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer"
                }
            }
        }
    })
}

fn openapi_action_operation(action: &RouteActionMeta) -> Value {
    let parameters = if action.idempotency_required {
        json!([
            {
                "name": "Idempotency-Key",
                "in": "header",
                "required": true,
                "schema": {"type": "string"},
                "description": "Required idempotency key for mutating agent actions."
            }
        ])
    } else {
        json!([])
    };
    let security = if action_auth_type(&action.auth) == "public" {
        json!([])
    } else {
        json!([{"bearerAuth": []}])
    };

    let mut operation = json!({
        "operationId": action.id,
        "summary": action.title,
        "description": action.description,
        "security": security,
        "parameters": parameters,
        "x-beater-connect": {
            "sideEffect": action.side_effect,
            "auth": action.auth,
            "confirm": action.confirm,
            "dryRun": action.dry_run,
            "idempotencyRequired": action.idempotency_required,
            "inputSchema": action.input_schema,
            "outputSchema": action.output_schema,
            "actions": [action_extension_json(action)]
        },
        "responses": {
            "200": {
                "description": "Action result",
                "content": {
                    "application/json": {
                        "schema": action.output_schema
                    }
                }
            }
        }
    });
    if !matches!(action.method.as_str(), "GET" | "HEAD") {
        operation["requestBody"] = json!({
            "required": true,
            "content": {
                "application/json": {
                    "schema": action.input_schema
                }
            }
        });
    }
    operation
}

fn append_colliding_action(operation: &mut Value, action: &RouteActionMeta) {
    let Some(extension) = operation
        .get_mut("x-beater-connect")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    extension.insert("multipleActions".to_string(), Value::Bool(true));
    let actions = extension
        .entry("actions")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(actions) = actions.as_array_mut() {
        actions.push(action_extension_json(action));
    }
}

fn action_extension_json(action: &RouteActionMeta) -> Value {
    json!({
        "id": action.id,
        "title": action.title,
        "description": action.description,
        "method": action.method,
        "path": action.path,
        "sideEffect": action.side_effect,
        "auth": action.auth,
        "confirm": action.confirm,
        "dryRun": action.dry_run,
        "idempotencyRequired": action.idempotency_required,
        "inputSchema": action.input_schema,
        "outputSchema": action.output_schema,
    })
}

fn action_auth_type(auth: &Value) -> &str {
    auth.get("type").and_then(Value::as_str).unwrap_or("public")
}

pub fn route_actions(routes: &[(String, Option<RouteMeta>)]) -> Vec<RouteActionMeta> {
    routes
        .iter()
        .filter_map(|(_, meta)| meta.as_ref())
        .filter(|meta| meta.crawl)
        .flat_map(|meta| meta.actions.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{llms_txt, openapi_json, route_actions, well_known};
    use crate::mcp::AccessConfig;
    use crate::worker::{RouteActionMeta, RouteMeta};

    fn demo_action() -> RouteActionMeta {
        RouteActionMeta {
            id: "health_check".to_string(),
            title: "Read health".to_string(),
            description: "Read runtime health status.".to_string(),
            method: "POST".to_string(),
            path: "/api/health".to_string(),
            side_effect: "read".to_string(),
            auth: json!({"type": "public", "scopes": []}),
            confirm: false,
            dry_run: false,
            idempotency_required: false,
            input_schema: json!({"type": "object", "properties": {}, "required": []}),
            output_schema: json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}),
        }
    }

    #[test]
    fn route_actions_collects_metadata_actions() {
        let routes = vec![(
            "/api/health".to_string(),
            Some(RouteMeta {
                title: Some("Health".to_string()),
                description: None,
                crawl: true,
                actions: vec![demo_action()],
            }),
        )];

        let actions = route_actions(&routes);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, "health_check");
    }

    #[test]
    fn route_actions_skip_non_crawlable_routes() {
        let routes = vec![(
            "/api/private".to_string(),
            Some(RouteMeta {
                title: Some("Private".to_string()),
                description: None,
                crawl: false,
                actions: vec![demo_action()],
            }),
        )];

        let actions = route_actions(&routes);

        assert!(actions.is_empty());
    }

    #[test]
    fn action_metadata_reaches_manifest_llms_and_openapi() {
        let action = demo_action();
        let actions = vec![action];
        let access = AccessConfig::default();

        let manifest = well_known("Demo", "http://127.0.0.1:3000", &[], &actions, &access);
        assert_eq!(manifest["actions"][0]["id"], "health_check");
        assert_eq!(manifest["openapi"], "http://127.0.0.1:3000/openapi.json");

        let llms = llms_txt("Demo", "http://127.0.0.1:3000", &[], &actions, &[], &access);
        assert!(llms.contains("## Actions"));
        assert!(llms.contains("health_check"));
        assert!(llms.contains("OpenAPI actions"));

        let openapi = openapi_json("Demo", "http://127.0.0.1:3000", &actions);
        assert_eq!(
            openapi["paths"]["/api/health"]["post"]["operationId"],
            "health_check"
        );
        assert_eq!(
            openapi["paths"]["/api/health"]["post"]["x-beater-connect"]["sideEffect"],
            "read"
        );
    }

    #[test]
    fn mutating_actions_require_idempotency_header_in_openapi() {
        let mut action = demo_action();
        action.id = "create_ticket".to_string();
        action.side_effect = "write".to_string();
        action.idempotency_required = true;

        let openapi = openapi_json("Demo", "http://127.0.0.1:3000", &[action]);
        let parameters = &openapi["paths"]["/api/health"]["post"]["parameters"];

        assert_eq!(parameters[0]["name"], "Idempotency-Key");
        assert_eq!(
            openapi["paths"]["/api/health"]["post"]["x-beater-connect"]["idempotencyRequired"],
            true
        );
    }

    #[test]
    fn get_actions_do_not_emit_required_request_bodies() {
        let mut action = demo_action();
        action.method = "GET".to_string();

        let openapi = openapi_json("Demo", "http://127.0.0.1:3000", &[action]);

        assert!(openapi["paths"]["/api/health"]["get"]["requestBody"].is_null());
        assert_eq!(
            openapi["paths"]["/api/health"]["get"]["x-beater-connect"]["inputSchema"]["type"],
            "object"
        );
    }

    #[test]
    fn colliding_actions_are_preserved_in_openapi_extension_metadata() {
        let first = demo_action();
        let mut second = demo_action();
        second.id = "health_alias".to_string();

        let openapi = openapi_json("Demo", "http://127.0.0.1:3000", &[first, second]);
        let extension = &openapi["paths"]["/api/health"]["post"]["x-beater-connect"];

        assert_eq!(extension["multipleActions"], true);
        assert_eq!(extension["actions"][0]["id"], "health_check");
        assert_eq!(extension["actions"][1]["id"], "health_alias");
    }
}
