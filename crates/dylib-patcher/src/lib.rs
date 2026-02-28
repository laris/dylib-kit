//! `dylib-patcher` — reusable macOS dylib injection toolkit.
//!
//! Provides the complete patch/restore/codesign/verify workflow as a library,
//! so hook projects only need ~30 lines in their xtask instead of ~400.
//!
//! # Usage
//!
//! ```no_run
//! use dylib_patcher::{Patcher, HookProject, TargetApp};
//!
//! let project = HookProject::new("my-hook", "libmy_hook.dylib")
//!     .with_crate_name("my-hook-crate");
//! let target = TargetApp::zed_preview();
//! let patcher = Patcher::new(project, target, std::env::current_dir().unwrap());
//!
//! // Full patch workflow
//! patcher.patch(None).unwrap();
//! ```

pub mod cli;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Configuration for a hook project.
#[derive(Debug, Clone)]
pub struct HookProject {
    /// Human-readable hook name (e.g., "zed-yolo-hook")
    pub name: String,
    /// Dylib filename (e.g., "libzed_yolo_hook.dylib")
    pub dylib_filename: String,
    /// Cargo crate name to build (defaults to name)
    pub crate_name: Option<String>,
    /// Registry entry template for this hook
    pub registry_entry: Option<dylib_hook_registry::HookEntry>,
}

impl HookProject {
    pub fn new(name: &str, dylib_filename: &str) -> Self {
        Self {
            name: name.to_string(),
            dylib_filename: dylib_filename.to_string(),
            crate_name: None,
            registry_entry: None,
        }
    }

    pub fn with_crate_name(mut self, name: &str) -> Self {
        self.crate_name = Some(name.to_string());
        self
    }

    pub fn with_registry_entry(mut self, entry: dylib_hook_registry::HookEntry) -> Self {
        self.registry_entry = Some(entry);
        self
    }

    fn effective_crate_name(&self) -> &str {
        self.crate_name.as_deref().unwrap_or(&self.name)
    }
}

/// Configuration for the target application.
#[derive(Debug, Clone)]
pub struct TargetApp {
    /// Path to the .app bundle
    pub app_path: PathBuf,
    /// Relative path to binary within bundle
    pub binary_rel_path: String,
    /// App identifier for the hook registry
    pub app_id: String,
}

impl TargetApp {
    pub fn new(app_path: &str, binary_rel: &str, app_id: &str) -> Self {
        Self {
            app_path: PathBuf::from(app_path),
            binary_rel_path: binary_rel.to_string(),
            app_id: app_id.to_string(),
        }
    }

    pub fn zed_preview() -> Self {
        Self::new(
            "/Applications/Zed Preview.app",
            "Contents/MacOS/zed",
            "zed-preview",
        )
    }

    pub fn zed_stable() -> Self {
        Self::new(
            "/Applications/Zed.app",
            "Contents/MacOS/zed",
            "zed-stable",
        )
    }

    /// Resolve target from CLI args (--stable flag).
    pub fn from_args(args: &[String]) -> Self {
        if args.iter().any(|a| a == "--stable") {
            Self::zed_stable()
        } else if let Some(path) = get_arg_value(args, "--app") {
            Self::new(&path, "Contents/MacOS/zed", "custom")
        } else {
            Self::zed_preview()
        }
    }

    pub fn binary_path(&self) -> PathBuf {
        self.app_path.join(&self.binary_rel_path)
    }

    pub fn backup_path(&self) -> PathBuf {
        let bin = self.binary_path();
        PathBuf::from(format!("{}.original", bin.display()))
    }

    /// Directory for patch logs — same as the target binary directory.
    /// e.g., `/Applications/Zed Preview.app/Contents/MacOS/`
    pub fn logs_dir(&self) -> PathBuf {
        self.binary_path()
            .parent()
            .unwrap_or(Path::new("/tmp"))
            .to_path_buf()
    }
}

/// The patcher engine.
pub struct Patcher {
    pub project: HookProject,
    pub target: TargetApp,
    pub project_root: PathBuf,
}

