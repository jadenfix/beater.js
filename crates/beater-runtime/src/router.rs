use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// `.ts`/`.js` — exports HTTP method handlers (GET, POST, ...).
    Api,
    /// `.tsx`/`.jsx` — exports a default React component (SSR, M4).
    Page,
}

#[derive(Debug, Clone)]
pub enum Segment {
    Static(String),
    /// `[id]` in the filename.
    Param(String),
}

#[derive(Debug, Clone)]
pub struct Route {
    pub segments: Vec<Segment>,
    pub file: PathBuf,
    pub kind: RouteKind,
    /// Display form, e.g. `/users/[id]`.
    pub pattern: String,
}

#[derive(Debug, Default)]
pub struct RouteTable {
    routes: Vec<Route>,
}

impl RouteTable {
    /// Scan `<app_dir>/app/routes/**` into a route table.
    ///
    /// `index.*` maps to the directory itself; `[name]` segments are dynamic.
    pub fn scan(app_dir: &Path) -> Result<Self> {
        let routes_dir = app_dir.join("app").join("routes");
        let mut routes = Vec::new();
        if routes_dir.is_dir() {
            for entry in walkdir::WalkDir::new(&routes_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                let path = entry.path();
                if path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.ends_with(".client") || stem.ends_with(".server"))
                {
                    continue;
                }
                let kind = match path.extension().and_then(|e| e.to_str()) {
                    Some("ts") | Some("js") | Some("mjs") => RouteKind::Api,
                    Some("tsx") | Some("jsx") => RouteKind::Page,
                    _ => continue,
                };
                let rel = path.strip_prefix(&routes_dir)?.with_extension("");
                let mut segments = Vec::new();
                let mut pattern = String::new();
                let components: Vec<_> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                for (i, comp) in components.iter().enumerate() {
                    // trailing `index` maps to the parent directory
                    if i == components.len() - 1 && comp == "index" {
                        break;
                    }
                    pattern.push('/');
                    pattern.push_str(comp);
                    if let Some(name) = comp.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                        segments.push(Segment::Param(name.to_string()));
                    } else {
                        segments.push(Segment::Static(comp.clone()));
                    }
                }
                if pattern.is_empty() {
                    pattern.push('/');
                }
                routes.push(Route {
                    segments,
                    file: path.to_path_buf(),
                    kind,
                    pattern,
                });
            }
        }
        let mut routes_by_pattern: Vec<_> = routes.iter().collect();
        routes_by_pattern.sort_by(|left, right| {
            left.pattern
                .cmp(&right.pattern)
                .then_with(|| left.file.cmp(&right.file))
        });
        for pair in routes_by_pattern.windows(2) {
            let [left, right] = pair else {
                continue;
            };
            if left.pattern == right.pattern {
                bail!(
                    "duplicate route pattern {} maps to both {} and {}",
                    left.pattern,
                    left.file.display(),
                    right.file.display()
                );
            }
        }
        // Static segments win over params at each depth; ties are stable across
        // filesystems so route precedence does not depend on walk order.
        routes.sort_by(|left, right| {
            left.segments
                .iter()
                .zip(&right.segments)
                .map(|(left, right)| segment_rank(left).cmp(&segment_rank(right)))
                .find(|ordering| !ordering.is_eq())
                .unwrap_or_else(|| {
                    left.segments
                        .len()
                        .cmp(&right.segments.len())
                        .then_with(|| left.pattern.cmp(&right.pattern))
                        .then_with(|| left.file.cmp(&right.file))
                })
        });
        Ok(Self { routes })
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Route> {
        self.routes.iter()
    }

    /// Match a URL path, returning the route and extracted params.
    pub fn match_path(&self, path: &str) -> Option<(&Route, HashMap<String, String>)> {
        let parts = decoded_path_segments(path)?;
        'route: for route in &self.routes {
            if route.segments.len() != parts.len() {
                continue;
            }
            let mut params = HashMap::new();
            for (seg, part) in route.segments.iter().zip(&parts) {
                match seg {
                    Segment::Static(s) if s == part => {}
                    Segment::Static(_) => continue 'route,
                    Segment::Param(name) => {
                        params.insert(name.clone(), part.clone());
                    }
                }
            }
            return Some((route, params));
        }
        None
    }
}

fn segment_rank(segment: &Segment) -> (u8, &str) {
    match segment {
        Segment::Static(value) => (0, value.as_str()),
        Segment::Param(value) => (1, value.as_str()),
    }
}

fn decoded_path_segments(path: &str) -> Option<Vec<String>> {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(percent_decode_path_segment)
        .collect()
}

