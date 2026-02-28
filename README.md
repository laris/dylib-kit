# dylib-kit

Reusable Rust SDK for building, coordinating, and managing multiple dylib hooks injected into macOS applications.

## Crates

| Crate | Description |
|-------|-------------|
| `dylib-hook-registry` | Multi-hook coordination registry — tracks injected hooks, detects conflicts, stores artifact hashes |
| `dylib-patcher` | Patch/restore/codesign/verify workflow — build, inject, sign, hash, health check |

## Quick Start

Add to your hook project's `xtask/Cargo.toml`:

```toml
[dependencies]
dylib-patcher = { path = "/path/to/dylib-kit/crates/dylib-patcher" }
dylib-hook-registry = { path = "/path/to/dylib-kit/crates/dylib-hook-registry" }
```

Write ~50 lines of xtask:

```rust
use dylib_hook_registry::{HealthCheck, HookEntry};
use dylib_patcher::{HookProject, Patcher, TargetApp};

fn main() -> anyhow::Result<()> {
    let project = HookProject::new("my-hook", "libmy_hook.dylib")
        .with_registry_entry(
            HookEntry::new("my-hook", "")
                .with_symbol("target_fn", "replace", "what it does")
                .with_load_order(1)
                .with_health_check(
                    HealthCheck::new("~/Library/Logs/App/my-hook.*.log")
                        .with_success("hook installed")
                        .with_timeout(10),
                ),
        );
    let target = TargetApp::from_args(&std::env::args().collect::<Vec<_>>());
    dylib_patcher::cli::run(Patcher::new(project, target, project_root()))
}
```

Then:

```bash
cargo patch                # build + inject + sign
cargo patch --verify       # + launch + verify health markers
cargo patch status         # show registry + artifact hashes + stale check
cargo patch verify         # verify only (already patched)
cargo patch remove         # remove this hook, keep others
cargo patch restore        # restore original binary
```

## Features

- **Multi-hook coordination**: Registry tracks all injected hooks per app, detects `replace()` conflicts
- **Artifact tracking**: SHA-256 hash + git commit stored at patch time, stale detection
- **Health check verification**: Log-based — scan for success/failure markers after app launch
- **Deterministic injection**: Restore from backup, re-inject ALL hooks in load order
- **No shell scripts**: Everything through `cargo patch` xtask
