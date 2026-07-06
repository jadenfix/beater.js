//! The Agent Access Layer's crawl half (ARCHITECTURE.md §6b): robots.txt,
//! sitemap.xml, llms.txt, and the .well-known manifest — all generated from
//! the route table and agent registry, never hand-maintained.

use serde_json::json;

use crate::worker::RouteMeta;

pub fn robots_txt(base_url: &str, routes: &[(String, Option<RouteMeta>)]) -> String {
    let mut disallowed: Vec<&str> = routes
        .iter()
        .filter_map(|(pattern, meta)| {
            if matches!(meta, Some(meta) if !meta.crawl) {
                Some(pattern.as_str())
            } else {
                None
            }
        })
        .collect();
    disallowed.sort_unstable();
    disallowed.dedup();

    let mut out = String::from("User-agent: *\n");
    if !disallowed.contains(&"/") {
        out.push_str("Allow: /\n");
    }
    for path in disallowed {
        out.push_str(&format!("Disallow: {path}\n"));
    }
    out.push_str(&format!(
        "\nSitemap: {base_url}/sitemap.xml\n# agent-readable map: {base_url}/llms.txt\n# manifest: {base_url}/.well-known/beater.json\n"
    ));
    out
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
        out.push_str(&format!(
            "    <loc>{}</loc>\n",
            escape_xml_text(&format!("{base_url}{pattern}"))
        ));
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
    actions: &[crate::mcp::RouteActionTool],
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
                let href = markdown_link_destination(&format!("{base_url}{pattern}"));
                let title = markdown_link_text(&title);
                match &m.description {
                    Some(d) => out.push_str(&format!("- [{title}]({href}): {d}\n")),
                    None => out.push_str(&format!("- [{title}]({href})\n")),
                }
            }
            None => {
                let href = markdown_link_destination(&format!("{base_url}{pattern}"));
                let title = markdown_link_text(pattern);
                out.push_str(&format!("- [{title}]({href})\n"));
            }
        }
    }
    if !actions.is_empty() {
        out.push_str("\n## Actions\n\n");
        for action in actions {
            let href = markdown_link_destination(&format!("{}{}", base_url, action.path));
            let name = markdown_link_text(&action.name);
            out.push_str(&format!(
                "- [{name}]({href}): {} ({} {}; confirm: {}; idempotency: {})\n",
                action.description,
                action.method,
                action.side_effect,
                action.confirm,
                action.idempotency_required
            ));
        }
    }
    if !agents.is_empty() {
        out.push_str("\n## Agents\n\n");
        for agent in agents {
            out.push_str(&format!("- {agent}\n"));
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
    out
}

pub fn well_known(
    app_name: &str,
    base_url: &str,
    agents: &[String],
    actions: &[crate::mcp::RouteActionTool],
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
            },
        },
        "openapi": format!("{base_url}/openapi.json"),
        "sitemap": format!("{base_url}/sitemap.xml"),
        "llms": format!("{base_url}/llms.txt"),
        "agents": agents,
        "actions": actions.iter().map(|action| {
            json!({
                "name": action.name,
                "description": action.description,
                "method": action.method,
                "path": action.path,
                "sideEffect": action.side_effect,
                "confirm": action.confirm,
                "dryRun": action.dry_run,
                "idempotencyRequired": action.idempotency_required,
                "auth": action.auth,
            })
        }).collect::<Vec<_>>(),
    })
}

pub fn openapi_json(
    app_name: &str,
    base_url: &str,
    actions: &[crate::mcp::RouteActionTool],
) -> serde_json::Value {
    let mut paths = serde_json::Map::new();
    for action in actions {
        let method = action.method.to_ascii_lowercase();
        let operation = json!({
            "operationId": action.name,
            "summary": action.name,
            "description": action.description,
            "security": [],
            "x-beater-action": {
                "sideEffect": action.side_effect,
                "confirm": action.confirm,
                "dryRun": action.dry_run,
                "idempotencyRequired": action.idempotency_required,
                "auth": action.auth,
            },
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": action.input_schema,
                    }
                }
            },
            "responses": {
                "200": {"description": "Action result"}
            }
        });
        let entry = paths
            .entry(action.path.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(path_item) = entry {
            path_item.insert(method, operation);
        }
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": app_name,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "servers": [{"url": base_url}],
        "paths": paths,
        "components": {"securitySchemes": {}},
    })
}