impl Patcher {
    pub fn new(project: HookProject, target: TargetApp, project_root: PathBuf) -> Self {
        Self { project, target, project_root }
    }

    /// Default dylib path: `{project_root}/target/release/{dylib_filename}`
    pub fn default_dylib_path(&self) -> PathBuf {
        self.project_root
            .join("target/release")
            .join(&self.project.dylib_filename)
    }

    /// Build the hook dylib in release mode.
    pub fn build(&self) -> Result<PathBuf> {
        let crate_name = self.project.effective_crate_name();
        eprintln!("[build] Building {} (release)...", crate_name);

        let status = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .arg("-p")
            .arg(crate_name)
            .current_dir(&self.project_root)
            .status()
            .context("failed to run cargo build")?;

        if !status.success() {
            bail!("cargo build failed with {}", status);
        }

        let dylib = self.project_root
            .join("target/release")
            .join(&self.project.dylib_filename);

        if !dylib.exists() {
            bail!("build succeeded but dylib not found at {}", dylib.display());
        }

        eprintln!("[build] OK: {}", dylib.display());
        Ok(dylib)
    }

    /// Ensure insert-dylib tool is installed.
    pub fn ensure_insert_dylib(&self) -> Result<PathBuf> {
        let tool_path = self.project_root.join("target/tools/bin/insert-dylib");

        if tool_path.exists() {
            return Ok(tool_path);
        }

        eprintln!("[tools] Installing insert-dylib...");
        let tools_root = self.project_root.join("target/tools");

        let status = Command::new("cargo")
            .arg("install")
            .arg("insert-dylib")
            .arg("--root")
            .arg(&tools_root)
            .status()
            .context("failed to install insert-dylib")?;

        if !status.success() {
            bail!("cargo install insert-dylib failed");
        }

        if !tool_path.exists() {
            bail!("install succeeded but binary not found at {}", tool_path.display());
        }

        Ok(tool_path)
    }

