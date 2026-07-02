use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

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
                    .is_some_and(|stem| stem.ends_with(".client"))
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
        // static segments win over params at the same depth
        routes.sort_by_key(|r| {
            r.segments
                .iter()
                .filter(|s| matches!(s, Segment::Param(_)))
                .count()
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
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
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
                        params.insert(name.clone(), (*part).to_string());
                    }
                }
            }
            return Some((route, params));
        }
        None
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
            "app/routes/users/[id].tsx",
            "export default function User() {}",
        );
        app.write("app/routes/users/[id].client.ts", "console.log('client')");
        app.write("app/routes/ignored.css", "body {}");

        let table = RouteTable::scan(app.path()).unwrap();
        let patterns: Vec<_> = table.iter().map(|route| route.pattern.as_str()).collect();

        assert!(patterns.contains(&"/"));
        assert!(patterns.contains(&"/api/health"));
        assert!(patterns.contains(&"/users/[id]"));
        assert!(!patterns.contains(&"/index.client"));
        assert!(!patterns.contains(&"/users/[id].client"));
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