fn escape_xml_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn markdown_link_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn markdown_link_destination(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            ' ' => out.push_str("%20"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::RouteMeta;

    fn meta(crawl: bool) -> Option<RouteMeta> {
        Some(RouteMeta {
            title: None,
            description: None,
            crawl,
            actions: Vec::new(),
        })
    }

    fn action() -> crate::mcp::RouteActionTool {
        crate::mcp::RouteActionTool {
            name: "hello.contact".to_string(),
            description: "Send a contact request.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "email": {"type": "string"},
                    "confirm": {"type": "boolean"},
                },
                "required": ["email", "confirm"],
            }),
            method: "POST".to_string(),
            path: "/api/actions/contact".to_string(),
            side_effect: "write".to_string(),
            confirm: true,
            dry_run: false,
            idempotency_required: true,
            auth: serde_json::json!({"type": "public"}),
        }
    }

    #[test]
    fn sitemap_xml_escapes_route_locations() {
        let xml = sitemap_xml(
            "https://example.test/root?x=1&y=2",
            &[(
                "/docs/<private>&notes".to_string(),
                std::path::PathBuf::from("missing-route.tsx"),
                None,
            )],
        );

        assert!(
            xml.contains(
                "<loc>https://example.test/root?x=1&amp;y=2/docs/&lt;private&gt;&amp;notes</loc>"
            ),
            "{xml}"
        );
    }

    #[test]
    fn llms_txt_escapes_markdown_link_text_and_destination() {
        let llms = llms_txt(
            "hello",
            "https://example.test/root",
            &[(
                "/docs/(draft)".to_string(),
                Some(RouteMeta {
                    title: Some("Docs [draft]".to_string()),
                    description: None,
                    crawl: true,
                    actions: Vec::new(),
                }),
            )],
            &[],
            &[],
            &crate::mcp::AccessConfig::default(),
        );

        assert!(llms.contains("- [Docs \\[draft\\]](https://example.test/root/docs/%28draft%29)"));
    }

    #[test]
    fn llms_txt_includes_route_actions() {
        let llms = llms_txt(
            "hello",
            "https://example.test",
            &[],
            &[action()],
            &[],
            &crate::mcp::AccessConfig::default(),
        );

        assert!(llms.contains("## Actions"), "{llms}");
        assert!(
            llms.contains("- [hello.contact](https://example.test/api/actions/contact): Send a contact request. (POST write; confirm: true; idempotency: true)"),
            "{llms}"
        );
    }

    #[test]
    fn well_known_does_not_disclose_trusted_origins() {
        let manifest = well_known(
            "hello",
            "https://hello.example.test",
            &[],
            &[],
            &crate::mcp::AccessConfig::new(
                Some("test-secret".to_string()),
                vec!["https://ops.example.test".to_string()],
            ),
        );

        assert_eq!(manifest["mcp"]["originPolicy"]["noOrigin"], "allowed");
        assert_eq!(manifest["mcp"]["originPolicy"]["loopbackOrigins"], true);
        assert!(
            manifest["mcp"]["originPolicy"]
                .as_object()
                .is_some_and(|policy| !policy.contains_key("trustedOrigins"))
        );
        assert!(!manifest.to_string().contains("https://ops.example.test"));
    }

    #[test]
    fn well_known_includes_openapi_and_route_actions() {
        let manifest = well_known(
            "hello",
            "https://example.test",
            &["support".to_string()],
            &[action()],
            &crate::mcp::AccessConfig::default(),
        );

        assert_eq!(manifest["openapi"], "https://example.test/openapi.json");
        assert_eq!(manifest["actions"][0]["name"], "hello.contact");
        assert_eq!(manifest["actions"][0]["path"], "/api/actions/contact");
        assert_eq!(manifest["actions"][0]["idempotencyRequired"], true);
        assert_eq!(manifest["actions"][0]["auth"]["type"], "public");
    }

    #[test]
    fn openapi_json_groups_route_actions_by_path() {
        let mut delete = action();
        delete.name = "hello.contact.delete".to_string();
        delete.method = "DELETE".to_string();
        let openapi = openapi_json("hello", "https://example.test", &[action(), delete]);

        assert_eq!(openapi["openapi"], "3.1.0");
        assert_eq!(openapi["servers"][0]["url"], "https://example.test");
        assert_eq!(
            openapi["paths"]["/api/actions/contact"]["post"]["operationId"],
            "hello.contact"
        );
        assert_eq!(
            openapi["paths"]["/api/actions/contact"]["delete"]["operationId"],
            "hello.contact.delete"
        );
        assert_eq!(
            openapi["paths"]["/api/actions/contact"]["post"]["x-beater-action"]["idempotencyRequired"],
            true
        );
        assert_eq!(
            openapi["paths"]["/api/actions/contact"]["post"]["requestBody"]["content"]["application/json"]
                ["schema"]["required"][0],
            "email"
        );
    }

    #[test]
    fn robots_txt_allows_all_when_no_routes_disable_crawling() {
        let routes = vec![("/".to_string(), None), ("/docs".to_string(), meta(true))];

        let robots = robots_txt("https://example.test", &routes);

        assert!(robots.contains("Allow: /\n"));
        assert!(!robots.contains("Disallow:"));
        assert!(robots.contains("Sitemap: https://example.test/sitemap.xml"));
    }

    #[test]
    fn robots_txt_disallows_routes_with_crawl_false() {
        let routes = vec![
            ("/".to_string(), None),
            ("/admin".to_string(), meta(false)),
            ("/docs".to_string(), meta(true)),
            ("/admin".to_string(), meta(false)),
        ];

        let robots = robots_txt("https://example.test", &routes);

        assert!(robots.contains("Allow: /\n"));
        assert!(robots.contains("Disallow: /admin\n"));
        assert!(!robots.contains("Disallow: /docs\n"));
        assert_eq!(robots.matches("Disallow: /admin\n").count(), 1);
    }

    #[test]
    fn robots_txt_omits_allow_all_when_root_disallows_crawling() {
        let routes = vec![("/".to_string(), meta(false))];

        let robots = robots_txt("https://example.test", &routes);

        assert!(!robots.contains("Allow: /\n"));
        assert!(robots.contains("Disallow: /\n"));
    }
}
