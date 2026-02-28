# dylib-kit

Reusable Rust SDK for building, coordinating, and managing multiple dylib hooks injected into macOS applications.

## Related Repositories

- `zed-yolo-hook`: https://github.com/laris/zed-yolo-hook
- `zed-project-workspace`: https://github.com/laris/zed-project-workspace

Both hook repos use `dylib-kit` as their patch/injection runtime.

## Crates

| Crate | Description |
|-------|-------------|
| `dylib-hook-registry` | Multi-hook coordination registry: tracks injected hooks, conflicts, artifact hashes |
| `dylib-patcher` | Patch/restore/codesign/verify workflow: build, inject, sign, hash, health check |

## Quick Start

Add to your hook project's `xtask/Cargo.toml`:

```toml
[dependencies]
dylib-patcher = { path = "../dylib-kit/crates/dylib-patcher" }
dylib-hook-registry = { path = "../dylib-kit/crates/dylib-hook-registry" }
```

Write your `xtask` with `HookProject` + `HookEntry` metadata and delegate command handling:

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
    let patcher = Patcher::new(project, target, project_root());
    dylib_patcher::cli::run(patcher)
}
```

Then run:

```bash
cargo patch                # build + inject + sign
cargo patch --verify       # + launch + verify health markers
cargo patch status         # show registry + artifact hashes + stale check
cargo patch verify         # verify only (already patched)
cargo patch remove         # remove this hook, keep others
cargo patch restore        # restore original binary
```

## How Hook Repos Use This SDK

`zed-yolo-hook` and `zed-project-workspace` both follow this pattern:

1. Define hook metadata in `xtask` (`hook name`, `dylib`, `symbols`, `load_order`, `health checks`).
2. Use the same `cargo patch` UX provided by `dylib-patcher::cli`.
3. Keep hook-specific behavior in their hook crates, not in shell scripts.

This keeps hook repos small while `dylib-kit` owns the shared patch orchestration logic.

## Features

- Multi-hook coordination with per-app registry and load-order rules
- Artifact tracking (SHA-256 + git commit) with stale detection
- Log-marker health verification after app launch
- Deterministic reinjection from clean backup state
- CLI-first workflow through `cargo patch`
