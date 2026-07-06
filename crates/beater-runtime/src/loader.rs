//! ESM module loader for the embedded isolate: reads files, transpiles
//! TS/TSX/JSX via deno_ast (SWC) with inline source maps so stack traces
//! point at the original source.

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use deno_ast::{
    EmitOptions, JsxAutomaticOptions, JsxRuntime, MediaType, ParseParams, SourceMapOption,
    TranspileOptions,
};
use deno_core::error::ModuleLoaderError;
use deno_core::{
    resolve_import, ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader,
    ModuleResolveResponse, ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType,
    ResolutionKind,
};
use deno_error::JsErrorBox;
use serde_json::Value;

pub struct BeaterModuleLoader;

#[derive(Clone, PartialEq, Eq)]
struct CacheFingerprint {
    modified: Option<SystemTime>,
    len: u64,
    content_hash: u64,
}

#[derive(Clone)]
struct TranspileCacheEntry {
    fingerprint: CacheFingerprint,
    code: String,
}

static TRANSPILE_CACHE: OnceLock<Mutex<HashMap<PathBuf, TranspileCacheEntry>>> = OnceLock::new();

fn transpile_cache() -> &'static Mutex<HashMap<PathBuf, TranspileCacheEntry>> {
    TRANSPILE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bare specifiers served from vendored, checked-in ESM bundles — the whole
/// "npm resolution" story for the framework's own JS (ARCHITECTURE.md §8).
fn vendor_specifier(specifier: &str) -> Option<&'static str> {
    match specifier {
        "react" => Some("beater:vendor/react"),
        "react/jsx-runtime" | "react/jsx-dev-runtime" => Some("beater:vendor/react-jsx-runtime"),
        "react-dom/server" => Some("beater:vendor/react-dom-server"),
        _ => None,
    }
}

fn vendor_source(specifier: &str) -> Option<&'static str> {
    match specifier {
        "beater:agent" => Some(include_str!("beater_agent.js")),
        "beater:vendor/react" => Some(include_str!("../assets/vendor/react.mjs")),
        "beater:vendor/react-jsx-runtime" => {
            Some(include_str!("../assets/vendor/react-jsx-runtime.mjs"))
        }
        "beater:vendor/react-dom-server" => {
            Some(include_str!("../assets/vendor/react-dom-server.mjs"))
        }
        _ => None,
    }
}

impl ModuleLoader for BeaterModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> ModuleResolveResponse {
        if let Some(mapped) = vendor_specifier(specifier) {
            return ModuleSpecifier::parse(mapped).map_err(JsErrorBox::from_err);
        }
        if let Some(resolved) = resolve_package_import(specifier, referrer)
            .map_err(|error| JsErrorBox::generic(format!("{error:#}")))?
        {
            return Ok(resolved);
        }
        resolve_import(specifier, referrer).map_err(JsErrorBox::from_err)
    }

    fn load(
        &self,
        specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        ModuleLoadResponse::Sync(load_sync(specifier))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PackageImport<'a> {
    package: &'a str,
    subpath: Option<&'a str>,
}

fn resolve_package_import(
    specifier: &str,
    referrer: &str,
) -> anyhow::Result<Option<ModuleSpecifier>> {
    let Some(import) = parse_package_import(specifier) else {
        return Ok(None);
    };
    let referrer = ModuleSpecifier::parse(referrer)?;
    if referrer.scheme() != "file" {
        anyhow::bail!("cannot resolve package import {specifier:?} from {referrer}");
    }
    let referrer_path = referrer
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("bad file referrer for package import: {referrer}"))?;
    let start = referrer_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("package import referrer has no parent: {referrer}"))?;

    for dir in start.ancestors() {
        let package_dir = dir.join("node_modules").join(import.package);
        if package_dir.is_dir() {
            let resolved = resolve_package_dir(&package_dir, import.subpath)?;
            return ModuleSpecifier::from_file_path(&resolved)
                .map(Some)
                .map_err(|_| anyhow::anyhow!("bad package path {}", resolved.display()));
        }
    }

    anyhow::bail!(
        "package import {specifier:?} was not found in node_modules from {}",
        start.display()
    )
}

