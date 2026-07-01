//! `dylib-hook-registry` — coordination registry for multiple injected dylib hooks.
//!
//! When multiple dylibs are injected into a host process (e.g., via `insert_dylib`
//! or `DYLD_INSERT_LIBRARIES`), they need to coordinate to avoid conflicts —
//! especially when two hooks want to `replace()` the same function symbol.
//!
//! This crate provides a file-based registry that each hook reads on init and
//! writes after installing its detours. The patcher tool reads it to know what's
//! already injected.
//!
//! # Registry Location
//!
//! Default: `~/.config/dylib-hooks/{app_id}/registry.json`
//!
//! The `app_id` isolates registries per host app (e.g., `zed-preview`, `zed-stable`).
//!
//! # Usage
//!
//! ```no_run
//! use dylib_hook_registry::{HookRegistry, HookEntry};
//!
//! // In hook's ctor (on dylib load):
//! let mut reg = HookRegistry::load("zed-preview").unwrap_or_default();
//!
//! // Check for conflicts before installing
//! if let Some(conflict) = reg.find_replace_conflict("sqlite3_prepare_v2", "my-hook") {
//!     eprintln!("Warning: '{}' already replaces this symbol, chaining", conflict);
//! }
//!
//! // After installing detour, register ourselves
//! reg.register(
//!     HookEntry::new("my-hook", "/path/to/hook.dylib")
//!         .with_symbol("sqlite3_prepare_v2", "replace", "detect workspace writes")
//!         .with_version("0.1.0")
//!         .with_load_order(2)
//! );
//! reg.save("zed-preview").ok();
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const REGISTRY_FILENAME: &str = "registry.json";
const CONFIG_DIR_NAME: &str = "dylib-hooks";

/// The hook registry file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRegistry {
    pub schema_version: u32,
    /// Host application identifier (e.g., "zed-preview", "my-app")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Path to the host application binary
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_patched: Option<String>,
    pub hooks: Vec<HookEntry>,
}

/// A single hook entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    pub name: String,
    pub dylib_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub hooked_symbols: Vec<HookedSymbol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_order: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    /// Health check: how to verify this hook is loaded and working after app restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,
    /// Build artifact identity: sha256 hash + size of the dylib at patch time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactInfo>,
    /// Extra fields preserved on round-trip
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Identity of a dylib artifact at patch time.
/// Used to detect if the dylib on disk has been rebuilt since it was injected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    /// SHA-256 hex digest of the dylib file.
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
    /// When this dylib was patched into the host binary (ISO 8601).
    pub patched_at: String,
    /// Git commit hash of the hook project at build time (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
}

/// How to verify a hook is loaded and working after the host app restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    /// Log file glob pattern (e.g., "~/Library/Logs/Zed/zed-yolo-hook.*.log")
    pub log_glob: String,
    /// Strings that MUST appear in the log after a fresh app start (all must match).
    /// Checked in order — earlier markers should appear before later ones.
    pub success_markers: Vec<String>,
    /// Strings that indicate FAILURE (any match = hook broken).
    #[serde(default)]
    pub failure_markers: Vec<String>,
    /// Max seconds to wait for markers after app launch.
    #[serde(default = "default_verify_timeout")]
    pub timeout_secs: u32,
}

fn default_verify_timeout() -> u32 {
    10
}

/// A symbol that a hook intercepts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookedSymbol {
    pub symbol: String,
    /// `"attach"` (listener, non-replacing) or `"replace"` (detour, replaces original)
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// --- Registry paths ---

/// Get the registry directory for an app.
/// `~/.config/dylib-hooks/{app_id}/`
fn registry_dir(app_id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".config").join(CONFIG_DIR_NAME).join(app_id))
}

/// Get the registry file path for an app.
fn registry_path(app_id: &str) -> Option<PathBuf> {
    Some(registry_dir(app_id)?.join(REGISTRY_FILENAME))
}

// --- HookRegistry ---