    /// Check if our hook is already injected in the target binary.
    pub fn is_injected(&self) -> Result<bool> {
        let output = Command::new("otool")
            .arg("-L")
            .arg(self.target.binary_path())
            .output()
            .context("failed to run otool")?;

        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text.contains(&self.project.dylib_filename))
    }

    /// List all custom (non-system) dylibs injected into the target.
    pub fn list_injected(&self) -> Result<Vec<String>> {
        let output = Command::new("otool")
            .arg("-L")
            .arg(self.target.binary_path())
            .output()
            .context("failed to run otool")?;

        let text = String::from_utf8_lossy(&output.stdout);
        let custom: Vec<String> = text
            .lines()
            .filter(|l| l.contains("(weak)") || l.contains("LC_LOAD_WEAK_DYLIB"))
            .filter(|l| !l.contains("/System/") && !l.contains("/usr/"))
            .map(|l| l.trim().split(" (").next().unwrap_or(l.trim()).to_string())
            .collect();

        Ok(custom)
    }

    /// Inject our hook dylib (stacking alongside existing hooks).
    pub fn inject(&self, dylib_path: &Path) -> Result<()> {
        let insert_dylib = self.ensure_insert_dylib()?;
        let dylib_abs = std::fs::canonicalize(dylib_path)
            .context("failed to canonicalize dylib path")?;

        let bin = self.target.binary_path();

        eprintln!("[inject] {} → {}", self.project.dylib_filename, bin.display());

        let output = Command::new(&insert_dylib)
            .arg("--weak")
            .arg("--strip-codesig")
            .arg("--all-yes")
            .arg("--inplace")
            .arg(dylib_abs.to_string_lossy().as_ref())
            .arg(bin.to_string_lossy().as_ref())
            .output()
            .context("failed to run insert-dylib")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("insert-dylib failed: {stderr}");
        }

        eprintln!("[inject] OK");
        Ok(())
    }

    /// Re-sign the app bundle.
    pub fn codesign(&self) -> Result<()> {
        eprintln!("[sign] Signing {}...", self.target.app_path.display());

        let output = Command::new("codesign")
            .arg("-fs")
            .arg("-")
            .arg("--deep")
            .arg(&self.target.app_path)
            .output()
            .context("failed to run codesign")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("codesign failed: {stderr}");
        }

        eprintln!("[sign] OK");
        Ok(())
    }

    /// Verify our hook is present in the binary.
    pub fn verify(&self) -> Result<bool> {
        self.is_injected()
    }

    /// Ensure a clean binary (backup exists, restore if re-patching).
    fn ensure_clean_binary(&self) -> Result<()> {
        let bin = self.target.binary_path();
        let backup = self.target.backup_path();

        if !backup.exists() {
            eprintln!("[backup] Creating backup...");
            std::fs::copy(&bin, &backup).context("failed to create backup")?;
        } else {
            eprintln!("[backup] Restoring clean binary before patching...");
            std::fs::copy(&backup, &bin).context("failed to restore from backup")?;
        }
        Ok(())
    }

    /// Full patch workflow: build → backup/restore → inject all registry hooks → sign → verify.
    ///
    /// If `dylib_path` is None, builds the dylib first.
    /// Restores from backup and re-injects ALL registered hooks (including ours) to ensure
    /// a clean, deterministic binary.
    pub fn patch(&self, dylib_path: Option<&Path>) -> Result<PatchResult> {
        let dylib = match dylib_path {
            Some(p) => p.to_path_buf(),
            None => self.build()?,
        };

        if !self.target.app_path.exists() {
            bail!("target app not found: {}", self.target.app_path.display());
        }

        // Step 1: Restore clean binary
        self.ensure_clean_binary()?;

        // Step 2: Read registry to find ALL hooks to inject
        let registry = dylib_hook_registry::HookRegistry::load(&self.target.app_id);
        let mut hooks_to_inject: Vec<(String, PathBuf)> = Vec::new();

        if let Some(reg) = &registry {
            for hook in reg.hooks_by_load_order() {
                if hook.name == self.project.name {
                    continue; // We'll add ourselves
                }
                let path = PathBuf::from(&hook.dylib_path);
                if path.exists() {
                    hooks_to_inject.push((hook.name.clone(), path));
                } else {
                    eprintln!("[warn] Registered hook '{}' dylib not found: {}", hook.name, hook.dylib_path);
                }
            }
        }

        // Add ourselves
        hooks_to_inject.push((self.project.name.clone(), dylib.clone()));

        // Sort by load order (registry order is already sorted)
        // Inject in order
        for (name, path) in &hooks_to_inject {
            eprintln!("[inject] {} ({})", name, path.display());
            self.inject(path)?;
        }

        // Step 3: Sign
        self.codesign()?;

        // Step 4: Verify
        let verified = self.verify()?;
        if !verified {
            bail!("verification failed: {} not found in binary after injection", self.project.dylib_filename);
        }

        // Step 5: Update registry
        self.update_registry(&dylib)?;

        eprintln!();
        eprintln!("Patched successfully!");
        eprintln!("  App:    {}", self.target.app_path.display());
        eprintln!("  Hooks:  {}", hooks_to_inject.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "));

        Ok(PatchResult {
            dylib_path: dylib,
            hooks_injected: hooks_to_inject.into_iter().map(|(n, _)| n).collect(),
            codesigned: true,
            verified,
        })
    }

    /// Check if we're running inside the target app's process tree.
    ///
    /// Walks up the parent PID chain and checks if any ancestor is the target
    /// app binary. This correctly distinguishes "Zed's terminal" from "external
    /// terminal while Zed is open".
    ///
    /// Example chain when running inside Zed:
    /// ```text
    /// Zed Preview (pid 17191)   ← target found here
    ///   → fish (terminal)
    ///     → node (agent)
    ///       → claude
    ///         → zsh (bash tool)
    ///           → cargo → xtask  ← we are here
    /// ```
    pub fn is_running_inside_target(&self) -> bool {
        let target_path = self.target.binary_path();
        let target_str = target_path.to_string_lossy();

        let mut pid = std::process::id().to_string();
        for _ in 0..20 {
            let output = match Command::new("ps")
                .args(["-o", "ppid=,comm=", "-p", &pid])
                .output()
            {
                Ok(o) if o.status.success() => o,
                _ => break,
            };

            let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if line.is_empty() {
                break;
            }

            // Parse: "  17191 /Applications/Zed Preview.app/Contents/MacOS/zed"
            let parts: Vec<&str> = line.splitn(2, |c: char| c.is_whitespace()).collect();
            if parts.len() < 2 {
                break;
            }
            let ppid = parts[0].trim();
            let comm = parts[1].trim();

            if comm.contains(&*target_str) || target_str.contains(comm) {
                return true;
            }

            if ppid == "1" || ppid == "0" || ppid == pid {
                break;
            }
            pid = ppid.to_string();
        }
        false
    }

    /// Check if the target app is running at all (regardless of ancestry).
    pub fn is_target_running(&self) -> bool {
        let target_bin = self.target.binary_path();
        Command::new("pgrep")
            .arg("-f")
            .arg(target_bin.to_string_lossy().as_ref())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Quit the target app. If we're running inside it, spawns a detached
    /// background process that waits for exit, then runs the callback script.
    /// Returns true if the app was quit, false if it wasn't running.
    pub fn quit_target_app(&self) -> Result<bool> {
        if !self.is_running_inside_target() {
            return Ok(false);
        }

        eprintln!("[quit] Quitting {}...", self.target.app_path.display());

        // Try graceful quit via AppleScript
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "tell application \"{}\" to quit",
                self.target.app_path.file_stem().unwrap_or_default().to_string_lossy()
            ))
            .output();

        // Wait up to 5 seconds
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if !self.is_running_inside_target() {
                eprintln!("[quit] App stopped.");
                return Ok(true);
            }
        }

        // Force kill
        eprintln!("[quit] Force killing...");
        let _ = Command::new("pkill")
            .arg("-f")
            .arg(self.target.binary_path().to_string_lossy().as_ref())
            .output();

        std::thread::sleep(std::time::Duration::from_secs(2));
        Ok(true)
    }

    /// Kill orphaned child processes left behind after the target app exits.
    ///
    /// Zed spawns many child processes (MCP servers, language servers, etc.) that
    /// may not be cleaned up when the main process dies. This finds them by looking
    /// for processes whose command line references the app's support directory.
    fn kill_orphaned_children(&self) {
        let app_name = self.target.app_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();

        // Find processes referencing the app's support/extension directories
        // e.g. processes with "Zed/extensions" or "Zed/languages" in their cmdline
        let patterns = [
            format!("{}/extensions/", app_name),
            format!("{}/languages/", app_name),
        ];

        for pattern in &patterns {
            let output = Command::new("pgrep")
                .arg("-f")
                .arg(pattern)
                .output();

            if let Ok(output) = output {
                let pids = String::from_utf8_lossy(&output.stdout);
                let count = pids.lines().filter(|l| !l.trim().is_empty()).count();
                if count > 0 {
                    eprintln!("[detached] Killing {} orphaned child processes (pattern: {})", count, pattern);
                    let _ = Command::new("pkill")
                        .arg("-f")
                        .arg(pattern)
                        .output();
                }
            }
        }

        // Also kill the crash handler if still running
        let binary_name = self.target.binary_path()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let crash_pattern = format!("--crash-handler.*{}", binary_name);
        let _ = Command::new("pkill").arg("-f").arg(&crash_pattern).output();
    }

    /// Launch the target app.
    pub fn launch_target_app(&self) -> Result<()> {
        eprintln!("[launch] Starting {}...", self.target.app_path.display());
        Command::new("open")
            .arg(&self.target.app_path)
            .status()
            .context("failed to launch app")?;
        Ok(())
    }

    /// Full patch + restart cycle. Handles the case where we're running inside the target app.
    ///
    /// When running inside the target app (e.g., Zed's terminal), quitting the app would
    /// kill this process too. To handle this, we:
    /// 1. Build the dylib FIRST (before quitting)
    /// 2. Spawn a detached process in a new session (`setsid`) that survives the app quit
    /// 3. The detached process waits for the app to die, patches, and relaunches
    ///
    /// When running outside the target app, does everything inline.
    pub fn patch_and_restart(&self, dylib_path: Option<&Path>) -> Result<PatchResult> {
        let dylib = match dylib_path {
            Some(p) => p.to_path_buf(),
            None => self.build()?,
        };

        if !self.is_running_inside_target() {
            // Simple case: app not running, just patch
            let result = self.patch(Some(&dylib))?;
            return Ok(result);
        }

        // Complex case: we're inside the target app.
        // Spawn a detached process in a NEW SESSION (setsid) so it survives when
        // the app quits and sends SIGHUP to its session.
        let xtask_bin = std::env::current_exe()
            .context("cannot determine current executable")?;

        let app_pid = Command::new("pgrep")
            .arg("-x")
            .arg(self.target.binary_path().file_name().unwrap_or_default().to_string_lossy().as_ref())
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().lines().next().map(|s| s.to_string()))
            .unwrap_or_default();

        // Open log file for the detached process (stderr will be invalid after app quits).
        // Logs are co-located with the target binary for discoverability.
        let log_dir = self.target.logs_dir();
        std::fs::create_dir_all(&log_dir).ok();
        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let log_path = log_dir.join(format!("dylib-patch-{}.log", timestamp));
        let log_file = std::fs::File::create(&log_path)
            .context("failed to create detached patcher log file")?;

        eprintln!("[patch] Running inside target app (pid={}). Spawning detached patcher...", app_pid);
        eprintln!("[patch] Detached log: {}", log_path.display());

        let mut cmd = Command::new(&xtask_bin);
        cmd.arg("__detached_patch");
        for arg in std::env::args().skip(1) {
            if arg != "__detached_patch" {
                cmd.arg(&arg);
            }
        }
        cmd.env("__DYLIB_PATCHER_APP_PID", &app_pid);
        cmd.env("__DYLIB_PATCHER_DYLIB_PATH", dylib.to_string_lossy().as_ref());

        // Redirect stdio away from the terminal (it dies when the app quits)
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(log_file));

        // Create a new session so the child survives when the app's session terminates.
        // SAFETY: setsid() is async-signal-safe per POSIX.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = cmd.spawn().context("failed to spawn detached patcher")?;
        eprintln!("[detached] Patcher spawned (pid={}). Quitting app...", child.id());

        // Now quit the app — this will kill us too (if we're a child process)
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "tell application \"{}\" to quit",
                self.target.app_path.file_stem().unwrap_or_default().to_string_lossy()
            ))
            .output();

        // If we're still alive (running from external terminal), wait briefly
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Return a placeholder result — the real work happens in the detached process
        Ok(PatchResult {
            dylib_path: dylib,
            hooks_injected: vec![self.project.name.clone()],
            codesigned: false, // will be done by detached process
            verified: false,
        })
    }

    /// Internal: run by the detached process after the app has quit.
    /// Called when argv[1] == "__detached_patch".
    pub fn run_detached_patch(&self) -> Result<PatchResult> {
        // Ignore SIGHUP as belt-and-suspenders (setsid should already protect us)
        #[cfg(unix)]
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN); }

        let app_pid = std::env::var("__DYLIB_PATCHER_APP_PID").unwrap_or_default();
        let dylib_path = std::env::var("__DYLIB_PATCHER_DYLIB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.default_dylib_path());

        eprintln!("[detached] Waiting for app to exit (pid={})...", app_pid);

        // Wait for the app process to die
        if !app_pid.is_empty() {
            for _ in 0..30 { // max 30 seconds
                let still_running = Command::new("kill")
                    .arg("-0")
                    .arg(&app_pid)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !still_running {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }

        // Grace period for children to notice parent died and exit on their own
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Kill orphaned child processes (MCP servers, language servers, etc.)
        self.kill_orphaned_children();

        eprintln!("[detached] App exited. Patching...");
        let result = self.patch(Some(&dylib_path))?;

        eprintln!("[detached] Relaunching app...");
        self.launch_target_app()?;

        eprintln!("[detached] Done.");
        Ok(result)
    }

    /// Restore the original unpatched binary from backup.
    pub fn restore(&self) -> Result<()> {
        let bin = self.target.binary_path();
        let backup = self.target.backup_path();

        if !backup.exists() {
            bail!("no backup found at {}", backup.display());
        }

        eprintln!("[restore] Restoring original binary...");
        std::fs::copy(&backup, &bin).context("failed to restore")?;
        self.codesign()?;

        // Clear registry
        if let Some(mut reg) = dylib_hook_registry::HookRegistry::load(&self.target.app_id) {
            reg.hooks.clear();
            let _ = reg.save(&self.target.app_id);
        }

        eprintln!("[restore] Done. All hooks removed.");
        Ok(())
    }

    /// Remove only this hook (restore + re-inject others).
    pub fn remove_self(&self) -> Result<()> {
        self.ensure_clean_binary()?;

        // Re-inject all hooks EXCEPT ours
        if let Some(reg) = dylib_hook_registry::HookRegistry::load(&self.target.app_id) {
            for hook in reg.hooks_by_load_order() {
                if hook.name == self.project.name {
                    continue;
                }
                let path = PathBuf::from(&hook.dylib_path);
                if path.exists() {
                    self.inject(&path)?;
                }
            }
        }

        self.codesign()?;

        // Remove from registry
        if let Some(mut reg) = dylib_hook_registry::HookRegistry::load(&self.target.app_id) {
            reg.remove(&self.project.name);
            let _ = reg.save(&self.target.app_id);
        }

        eprintln!("[remove] Removed {}. Other hooks preserved.", self.project.name);
        Ok(())
    }

    /// Show status.
    pub fn status(&self) -> Result<()> {
        let bin = self.target.binary_path();
        eprintln!("App:     {}", self.target.app_path.display());
        eprintln!("Binary:  {}", bin.display());
        eprintln!("Backup:  {} ({})", self.target.backup_path().display(),
            if self.target.backup_path().exists() { "exists" } else { "missing" });

        eprintln!("\nInjected hooks (otool):");
        for h in self.list_injected()? {
            eprintln!("  {}", h);
        }

        eprintln!("\nRegistry ({}):", self.target.app_id);
        match dylib_hook_registry::HookRegistry::load(&self.target.app_id) {
            Some(reg) => {
                if let Some(ts) = &reg.last_patched {
                    eprintln!("  Last patched: {}", ts);
                }
                for hook in reg.hooks_by_load_order() {
                    eprintln!("\n  {} v{} (order={})",
                        hook.name,
                        hook.version.as_deref().unwrap_or("?"),
                        hook.load_order.unwrap_or(0));
                    eprintln!("    features: {:?}", hook.features);
                    eprintln!("    dylib: {}", hook.dylib_path);
                    for sym in &hook.hooked_symbols {
                        eprintln!("    hook: {} [{}] {}", sym.symbol, sym.method,
                            sym.description.as_deref().unwrap_or(""));
                    }
                    // Artifact info
                    if let Some(art) = &hook.artifact {
                        eprintln!("    artifact: sha256={:.16}... size={} patched={}",
                            art.sha256, art.size, art.patched_at);
                        if let Some(commit) = &art.git_commit {
                            eprintln!("    git: {}", commit);
                        }
                        // Stale check
                        match check_artifact_stale(hook) {
                            Some(true) => eprintln!("    WARNING: dylib on disk has CHANGED since patching — re-patch needed"),
                            Some(false) => eprintln!("    artifact: up to date"),
                            None => eprintln!("    artifact: cannot verify (dylib missing?)"),
                        }
                    }
                    // Health check info
                    if let Some(hc) = &hook.health_check {
                        eprintln!("    health: log={}, markers={}, timeout={}s",
                            hc.log_glob, hc.success_markers.len(), hc.timeout_secs);
                    }
                }
            }
            None => eprintln!("  (no registry file)"),
        }

        Ok(())
    }

    /// Launch the host app and verify all hooks loaded via their health checks.
    ///
    /// 1. Launches the app
    /// 2. Waits for each hook's success_markers in its log file
    /// 3. Checks no failure_markers appear
    /// 4. Reports pass/fail per hook
    pub fn launch_and_verify(&self) -> Result<VerifyResult> {
        let registry = dylib_hook_registry::HookRegistry::load(&self.target.app_id);
        let hooks: Vec<&dylib_hook_registry::HookEntry> = match &registry {
            Some(reg) => reg.hooks_by_load_order(),
            None => {
                bail!("no registry found for '{}' — run `cargo patch` first", self.target.app_id);
            }
        };

        if hooks.is_empty() {
            bail!("registry is empty — no hooks to verify");
        }

        // Record log file sizes BEFORE launching (so we only check new content)
        let log_baselines: Vec<(&str, Option<u64>)> = hooks
            .iter()
            .map(|h| {
                let size = h.health_check.as_ref().and_then(|hc| {
                    resolve_glob_latest(&hc.log_glob).and_then(|p| std::fs::metadata(&p).ok().map(|m| m.len()))
                });
                (h.name.as_str(), size)
            })
            .collect();

        // Launch the app
        eprintln!("[verify] Launching {}...", self.target.app_path.display());
        Command::new("open")
            .arg(&self.target.app_path)
            .status()
            .context("failed to launch app")?;

        // Verify each hook
        let mut results = Vec::new();
        let max_timeout = hooks
            .iter()
            .filter_map(|h| h.health_check.as_ref().map(|hc| hc.timeout_secs))
            .max()
            .unwrap_or(10);

        eprintln!("[verify] Waiting up to {}s for hooks to initialize...", max_timeout);
        std::thread::sleep(std::time::Duration::from_secs(max_timeout as u64));

        for (i, hook) in hooks.iter().enumerate() {
            let (_, baseline_size) = log_baselines[i];
            let check_result = verify_hook(hook, baseline_size);
            let status = match &check_result {
                Ok(true) => "PASS",
                Ok(false) => "FAIL",
                Err(_) => "ERROR",
            };
            eprintln!("[verify] {} {} — {}", status, hook.name,
                match &check_result {
                    Ok(true) => "all markers found".to_string(),
                    Ok(false) => "markers not found in log".to_string(),
                    Err(e) => format!("{}", e),
                }
            );
            let (passed, error) = match check_result {
                Ok(b) => (b, None),
                Err(e) => (false, Some(e.to_string())),
            };
            results.push(HookVerifyResult {
                name: hook.name.clone(),
                passed,
                error,
            });
        }

        let all_passed = results.iter().all(|r| r.passed);
        eprintln!("\n[verify] {}", if all_passed { "ALL HOOKS VERIFIED" } else { "SOME HOOKS FAILED" });

        Ok(VerifyResult { hooks: results, all_passed })
    }

    /// Update the hook registry after patching, including artifact hash.
    fn update_registry(&self, dylib_path: &Path) -> Result<()> {
        let mut reg = dylib_hook_registry::HookRegistry::load(&self.target.app_id)
            .unwrap_or_default();

        reg.app_id = Some(self.target.app_id.clone());
        reg.host_app = Some(self.target.app_path.to_string_lossy().to_string());
        reg.last_patched = Some(chrono::Utc::now().to_rfc3339());

        let mut entry = self.project.registry_entry.clone().unwrap_or_else(||
            dylib_hook_registry::HookEntry::new(&self.project.name, "")
        );
        entry.dylib_path = dylib_path.to_string_lossy().to_string();
        entry.installed_at = Some(chrono::Utc::now().to_rfc3339());
        entry.artifact = Some(compute_artifact_info(dylib_path, &self.project_root)?);

        reg.register(entry);
        reg.save(&self.target.app_id)
            .context("failed to save registry")?;

        Ok(())
    }
}

pub struct PatchResult {
    pub dylib_path: PathBuf,
    pub hooks_injected: Vec<String>,
    pub codesigned: bool,
    pub verified: bool,
}

pub struct VerifyResult {
    pub hooks: Vec<HookVerifyResult>,
    pub all_passed: bool,
}

pub struct HookVerifyResult {
    pub name: String,
    pub passed: bool,
    pub error: Option<String>,
}

/// Compute artifact identity (hash + size + git commit) for a dylib file.
fn compute_artifact_info(
    dylib_path: &Path,
    project_root: &Path,
) -> Result<dylib_hook_registry::ArtifactInfo> {
    let data = std::fs::read(dylib_path)
        .with_context(|| format!("failed to read {}", dylib_path.display()))?;

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hash = format!("{:x}", hasher.finalize());

    let git_commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    Ok(dylib_hook_registry::ArtifactInfo {
        sha256: hash,
        size: data.len() as u64,
        patched_at: chrono::Utc::now().to_rfc3339(),
        git_commit,
    })
}

/// Check if a dylib on disk matches its registered artifact hash.
/// Returns `Some(true)` if matches, `Some(false)` if stale, `None` if can't check.
pub fn check_artifact_stale(entry: &dylib_hook_registry::HookEntry) -> Option<bool> {
    let artifact = entry.artifact.as_ref()?;
    let path = Path::new(&entry.dylib_path);
    let data = std::fs::read(path).ok()?;

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let current_hash = format!("{:x}", hasher.finalize());

    Some(current_hash != artifact.sha256)
}

fn get_arg_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

/// Verify a single hook by checking its log file for health check markers.
fn verify_hook(
    hook: &dylib_hook_registry::HookEntry,
    baseline_size: Option<u64>,
) -> Result<bool> {
    let hc = match &hook.health_check {
        Some(hc) => hc,
        None => {
            // No health check defined — can't verify, assume pass
            eprintln!("  {} — no health check defined, skipping", hook.name);
            return Ok(true);
        }
    };

    let log_path = resolve_glob_latest(&hc.log_glob)
        .ok_or_else(|| anyhow::anyhow!("log file not found matching: {}", hc.log_glob))?;

    // Read only NEW log content (after baseline)
    let content = std::fs::read_to_string(&log_path)
        .context("failed to read log file")?;
    let new_content = match baseline_size {
        Some(offset) if (offset as usize) < content.len() => &content[offset as usize..],
        _ => &content,
    };

    // Check for failure markers first
    for marker in &hc.failure_markers {
        if new_content.contains(marker) {
            eprintln!("  {} — FAILURE marker found: \"{}\"", hook.name, marker);
            return Ok(false);
        }
    }

    // Check all success markers present
    for marker in &hc.success_markers {
        if !new_content.contains(marker) {
            eprintln!("  {} — missing marker: \"{}\"", hook.name, marker);
            return Ok(false);
        }
    }

    Ok(true)
}

/// Resolve a glob pattern to the most recent matching file.
/// Supports `~` expansion and `*` wildcards.
fn resolve_glob_latest(pattern: &str) -> Option<PathBuf> {
    let expanded = if pattern.starts_with("~/") {
        dirs::home_dir()?.join(&pattern[2..]).to_string_lossy().to_string()
    } else {
        pattern.to_string()
    };

    // Simple glob: split at the last `/`, glob the filename part
    let path = Path::new(&expanded);
    let parent = path.parent()?;
    let file_pattern = path.file_name()?.to_string_lossy();

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if matches_simple_glob(&name_str, &file_pattern) {
                candidates.push(entry.path());
            }
        }
    }

    // Return the most recently modified file
    candidates.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    candidates.pop()
}

/// Simple glob matching: only supports `*` as wildcard.
fn matches_simple_glob(text: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return text == pattern;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.is_empty() {
        return true;
    }
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match text[pos..].find(part) {
            Some(idx) => {
                if i == 0 && idx != 0 {
                    return false; // Must match at start if pattern doesn't start with *
                }
                pos += idx + part.len();
            }
            None => return false,
        }
    }
    // If pattern doesn't end with *, text must end here
    if !pattern.ends_with('*') {
        return pos == text.len();
    }
    true
}