fn parse_package_import(specifier: &str) -> Option<PackageImport<'_>> {
    if specifier.is_empty()
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with('/')
        || ModuleSpecifier::parse(specifier).is_ok()
    {
        return None;
    }

    if specifier.starts_with('@') {
        let mut parts = specifier.splitn(3, '/');
        let scope = parts.next()?;
        let name = parts.next()?;
        if scope.len() <= 1 || !valid_package_segment(&scope[1..]) || !valid_package_segment(name) {
            return None;
        }
        let package_len = scope.len() + 1 + name.len();
        let subpath = parts.next().filter(|subpath| !subpath.is_empty());
        return Some(PackageImport {
            package: &specifier[..package_len],
            subpath,
        });
    }

    let (package, subpath) = specifier
        .split_once('/')
        .map_or((specifier, None), |(package, subpath)| {
            (package, (!subpath.is_empty()).then_some(subpath))
        });
    if package.is_empty() {
        return None;
    }
    if !valid_package_segment(package) {
        return None;
    }
    Some(PackageImport { package, subpath })
}

fn valid_package_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment
            .chars()
            .any(|ch| matches!(ch, '\\' | '\0') || ch.is_control())
}

fn resolve_package_dir(package_dir: &Path, subpath: Option<&str>) -> anyhow::Result<PathBuf> {
    let package_json = read_package_json(package_dir)?;
    let export_key = subpath.map_or(".".to_string(), |subpath| format!("./{subpath}"));
    if let Some(subpath) = subpath {
        validate_package_relative(package_dir, subpath)?;
    }

    if let Some(exports) = package_json.get("exports") {
        if let Some(target) = resolve_package_export(package_dir, exports, &export_key)? {
            return Ok(target);
        }
        anyhow::bail!(
            "package {} does not export {export_key}",
            package_dir.display()
        );
    }

    if let Some(subpath) = subpath {
        return resolve_package_relative(package_dir, subpath);
    }
    if let Some(module) = package_json.get("module").and_then(Value::as_str) {
        return resolve_package_relative(package_dir, module);
    }
    if let Some(main) = package_json.get("main").and_then(Value::as_str) {
        return resolve_package_relative(package_dir, main);
    }
    resolve_file_or_dir(package_dir.join("index"))
}

fn read_package_json(package_dir: &Path) -> anyhow::Result<Value> {
    let path = package_dir.join("package.json");
    if !path.is_file() {
        return Ok(Value::Object(Default::default()));
    }
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
}

