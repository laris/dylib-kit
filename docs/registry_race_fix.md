# Registry Race Condition Fix: locked_register

> Date: 2026-04-12
> Commit: `11c764a`
> Affects: `dylib-hook-registry` crate

---

## 1. Problem

When multiple dylib hooks are injected into the same application (e.g., both
`zed-yolo-hook` and `zed-prj-workspace-hook` in Zed Preview), their `#[ctor]`
constructor functions run concurrently in multiple processes. Zed spawns 2-3
processes, each loading all injected dylibs.

Each hook's registration code was:

```rust
let mut registry = HookRegistry::load(app_id).unwrap_or_default();
registry.register(entry);  // add/update by name
registry.save(app_id)?;
```

**Race condition:** If process A reads the registry, then process B reads the
same file before A writes, both start with the same base state. Then:

1. A writes (base + hook A)
2. B writes (base + hook B) — **overwrites A's entry**

Result: the registry file ends up with only one hook registered, even though
both are injected in the binary.

### Observed symptoms

- `~/.config/dylib-hooks/zed-preview/registry.json` contained only one hook
  entry instead of two
- Running `cargo patch` from one hook project would restore the clean binary
  and only re-inject the hook it found in the registry, losing the other hook
- The stacking logic in `Patcher::patch()` worked correctly — the bug was
  that the registry was missing entries due to the race

---

## 2. Fix

Added `HookRegistry::locked_register()` method that atomically performs
load → register → save under a file lock:

```rust
pub fn locked_register(app_id: &str, entry: HookEntry) -> std::io::Result<()> {
    let path = registry_path(app_id)?;
    let lock_path = path.with_extension("lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;

    use fs2::FileExt;
    lock_file.lock_exclusive()?;

    let mut reg = Self::load_from(&path).unwrap_or_default();
    reg.app_id = Some(app_id.to_string());
    reg.register(entry);
    let result = reg.save_to(&path);

    let _ = lock_file.unlock();
    result
}
```

### Design decisions

- **File-based locking** via `fs2::FileExt::lock_exclusive()` — works across
  processes (which is the actual contention scenario)
- **Separate `.lock` file** rather than locking the registry JSON itself —
  avoids issues with `std::fs::write()` replacing the file
- **Blocking lock** — the lock is held for ~1ms (JSON read + modify + write),
  so blocking is fine. No timeout needed.
- **`fs2` crate** (v0.4) — minimal dependency, well-established, cross-platform

### Migration path

The old `load() + register() + save()` pattern still exists and works for
single-writer scenarios (e.g., the patcher CLI). The new `locked_register()`
should be used in any context where concurrent writers are possible (i.e.,
hook `#[ctor]` functions).

---

## 3. Affected consumers

| Repo | File | Change |
|------|------|--------|
| `zed-yolo-hook` | `src/lib.rs` | `register_in_registry()` uses `locked_register` |
| `zed-project-workspace` | `zed-prj-workspace-hook/src/lib.rs` | `register_in_registry()` uses `locked_register` |
| `dylib-patcher` | `src/lib.rs` `update_registry()` | Unchanged (single-writer, runs from CLI) |

---

## 4. Testing

Both hook projects verified via GitHub Actions CI after the fix:

| Repo | CI Run | Result |
|------|--------|--------|
| `zed-yolo-hook` | [24307265961](https://github.com/laris/zed-yolo-hook/actions/runs/24307265961) | PASS |
| `zed-project-workspace` | [24307266929](https://github.com/laris/zed-project-workspace/actions/runs/24307266929) | PASS |

Note: CI tests each hook in isolation (only one hook injected per run). The
race condition only manifests when both hooks are loaded simultaneously in the
same Zed instance. Full stacking verification requires local testing with
both hooks injected.
