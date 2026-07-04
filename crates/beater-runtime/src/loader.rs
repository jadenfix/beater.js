//! ESM module loader for the embedded isolate: reads files, transpiles
//! TS/TSX/JSX via deno_ast (SWC) with inline source maps so stack traces
//! point at the original source.

use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use deno_ast::{
    EmitOptions, JsxAutomaticOptions, JsxRuntime, MediaType, ParseParams, SourceMapOption,
    TranspileOptions,
};
use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleResolveResponse,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind, resolve_import,
};
use deno_error::JsErrorBox;

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
    use super::{load_sync, transpile_cache};
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
        assert!(
            transpile_cache()
                .lock()
                .unwrap()
                .contains_key(&file.canonicalize().unwrap_or(file))
        );
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
}
