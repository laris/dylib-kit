# dylib-kit SDK — Reusable macOS Dylib Hook Toolkit

Updated: 2026-02-28

## Overview

`dylib-kit` is a Rust SDK for building, coordinating, and managing multiple dylib hooks injected into macOS applications. It provides:

| Crate | Purpose |
|-------|---------|
| `dylib-hook-registry` | Multi-hook coordination registry (`~/.config/dylib-hooks/{app_id}/registry.json`) |
| `dylib-patcher` | Build, inject, codesign, verify, restore workflow as a library + CLI |

Hook developers write ~50 lines of xtask config. The SDK handles everything else.

## Research Finding

Surveyed 20+ projects across Rust, C/C++, ObjC/Swift ecosystems. **No existing Rust SDK provides this combination.** Closest comparables:
- `dylib_dobby_hook` (C/ObjC) — adapter pattern + auto_hack.sh
- `macSubstrate` (ObjC) — plugin manager for macOS
- `Cydia Substrate` (C++) — MobileLoader pattern (design reference)

## Crate Layout

```
~/codes/dylib-kit/
├── Cargo.toml                        # workspace root
├── crates/
│   ├── dylib-hook-registry/          # coordination registry
│   │   └── src/lib.rs                # HookRegistry, HookEntry, HealthCheck, ArtifactInfo
│   └── dylib-patcher/                # injection toolkit
│       └── src/
│           ├── lib.rs                # Patcher, build/inject/sign/verify/restore
│           └── cli.rs                # CLI: cargo patch / status / verify / remove / restore
└── docs/
    └── sdk_design.md                 # this file
```

## Registry Schema (`registry.json`)

```json
{
  "schema_version": 1,
  "app_id": "zed-preview",
  "host_app": "/Applications/Zed Preview.app",
  "last_patched": "2026-02-28T06:00:00Z",
  "hooks": [
    {
      "name": "zed-yolo-hook",
      "dylib_path": "{project_root}/target/release/libzed_yolo_hook.dylib",
      "version": "0.1.0",
      "features": ["yolo-mode", "auto-approve-tools"],
      "hooked_symbols": [
        { "symbol": "ToolPermissionDecision::from_input", "method": "attach", "description": "Auto-approve built-in tool calls" },
        { "symbol": "AcpThread::request_tool_call_authorization", "method": "attach", "description": "Auto-approve ACP agent tool calls" }
      ],
      "load_order": 1,
      "artifact": {
        "sha256": "a1b2c3d4e5f6...",
        "size": 2949120,
        "patched_at": "2026-02-28T06:00:00Z",
        "git_commit": "34400cc"
      },
      "health_check": {
        "log_glob": "~/Library/Logs/Zed/zed-yolo-hook.*.log",
        "success_markers": ["=== zed-yolo-hook v", "YOLO mode ACTIVE"],
        "failure_markers": ["Cannot find", "attach failed"],
        "timeout_secs": 15
      }
    }
  ]
}
```

## Key Schema Types

| Type | Purpose |
|------|---------|
| `HookEntry` | One hook: name, dylib path, version, symbols, load order |
| `HookedSymbol` | Symbol name + method (`attach` or `replace`) + description |
| `ArtifactInfo` | SHA-256 hash, file size, patch timestamp, git commit |
| `HealthCheck` | Log glob, success/failure markers, timeout for verification |

## Conflict Detection

- `attach()` hooks (listeners) never conflict — multiple OK
- `replace()` hooks on the same symbol conflict — must chain
- `find_replace_conflict(symbol, my_name)` checks before install
- Frida-Gum naturally chains if load order is deterministic

## cargo patch Commands

| Command | What It Does |
|---------|-------------|
| `cargo patch` | Build + quit app + patch + relaunch |
| `cargo patch --no-build` | Skip build, use existing dylib |
| `cargo patch --verify` | Build + quit + patch + relaunch + wait + verify health |
| `cargo patch verify` | Check hook health from logs (app must be running) |
| `cargo patch status` | Show registry, artifact hashes, stale detection |
| `cargo patch remove` | Remove THIS hook only (restore + re-inject others + relaunch) |
| `cargo patch restore` | Restore original binary (remove ALL hooks + relaunch) |

All commands that modify the binary automatically quit and relaunch the app.

## Smart Process Detection

`cargo patch` detects whether it's running inside the target app (e.g., from Zed's terminal) by walking the PID ancestry chain:

```
ps -o ppid=,comm= -p {pid}  →  repeat up to pid 1
```

| Running from | Detected | Behavior |
|---|---|---|
| **Zed's terminal** | `is_running_inside_target() = true` | Spawn **detached** child → quit Zed → child waits for exit → patches → relaunches |
| **External terminal** | `is_running_inside_target() = false` | Inline: quit Zed → patch → relaunch (synchronous) |
| **Zed not running** | `is_target_running() = false` | Just patch, no quit/relaunch |

The detached process pattern solves the problem where quitting the app kills the patcher process too.

Two separate checks:
- `is_running_inside_target()` — ancestry walk (am I a child of the app?)
- `is_target_running()` — pgrep (is the app open at all?)

## Verification Methodology

After patching, `cargo patch verify` or `--verify`:

1. Records log file sizes BEFORE launching (baseline)
2. Launches the app via `open`
3. Waits `timeout_secs` (default 15s)
4. Reads ONLY new log content (after baseline)
5. Checks ALL `success_markers` present
6. Checks NO `failure_markers` present
7. Reports PASS/FAIL per hook

## Stale Detection

`cargo patch status` computes current SHA-256 of each dylib on disk and compares against the hash stored at patch time. If different:

```
WARNING: dylib on disk has CHANGED since patching — re-patch needed
```

## xtask Template (~50 lines)

```rust
use dylib_hook_registry::{HealthCheck, HookEntry};
use dylib_patcher::{HookProject, Patcher, TargetApp};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let project = HookProject::new("my-hook", "libmy_hook.dylib")
        .with_crate_name("my-hook")
        .with_registry_entry(
            HookEntry::new("my-hook", "")
                .with_version(env!("CARGO_PKG_VERSION"))
                .with_features(&["my-feature"])
                .with_symbol("target_function", "replace", "what it does")
                .with_load_order(1)
                .with_health_check(
                    HealthCheck::new("~/Library/Logs/App/my-hook.*.log")
                        .with_success("hook installed")
                        .with_failure("symbol not found")
                        .with_timeout(10),
                ),
        );

    let target = TargetApp::from_args(&args);
    let patcher = Patcher::new(project, target, project_root());
    dylib_patcher::cli::run(patcher)
}
```

## Consumer Projects

| Project | Hook Type | Symbols | Load Order |
|---------|-----------|---------|------------|
| `zed-yolo-hook` | `attach` (listeners) | Permission functions | 1 |
| `zed-prj-workspace-hook` | `replace` (detour) | `sqlite3_prepare_v2` | 2 |