fn percent_decode_path_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = *bytes.get(index + 1)?;
                let low = *bytes.get(index + 2)?;
                let byte = hex_byte(high, low)?;
                if matches!(byte, b'/' | b'\0') {
                    return None;
                }
                out.push(byte);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn hex_byte(high: u8, low: u8) -> Option<u8> {
    Some((hex_digit(high)? << 4) | hex_digit(low)?)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{RouteKind, RouteTable};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-router-{name}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn write(&self, rel: &str, contents: &str) {
            let path = self.path.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn scans_index_api_page_and_param_routes() {
        let app = TempDir::new("scan");
        app.write("app/routes/index.tsx", "export default function Home() {}");
        app.write("app/routes/api/health.ts", "export function GET() {}");
        app.write("app/routes/index.client.ts", "console.log('client')");
        app.write(
            "app/routes/index.server.tsx",
            "export default function Rsc() {}",
        );
        app.write(
            "app/routes/users/[id].tsx",
            "export default function User() {}",
        );
        app.write("app/routes/users/[id].client.ts", "console.log('client')");
        app.write(
            "app/routes/users/[id].server.tsx",
            "export default function Rsc() {}",
        );
        app.write("app/routes/ignored.css", "body {}");

        let table = RouteTable::scan(app.path()).unwrap();
        let patterns: Vec<_> = table.iter().map(|route| route.pattern.as_str()).collect();

        assert!(patterns.contains(&"/"));
        assert!(patterns.contains(&"/api/health"));
        assert!(patterns.contains(&"/users/[id]"));
        assert!(!patterns.contains(&"/index.client"));
        assert!(!patterns.contains(&"/index.server"));
        assert!(!patterns.contains(&"/users/[id].client"));
        assert!(!patterns.contains(&"/users/[id].server"));
        assert_eq!(patterns.len(), 3);

        let (route, _) = table.match_path("/").unwrap();
        assert_eq!(route.kind, RouteKind::Page);
        let (route, _) = table.match_path("/api/health").unwrap();
        assert_eq!(route.kind, RouteKind::Api);
        let (_, params) = table.match_path("/users/42").unwrap();
        assert_eq!(params.get("id").map(String::as_str), Some("42"));
    }

    #[test]
    fn static_routes_win_over_dynamic_collisions() {
        let app = TempDir::new("collision");
        app.write(
            "app/routes/users/[id].tsx",
            "export default function User() {}",
        );
        app.write(
            "app/routes/users/settings.tsx",
            "export default function Settings() {}",
        );

        let table = RouteTable::scan(app.path()).unwrap();
        let (route, params) = table.match_path("/users/settings").unwrap();

        assert_eq!(route.pattern, "/users/settings");
        assert!(params.is_empty());
    }

    #[test]
    fn static_segments_win_at_the_same_depth_before_total_param_count() {
        let app = TempDir::new("specificity");
        app.write(
            "app/routes/[tenant]/profile/edit.tsx",
            "export default function TenantProfileEdit() {}",
        );
        app.write(
            "app/routes/users/[id]/[tab].tsx",
            "export default function UserTab() {}",
        );

        let table = RouteTable::scan(app.path()).unwrap();
        let (route, params) = table.match_path("/users/profile/edit").unwrap();

        assert_eq!(route.pattern, "/users/[id]/[tab]");
        assert_eq!(params.get("id").map(String::as_str), Some("profile"));
        assert_eq!(params.get("tab").map(String::as_str), Some("edit"));
    }

    #[test]
    fn rejects_duplicate_patterns_across_api_and_page_routes() {
        let app = TempDir::new("duplicate-api-page");
        app.write("app/routes/about.ts", "export function GET() {}");
        app.write("app/routes/about.tsx", "export default function About() {}");

        let err = RouteTable::scan(app.path()).unwrap_err().to_string();

        assert!(err.contains("duplicate route pattern /about"), "{err}");
        assert!(err.contains("about.ts"), "{err}");
        assert!(err.contains("about.tsx"), "{err}");
    }

    #[test]
    fn rejects_duplicate_patterns_across_file_and_index_routes() {
        let app = TempDir::new("duplicate-index");
        app.write("app/routes/users.tsx", "export default function Users() {}");
        app.write(
            "app/routes/users/index.tsx",
            "export default function UsersIndex() {}",
        );

        let err = RouteTable::scan(app.path()).unwrap_err().to_string();

        assert!(err.contains("duplicate route pattern /users"), "{err}");
        assert!(err.contains("users.tsx"), "{err}");
        assert!(err.contains("users/index.tsx"), "{err}");
    }

    #[test]
    fn percent_decodes_static_path_segments_and_params() {
        let app = TempDir::new("percent");
        app.write("app/routes/api/health.ts", "export function GET() {}");
        app.write(
            "app/routes/users/[id].tsx",
            "export default function User() {}",
        );
        let table = RouteTable::scan(app.path()).unwrap();

        let (route, _) = table.match_path("/api/he%61lth").unwrap();
        assert_eq!(route.pattern, "/api/health");

        let (_, params) = table.match_path("/users/John%20Doe").unwrap();
        assert_eq!(params.get("id").map(String::as_str), Some("John Doe"));

        let (_, params) = table.match_path("/users/Ol%C3%A9").unwrap();
        assert_eq!(params.get("id").map(String::as_str), Some("Olé"));
    }

    #[test]
    fn rejects_path_segment_decode_escape_and_utf8_failures() {
        let app = TempDir::new("bad-percent");
        app.write(
            "app/routes/users/[id].tsx",
            "export default function User() {}",
        );
        let table = RouteTable::scan(app.path()).unwrap();

        for path in [
            "/users/a%2Fb",
            "/users/a%2fb",
            "/users/a%00b",
            "/users/%",
            "/users/%2",
            "/users/%GG",
            "/users/%FF",
        ] {
            assert!(table.match_path(path).is_none(), "{path}");
        }
    }

    #[test]
    fn rejects_wrong_depth_or_unknown_path() {
        let app = TempDir::new("miss");
        app.write(
            "app/routes/users/[id].tsx",
            "export default function User() {}",
        );
        let table = RouteTable::scan(app.path()).unwrap();

        assert!(table.match_path("/users").is_none());
        assert!(table.match_path("/users/1/extra").is_none());
        assert!(table.match_path("/posts/1").is_none());
    }
}