fn resolve_package_export(
    package_dir: &Path,
    exports: &Value,
    export_key: &str,
) -> anyhow::Result<Option<PathBuf>> {
    match exports {
        Value::String(_) if export_key == "." => resolve_export_target(package_dir, exports),
        Value::Object(map) => {
            if let Some(value) = map.get(export_key) {
                return resolve_export_target(package_dir, value);
            }
            let mut best_pattern: Option<(&String, &Value, String)> = None;
            for (key, value) in map {
                let Some(pattern_match) = export_pattern_match(key, export_key) else {
                    continue;
                };
                let is_better = match &best_pattern {
                    Some((best_key, _, _)) => {
                        export_pattern_specificity(key) > export_pattern_specificity(best_key)
                    }
                    None => true,
                };
                if is_better {
                    best_pattern = Some((key, value, pattern_match));
                }
            }
            if let Some((_, value, pattern_match)) = best_pattern {
                return resolve_export_target_with_match(package_dir, value, Some(&pattern_match));
            }
            if export_key == "."
                && map.keys().all(|key| !key.starts_with('.'))
                && let Some(target) = resolve_export_target(package_dir, exports)?
            {
                return Ok(Some(target));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn resolve_export_target(package_dir: &Path, value: &Value) -> anyhow::Result<Option<PathBuf>> {
    resolve_export_target_with_match(package_dir, value, None)
}

fn resolve_export_target_with_match(
    package_dir: &Path,
    value: &Value,
    pattern_match: Option<&str>,
) -> anyhow::Result<Option<PathBuf>> {
    match value {
        Value::String(target) => {
            let target =
                pattern_match.map_or_else(|| target.to_string(), |m| target.replace('*', m));
            resolve_package_relative(package_dir, &target).map(Some)
        }
        Value::Object(map) => {
            for (condition, target) in map {
                if is_active_export_condition(condition)
                    && let Some(resolved) =
                        resolve_export_target_with_match(package_dir, target, pattern_match)?
                {
                    return Ok(Some(resolved));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn export_pattern_match(pattern: &str, export_key: &str) -> Option<String> {
    if !pattern.starts_with("./") {
        return None;
    }
    let (prefix, suffix) = pattern.split_once('*')?;
    if suffix.contains('*') {
        return None;
    }
    if !export_key.starts_with(prefix) || !export_key.ends_with(suffix) {
        return None;
    }
    let match_end = export_key.len().checked_sub(suffix.len())?;
    if match_end <= prefix.len() {
        return None;
    }
    Some(export_key[prefix.len()..match_end].to_string())
}

fn export_pattern_specificity(pattern: &str) -> (usize, usize, usize) {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return (pattern.len(), 0, pattern.len());
    };
    (prefix.len(), suffix.len(), pattern.len())
}

fn is_active_export_condition(condition: &str) -> bool {
    matches!(condition, "node" | "import" | "module" | "default")
}

fn resolve_package_relative(package_dir: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    validate_package_relative(package_dir, raw)?;
    let relative = raw.strip_prefix("./").unwrap_or(raw);
    resolve_file_or_dir(package_dir.join(relative))
}

fn validate_package_relative(package_dir: &Path, raw: &str) -> anyhow::Result<()> {
    let relative = raw.strip_prefix("./").unwrap_or(raw);
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        anyhow::bail!(
            "package {} points outside its package: {raw}",
            package_dir.display()
        );
    }
    Ok(())
}

fn resolve_file_or_dir(path: PathBuf) -> anyhow::Result<PathBuf> {
    if path.is_file() {
        return Ok(path);
    }
    if path.extension().is_none() {
        for ext in ["js", "mjs", "ts", "tsx", "jsx", "json"] {
            let candidate = path.with_extension(ext);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if path.is_dir() {
        let package_json = read_package_json(&path)?;
        if let Some(module) = package_json.get("module").and_then(Value::as_str) {
            return resolve_package_relative(&path, module);
        }
        if let Some(main) = package_json.get("main").and_then(Value::as_str) {
            return resolve_package_relative(&path, main);
        }
        for ext in ["js", "mjs", "ts", "tsx", "jsx", "json"] {
            let candidate = path.join(format!("index.{ext}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!("could not resolve file {}", path.display())
}

fn load_sync(specifier: &ModuleSpecifier) -> Result<ModuleSource, ModuleLoaderError> {
    // framework-provided modules under the beater: scheme
    if specifier.scheme() == "beater" {
        let source = vendor_source(specifier.as_str())
            .ok_or_else(|| JsErrorBox::generic(format!("unknown beater module {specifier}")))?;
        return Ok(ModuleSource::new(
            ModuleType::JavaScript,
            ModuleSourceCode::String(deno_core::FastString::from_static(source)),
            specifier,
            None,
        ));
    }
    let path = specifier
        .to_file_path()
        .map_err(|_| JsErrorBox::generic(format!("not a loadable specifier: {specifier}")))?;
    let media_type = MediaType::from_specifier(specifier);
    let (code, module_type) = match media_type {
        MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => {
            (read_source(&path)?, ModuleType::JavaScript)
        }
        MediaType::Json => (read_source(&path)?, ModuleType::Json),
        MediaType::TypeScript
        | MediaType::Mts
        | MediaType::Cts
        | MediaType::Jsx
        | MediaType::Tsx => (
            transpile_cached(specifier, &path, media_type)
                .map_err(|e| JsErrorBox::generic(format!("transpile {specifier}: {e:#}")))?,
            ModuleType::JavaScript,
        ),
        other => {
            return Err(JsErrorBox::generic(format!(
                "unsupported module type {other:?} for {specifier}"
            )));
        }
    };

    Ok(ModuleSource::new(
        module_type,
        ModuleSourceCode::String(code.into()),
        specifier,
        None,
    ))
}

pub fn transpile_client_module(path: &Path) -> anyhow::Result<String> {
    let specifier = ModuleSpecifier::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("not a loadable client module: {}", path.display()))?;
    let media_type = MediaType::from_specifier(&specifier);
    match media_type {
        MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => {
            Ok(std::fs::read_to_string(path)?)
        }
        MediaType::TypeScript
        | MediaType::Mts
        | MediaType::Cts
        | MediaType::Jsx
        | MediaType::Tsx => transpile_cached(&specifier, path, media_type),
        other => Err(anyhow::anyhow!(
            "unsupported client module type {other:?} for {}",
            path.display()
        )),
    }
}

fn read_source(path: &Path) -> Result<String, ModuleLoaderError> {
    std::fs::read_to_string(path)
        .map_err(|e| JsErrorBox::generic(format!("failed to read {}: {e}", path.display())))
}

fn transpile_cached(
    specifier: &ModuleSpecifier,
    path: &Path,
    media_type: MediaType,
) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)?;
    let source = std::fs::read_to_string(path)?;
    let fingerprint = CacheFingerprint {
        modified: metadata.modified().ok(),
        len: metadata.len(),
        content_hash: source_hash(&source),
    };
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    if let Some(entry) = transpile_cache()
        .lock()
        .expect("transpile cache poisoned")
        .get(&key)
        .filter(|entry| entry.fingerprint == fingerprint)
        .cloned()
    {
        return Ok(entry.code);
    }

    let code = transpile(specifier, source, media_type)?;
    transpile_cache()
        .lock()
        .expect("transpile cache poisoned")
        .insert(
            key,
            TranspileCacheEntry {
                fingerprint,
                code: code.clone(),
            },
        );
    Ok(code)
}

fn source_hash(source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

fn transpile(
    specifier: &ModuleSpecifier,
    code: String,
    media_type: MediaType,
) -> anyhow::Result<String> {
    let parsed = deno_ast::parse_module(ParseParams {
        specifier: specifier.clone(),
        text: code.into(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })?;
    let transpiled = parsed.transpile(
        &TranspileOptions {
            // React 17+ automatic JSX runtime; resolved from the vendored
            // assets in M4. Harmless for plain TS.
            jsx: Some(JsxRuntime::Automatic(JsxAutomaticOptions {
                development: false,
                import_source: Some("react".to_string()),
            })),
            ..Default::default()
        },
        &Default::default(),
        &EmitOptions {
            source_map: SourceMapOption::Inline,
            ..Default::default()
        },
    )?;
    Ok(transpiled.into_source().text)
}

#[cfg(test)]
mod tests {
    use super::{load_sync, parse_package_import, resolve_package_import, transpile_cache};
    use deno_core::{ModuleSource, ModuleSourceCode, ModuleSpecifier};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-loader-{name}-{}-{}",
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

    fn source_text(source: ModuleSource) -> String {
        match source.code {
            ModuleSourceCode::String(text) => text.to_string(),
            ModuleSourceCode::Bytes(bytes) => String::from_utf8(bytes.to_vec()).unwrap(),
        }
    }

    #[test]
    fn transpile_cache_reuses_unchanged_ts_source() {
        let dir = TempDir::new("reuse");
        let file = dir.path().join("route.ts");
        fs::write(&file, "export const answer: number = 41;\n").unwrap();
        let specifier = ModuleSpecifier::from_file_path(&file).unwrap();

        let first = source_text(load_sync(&specifier).unwrap());
        let second = source_text(load_sync(&specifier).unwrap());

        assert!(first.contains("answer"));
        assert_eq!(first, second);
        assert!(transpile_cache()
            .lock()
            .unwrap()
            .contains_key(&file.canonicalize().unwrap_or(file)));
    }

    #[test]
    fn transpile_cache_invalidates_after_file_change() {
        let dir = TempDir::new("invalidate");
        let file = dir.path().join("route.ts");
        fs::write(&file, "export const label: string = 'old';\n").unwrap();
        let specifier = ModuleSpecifier::from_file_path(&file).unwrap();

        let first = source_text(load_sync(&specifier).unwrap());
        fs::write(&file, "export const label: string = 'newer';\n").unwrap();
        let second = source_text(load_sync(&specifier).unwrap());

        assert!(first.contains("old"));
        assert!(second.contains("newer"));
    }

    #[test]
    fn transpile_cache_invalidates_same_length_edits() {
        let dir = TempDir::new("same-length");
        let file = dir.path().join("route.ts");
        fs::write(&file, "export const label: string = 'old';\n").unwrap();
        let specifier = ModuleSpecifier::from_file_path(&file).unwrap();

        let first = source_text(load_sync(&specifier).unwrap());
        fs::write(&file, "export const label: string = 'new';\n").unwrap();
        let second = source_text(load_sync(&specifier).unwrap());

        assert_ne!(first, second);
        assert!(first.contains("old"));
        assert!(second.contains("new"));
    }

    #[test]
    fn transpile_client_module_accepts_route_scoped_ts() {
        let dir = TempDir::new("client");
        let file = dir.path().join("index.client.ts");
        fs::write(
            &file,
            "const count: number = 1;\ndocument.body.dataset.count = String(count);\n",
        )
        .unwrap();

        let code = super::transpile_client_module(&file).unwrap();

        assert!(code.contains("document.body.dataset.count"));
        assert!(!code.contains(": number"));
    }

    #[test]
    fn package_import_parser_handles_scoped_and_subpath_specifiers() {
        assert_eq!(
            parse_package_import("zod"),
            Some(super::PackageImport {
                package: "zod",
                subpath: None
            })
        );
        assert_eq!(
            parse_package_import("zod/v4"),
            Some(super::PackageImport {
                package: "zod",
                subpath: Some("v4")
            })
        );
        assert_eq!(
            parse_package_import("@scope/pkg/sub/module"),
            Some(super::PackageImport {
                package: "@scope/pkg",
                subpath: Some("sub/module")
            })
        );
        assert!(parse_package_import("./local").is_none());
        assert!(parse_package_import("https://example.test/mod.js").is_none());
        assert!(parse_package_import("@scope/../pkg").is_none());
        assert!(parse_package_import("@scope/./pkg").is_none());
        assert!(parse_package_import("../pkg").is_none());
    }

    #[test]
    fn package_import_resolves_exports_import_condition_from_node_modules() {
        let app = TempDir::new("package-export");
        app.write("app/routes/api/schema.ts", "import { z } from 'zod';\n");
        app.write(
            "node_modules/zod/package.json",
            r#"{
  "name": "zod",
  "type": "module",
  "exports": {
    ".": {
      "types": "./index.d.ts",
      "import": "./index.js",
      "require": "./index.cjs"
    }
  }
}"#,
        );
        app.write(
            "node_modules/zod/index.js",
            "export const z = { string: () => 'ok' };\n",
        );
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let resolved = resolve_package_import("zod", referrer.as_str())
            .unwrap()
            .unwrap();

        assert_eq!(
            resolved,
            ModuleSpecifier::from_file_path(app.path().join("node_modules/zod/index.js")).unwrap()
        );
    }

    #[test]
    fn package_import_respects_condition_object_order() {
        let app = TempDir::new("package-condition-order");
        app.write(
            "app/routes/api/schema.ts",
            "import first from 'first';\nimport second from 'second';\n",
        );
        app.write(
            "node_modules/first/package.json",
            r#"{
  "name": "first",
  "type": "module",
  "exports": {
    ".": {
      "default": "./default.js",
      "import": "./import.js"
    }
  }
}"#,
        );
        app.write(
            "node_modules/first/default.js",
            "export default 'default';\n",
        );
        app.write("node_modules/first/import.js", "export default 'import';\n");
        app.write(
            "node_modules/second/package.json",
            r#"{
  "name": "second",
  "type": "module",
  "exports": {
    ".": {
      "import": "./import.js",
      "default": "./default.js"
    }
  }
}"#,
        );
        app.write(
            "node_modules/second/default.js",
            "export default 'default';\n",
        );
        app.write(
            "node_modules/second/import.js",
            "export default 'import';\n",
        );
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let first = resolve_package_import("first", referrer.as_str())
            .unwrap()
            .unwrap();
        let second = resolve_package_import("second", referrer.as_str())
            .unwrap()
            .unwrap();

        assert_eq!(
            first,
            ModuleSpecifier::from_file_path(app.path().join("node_modules/first/default.js"))
                .unwrap()
        );
        assert_eq!(
            second,
            ModuleSpecifier::from_file_path(app.path().join("node_modules/second/import.js"))
                .unwrap()
        );
    }

    #[test]
    fn package_import_uses_server_conditions() {
        let app = TempDir::new("package-server-conditions");
        app.write(
            "app/routes/api/schema.ts",
            "import withNode from 'with-node';\nimport withBrowser from 'with-browser';\n",
        );
        app.write(
            "node_modules/with-node/package.json",
            r#"{
  "name": "with-node",
  "type": "module",
  "exports": {
    ".": {
      "browser": "./browser.js",
      "node": "./node.js",
      "import": "./import.js",
      "default": "./default.js"
    }
  }
}"#,
        );
        app.write(
            "node_modules/with-node/browser.js",
            "export default 'browser';\n",
        );
        app.write("node_modules/with-node/node.js", "export default 'node';\n");
        app.write(
            "node_modules/with-node/import.js",
            "export default 'import';\n",
        );
        app.write(
            "node_modules/with-node/default.js",
            "export default 'default';\n",
        );
        app.write(
            "node_modules/with-browser/package.json",
            r#"{
  "name": "with-browser",
  "type": "module",
  "exports": {
    ".": {
      "browser": "./browser.js",
      "import": "./import.js",
      "default": "./default.js"
    }
  }
}"#,
        );
        app.write(
            "node_modules/with-browser/browser.js",
            "export default 'browser';\n",
        );
        app.write(
            "node_modules/with-browser/import.js",
            "export default 'import';\n",
        );
        app.write(
            "node_modules/with-browser/default.js",
            "export default 'default';\n",
        );
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let with_node = resolve_package_import("with-node", referrer.as_str())
            .unwrap()
            .unwrap();
        let with_browser = resolve_package_import("with-browser", referrer.as_str())
            .unwrap()
            .unwrap();

        assert_eq!(
            with_node,
            ModuleSpecifier::from_file_path(app.path().join("node_modules/with-node/node.js"))
                .unwrap()
        );
        assert_eq!(
            with_browser,
            ModuleSpecifier::from_file_path(app.path().join("node_modules/with-browser/import.js"))
                .unwrap()
        );
    }

    #[test]
    fn package_import_resolves_wildcard_subpath_exports() {
        let app = TempDir::new("package-wildcard-exports");
        app.write(
            "app/routes/api/schema.ts",
            "import add from 'fixture/features/add';\nimport exact from 'fixture/features/exact';\n",
        );
        app.write(
            "node_modules/fixture/package.json",
            r#"{
  "name": "fixture",
  "type": "module",
  "exports": {
    "./features/exact": "./exact.js",
    "./*": "./dist/*.js"
  }
}"#,
        );
        app.write(
            "node_modules/fixture/dist/features/add.js",
            "export default 'wildcard';\n",
        );
        app.write("node_modules/fixture/exact.js", "export default 'exact';\n");
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let wildcard = resolve_package_import("fixture/features/add", referrer.as_str())
            .unwrap()
            .unwrap();
        let exact = resolve_package_import("fixture/features/exact", referrer.as_str())
            .unwrap()
            .unwrap();

        assert_eq!(
            wildcard,
            ModuleSpecifier::from_file_path(
                app.path().join("node_modules/fixture/dist/features/add.js")
            )
            .unwrap()
        );
        assert_eq!(
            exact,
            ModuleSpecifier::from_file_path(app.path().join("node_modules/fixture/exact.js"))
                .unwrap()
        );
    }

    #[test]
    fn package_import_applies_conditions_to_wildcard_exports() {
        let app = TempDir::new("package-wildcard-condition");
        app.write(
            "app/routes/api/schema.ts",
            "import feature from 'fixture/features/math';\n",
        );
        app.write(
            "node_modules/fixture/package.json",
            r#"{
  "name": "fixture",
  "type": "module",
  "exports": {
    "./features/*": {
      "browser": "./browser/*.js",
      "node": "./server/*.js",
      "default": "./default/*.js"
    }
  }
}"#,
        );
        app.write(
            "node_modules/fixture/browser/math.js",
            "export default 'browser';\n",
        );
        app.write(
            "node_modules/fixture/server/math.js",
            "export default 'node';\n",
        );
        app.write(
            "node_modules/fixture/default/math.js",
            "export default 'default';\n",
        );
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let resolved = resolve_package_import("fixture/features/math", referrer.as_str())
            .unwrap()
            .unwrap();

        assert_eq!(
            resolved,
            ModuleSpecifier::from_file_path(app.path().join("node_modules/fixture/server/math.js"))
                .unwrap()
        );
    }

    #[test]
    fn package_import_rejects_wildcard_export_target_escape() {
        let app = TempDir::new("package-wildcard-escape");
        app.write(
            "app/routes/api/schema.ts",
            "import value from 'fixture/private';\n",
        );
        app.write(
            "node_modules/fixture/package.json",
            r#"{
  "name": "fixture",
  "type": "module",
  "exports": {
    "./*": "./../*.js"
  }
}"#,
        );
        app.write("node_modules/private.js", "export default 'outside';\n");
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let error = resolve_package_import("fixture/private", referrer.as_str()).unwrap_err();

        assert!(
            error.to_string().contains("points outside its package"),
            "{error:#}"
        );
    }

    #[test]
    fn package_import_rejects_empty_wildcard_export_match() {
        let app = TempDir::new("package-wildcard-empty");
        app.write(
            "app/routes/api/schema.ts",
            "import value from 'fixture/features/';\n",
        );
        app.write(
            "node_modules/fixture/package.json",
            r#"{
  "name": "fixture",
  "type": "module",
  "exports": {
    "./features/*": "./dist/*.js"
  }
}"#,
        );
        app.write(
            "node_modules/fixture/dist/.js",
            "export default 'hidden';\n",
        );
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let error = resolve_package_import("fixture/features/", referrer.as_str()).unwrap_err();

        assert!(
            error.to_string().contains("does not export ./features/"),
            "{error:#}"
        );
    }

    #[test]
    fn package_import_reports_missing_node_modules_package() {
        let app = TempDir::new("package-missing");
        app.write("app/routes/api/schema.ts", "import { z } from 'zod';\n");
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let error = resolve_package_import("zod", referrer.as_str()).unwrap_err();

        assert!(
            error.to_string().contains("was not found in node_modules"),
            "{error:#}"
        );
    }

    #[test]
    fn package_import_rejects_subpath_escape_without_exports_map() {
        let app = TempDir::new("package-subpath-escape");
        app.write(
            "app/routes/api/schema.ts",
            "import value from 'fixture/../../outside.js';\n",
        );
        app.write("node_modules/fixture/package.json", r#"{"name":"fixture"}"#);
        app.write("outside.js", "export default 'outside';\n");
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        let error =
            resolve_package_import("fixture/../../outside.js", referrer.as_str()).unwrap_err();

        assert!(
            error.to_string().contains("points outside its package"),
            "{error:#}"
        );
    }

    #[test]
    fn package_import_rejects_scoped_package_name_escape() {
        let app = TempDir::new("package-scoped-name-escape");
        app.write(
            "app/routes/api/schema.ts",
            "import value from '@scope/../fixture/private.js';\n",
        );
        fs::create_dir_all(app.path().join("node_modules/@scope")).unwrap();
        app.write("node_modules/fixture/package.json", r#"{"name":"fixture"}"#);
        app.write(
            "node_modules/fixture/private.js",
            "export default 'private';\n",
        );
        let referrer =
            ModuleSpecifier::from_file_path(app.path().join("app/routes/api/schema.ts")).unwrap();

        assert!(
            resolve_package_import("@scope/../fixture/private.js", referrer.as_str())
                .unwrap()
                .is_none()
        );
    }
}
