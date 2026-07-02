//! The Agent Access Layer's crawl half (ARCHITECTURE.md §6b): robots.txt,
//! sitemap.xml, llms.txt, and the .well-known manifest — all generated from
//! the route table and agent registry, never hand-maintained.

use serde_json::json;

use crate::worker::RouteMeta;

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
    agents: &[String],
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
    out.push_str(&format!(
        "\n## For AI agents\n\n- MCP endpoint (tools): {base_url}/mcp (Streamable HTTP, spec {})\n- Manifest: {base_url}/.well-known/beater.json\n",
        crate::mcp::PROTOCOL_VERSION,
    ));
    out
}

pub fn well_known(app_name: &str, base_url: &str, agents: &[String]) -> serde_json::Value {
    json!({
        "name": app_name,
        "framework": {"name": "beater.js", "version": env!("CARGO_PKG_VERSION")},
        "mcp": {
            "endpoint": format!("{base_url}/mcp"),
            "transport": "streamable-http",
            "protocolVersion": crate::mcp::PROTOCOL_VERSION,
        },
        "sitemap": format!("{base_url}/sitemap.xml"),
        "llms": format!("{base_url}/llms.txt"),
        "agents": agents,
    })
}