impl HookRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            schema_version: 1,
            app_id: None,
            host_app: None,
            last_patched: None,
            hooks: Vec::new(),
        }
    }

    /// Load from the default location for an app.
    pub fn load(app_id: &str) -> Option<Self> {
        let path = registry_path(app_id)?;
        Self::load_from(&path)
    }

    /// Load from a specific path.
    pub fn load_from(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save to the default location for an app.
    pub fn save(&self, app_id: &str) -> std::io::Result<()> {
        let path = registry_path(app_id)
            .ok_or_else(|| std::io::Error::other("cannot determine registry path"))?;
        self.save_to(&path)
    }

    /// Save to a specific path.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Register or update a hook entry by name.
    pub fn register(&mut self, entry: HookEntry) {
        if let Some(existing) = self.hooks.iter_mut().find(|h| h.name == entry.name) {
            *existing = entry;
        } else {
            self.hooks.push(entry);
        }
    }

    /// Atomically load → register → save with file locking.
    ///
    /// Prevents race conditions when multiple hooks' `#[ctor]` functions run
    /// concurrently in different processes (Zed spawns multiple processes, each
    /// loading all injected dylibs). Without locking, concurrent read-modify-write
    /// cycles can overwrite each other's entries.
    pub fn locked_register(app_id: &str, entry: HookEntry) -> std::io::Result<()> {
        let path = registry_path(app_id)
            .ok_or_else(|| std::io::Error::other("cannot determine registry path"))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let lock_path = path.with_extension("lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;

        use fs2::FileExt;
        // Block up to ~5 seconds waiting for the lock
        lock_file
            .lock_exclusive()
            .map_err(|e| std::io::Error::other(format!("registry lock failed: {e}")))?;

        let result = (|| {
            let mut reg = Self::load_from(&path).unwrap_or_default();
            reg.app_id = Some(app_id.to_string());
            reg.register(entry);
            reg.save_to(&path)
        })();

        let _ = lock_file.unlock();
        result
    }

    /// Remove a hook entry by name. Returns true if found and removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.hooks.len();
        self.hooks.retain(|h| h.name != name);
        self.hooks.len() != before
    }

    /// Find a hook by name.
    pub fn find(&self, name: &str) -> Option<&HookEntry> {
        self.hooks.iter().find(|h| h.name == name)
    }

    /// Check if a symbol has a conflicting `replace` hook.
    ///
    /// Returns the conflicting hook name if another hook (not `my_hook_name`)
    /// uses `replace` on the same symbol.
    pub fn find_replace_conflict(&self, symbol: &str, my_hook_name: &str) -> Option<&str> {
        for hook in &self.hooks {
            if hook.name == my_hook_name {
                continue;
            }
            for hs in &hook.hooked_symbols {
                if hs.symbol == symbol && hs.method == "replace" {
                    return Some(&hook.name);
                }
            }
        }
        None
    }

    /// Get all hooks sorted by load_order (lowest first).
    pub fn hooks_by_load_order(&self) -> Vec<&HookEntry> {
        let mut sorted: Vec<&HookEntry> = self.hooks.iter().collect();
        sorted.sort_by_key(|h| h.load_order.unwrap_or(u32::MAX));
        sorted
    }

    /// List all hooked symbols across all hooks.
    pub fn all_hooked_symbols(&self) -> Vec<(&str, &str, &str)> {
        self.hooks
            .iter()
            .flat_map(|h| {
                h.hooked_symbols
                    .iter()
                    .map(move |s| (h.name.as_str(), s.symbol.as_str(), s.method.as_str()))
            })
            .collect()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// --- HookEntry ---

impl HookEntry {
    /// Create a new hook entry.
    pub fn new(name: &str, dylib_path: &str) -> Self {
        Self {
            name: name.to_string(),
            dylib_path: dylib_path.to_string(),
            version: None,
            features: Vec::new(),
            hooked_symbols: Vec::new(),
            load_order: None,
            installed_at: None,
            log_path: None,
            health_check: None,
            artifact: None,
            extra: serde_json::Map::new(),
        }
    }

    pub fn with_symbol(mut self, symbol: &str, method: &str, description: &str) -> Self {
        self.hooked_symbols.push(HookedSymbol {
            symbol: symbol.to_string(),
            method: method.to_string(),
            description: Some(description.to_string()),
        });
        self
    }

    pub fn with_features(mut self, features: &[&str]) -> Self {
        self.features = features.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_version(mut self, version: &str) -> Self {
        self.version = Some(version.to_string());
        self
    }

    pub fn with_load_order(mut self, order: u32) -> Self {
        self.load_order = Some(order);
        self
    }

    pub fn with_log_path(mut self, path: &str) -> Self {
        self.log_path = Some(path.to_string());
        self
    }

    pub fn with_health_check(mut self, check: HealthCheck) -> Self {
        self.health_check = Some(check);
        self
    }
}

impl HealthCheck {
    pub fn new(log_glob: &str) -> Self {
        Self {
            log_glob: log_glob.to_string(),
            success_markers: Vec::new(),
            failure_markers: Vec::new(),
            timeout_secs: default_verify_timeout(),
        }
    }

    pub fn with_success(mut self, marker: &str) -> Self {
        self.success_markers.push(marker.to_string());
        self
    }

    pub fn with_failure(mut self, marker: &str) -> Self {
        self.failure_markers.push(marker.to_string());
        self
    }

    pub fn with_timeout(mut self, secs: u32) -> Self {
        self.timeout_secs = secs;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_registry_is_empty() {
        let reg = HookRegistry::new();
        assert_eq!(reg.schema_version, 1);
        assert!(reg.hooks.is_empty());
    }

    #[test]
    fn register_and_find() {
        let mut reg = HookRegistry::new();
        reg.register(
            HookEntry::new("test-hook", "/path/to/hook.dylib")
                .with_version("0.1.0")
                .with_features(&["feature-a"])
                .with_symbol("sqlite3_prepare_v2", "replace", "test")
                .with_load_order(1),
        );
        assert_eq!(reg.hooks.len(), 1);
        assert!(reg.find("test-hook").is_some());
        assert!(reg.find("nonexistent").is_none());
    }

    #[test]
    fn register_updates_existing() {
        let mut reg = HookRegistry::new();
        reg.register(HookEntry::new("hook-a", "/v1.dylib").with_version("1.0"));
        reg.register(HookEntry::new("hook-a", "/v2.dylib").with_version("2.0"));
        assert_eq!(reg.hooks.len(), 1);
        assert_eq!(reg.find("hook-a").unwrap().dylib_path, "/v2.dylib");
    }

    #[test]
    fn remove_hook() {
        let mut reg = HookRegistry::new();
        reg.register(HookEntry::new("a", "/a.dylib"));
        reg.register(HookEntry::new("b", "/b.dylib"));
        assert!(reg.remove("a"));
        assert_eq!(reg.hooks.len(), 1);
        assert!(!reg.remove("a"));
    }

    #[test]
    fn find_replace_conflict() {
        let mut reg = HookRegistry::new();
        reg.register(HookEntry::new("hook-a", "/a.dylib").with_symbol(
            "sqlite3_prepare_v2",
            "replace",
            "a",
        ));
        reg.register(HookEntry::new("hook-b", "/b.dylib").with_symbol("some_fn", "attach", "b"));

        assert_eq!(
            reg.find_replace_conflict("sqlite3_prepare_v2", "hook-c"),
            Some("hook-a")
        );
        assert!(reg.find_replace_conflict("some_fn", "hook-c").is_none());
        assert!(
            reg.find_replace_conflict("sqlite3_prepare_v2", "hook-a")
                .is_none()
        );
    }

    #[test]
    fn load_order_sorting() {
        let mut reg = HookRegistry::new();
        reg.register(HookEntry::new("b", "/b.dylib").with_load_order(2));
        reg.register(HookEntry::new("a", "/a.dylib").with_load_order(1));
        reg.register(HookEntry::new("c", "/c.dylib").with_load_order(3));
        let sorted = reg.hooks_by_load_order();
        assert_eq!(sorted[0].name, "a");
        assert_eq!(sorted[1].name, "b");
        assert_eq!(sorted[2].name, "c");
    }

    #[test]
    fn all_hooked_symbols() {
        let mut reg = HookRegistry::new();
        reg.register(
            HookEntry::new("a", "/a.dylib")
                .with_symbol("fn1", "replace", "")
                .with_symbol("fn2", "attach", ""),
        );
        reg.register(HookEntry::new("b", "/b.dylib").with_symbol("fn3", "replace", ""));
        let syms = reg.all_hooked_symbols();
        assert_eq!(syms.len(), 3);
    }

    #[test]
    fn roundtrip_json() {
        let mut reg = HookRegistry::new();
        reg.app_id = Some("test-app".into());
        reg.host_app = Some("/usr/bin/test".into());
        reg.register(
            HookEntry::new("hook-a", "/a.dylib")
                .with_version("1.0.0")
                .with_features(&["f1", "f2"])
                .with_symbol("fn1", "replace", "desc")
                .with_load_order(1)
                .with_log_path("/tmp/hook.log"),
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        reg.save_to(&path).unwrap();

        let loaded = HookRegistry::load_from(&path).unwrap();
        assert_eq!(loaded.schema_version, 1);
        assert_eq!(loaded.app_id.as_deref(), Some("test-app"));
        assert_eq!(loaded.hooks.len(), 1);
        assert_eq!(loaded.hooks[0].name, "hook-a");
        assert_eq!(loaded.hooks[0].hooked_symbols[0].method, "replace");
        assert_eq!(loaded.hooks[0].log_path.as_deref(), Some("/tmp/hook.log"));
    }

    #[test]
    fn load_nonexistent_returns_none() {
        assert!(HookRegistry::load_from(Path::new("/nonexistent/registry.json")).is_none());
    }
}
