//! CLI dispatcher for `cargo patch` subcommands.
//!
//! Hook projects call `dylib_patcher::cli::run(patcher)` from their xtask.

use crate::Patcher;
use anyhow::Result;

/// Run the CLI dispatcher. Parses args and dispatches to the appropriate patcher method.
pub fn run(patcher: Patcher) -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage(&patcher);
        return Ok(());
    }

    let subcommand = args.iter().skip(1).find(|a| !a.starts_with('-'));

    match subcommand.map(|s| s.as_str()) {
        // Internal: run by the detached process after app quit
        Some("__detached_patch") => {
            return patcher.run_detached_patch().map(|_| ());
        }
        Some("restore") => {
            patcher.quit_target_app()?;
            patcher.restore()?;
            patcher.launch_target_app()
        }
        Some("remove") => {
            patcher.quit_target_app()?;
            patcher.remove_self()?;
            patcher.launch_target_app()
        }
        Some("status") | Some("list") => patcher.status(),
        Some("verify") => {
            // If app not running, launch it first
            if !patcher.is_running_inside_target() {
                patcher.launch_target_app()?;
            }
            let result = patcher.launch_and_verify()?;
            if !result.all_passed {
                std::process::exit(1);
            }
            Ok(())
        }
        Some("patch") | None => {
            let no_build = args.iter().any(|a| a == "--no-build");
            let and_verify = args.iter().any(|a| a == "--verify");

            // Resolve dylib path: explicit --dylib > --no-build (use default) > build
            let dylib_path = if let Some(explicit) = crate::get_arg_value(&args, "--dylib") {
                let p = std::path::PathBuf::from(&explicit);
                if !p.exists() {
                    anyhow::bail!("specified dylib not found: {explicit}");
                }
                eprintln!("[dylib] Using explicit: {}", p.display());
                Some(p)
            } else if no_build {
                let default = patcher.default_dylib_path();
                if !default.exists() {
                    anyhow::bail!(
                        "--no-build specified but dylib not found at {}\nRun `cargo build --release` first.",
                        default.display()
                    );
                }
                eprintln!("[dylib] Using existing: {}", default.display());
                Some(default)
            } else {
                None // will trigger build inside patch_and_restart()
            };

            // patch_and_restart handles: quit app → patch → relaunch
            let result = patcher.patch_and_restart(dylib_path.as_deref())?;

            if and_verify {
                eprintln!();
                eprintln!("[verify] Waiting for hooks to initialize...");
                // App was just launched by patch_and_restart, wait for hooks
                let max_timeout = 15;
                std::thread::sleep(std::time::Duration::from_secs(max_timeout));
                let verify_result = patcher.launch_and_verify()?;
                if !verify_result.all_passed {
                    std::process::exit(1);
                }
            }

            let _ = result; // suppress unused warning
            Ok(())
        }
        Some("config") => {
            // Sub-subcommands: config [show|set|reset|path]
            let config_args: Vec<&str> = args
                .iter()
                .skip(1)
                .filter(|a| *a != "config" && !a.starts_with('-'))
                .map(|s| s.as_str())
                .collect();

            match config_args.first().copied() {
                None | Some("show") => patcher.config_show(),
                Some("set") => {
                    if config_args.len() < 3 {
                        // Show field list as usage help
                        if let Some(meta) = patcher.config_meta() {
                            let mut msg = "Usage: cargo patch config set <key> <value>\n\nKeys:\n"
                                .to_string();
                            for field in &meta.fields {
                                let vals: Vec<&str> =
                                    field.options.iter().map(|o| o.value.as_str()).collect();
                                msg.push_str(&format!("  {:<20}{}\n", field.key, vals.join(" | ")));
                            }
                            anyhow::bail!("{msg}");
                        } else {
                            anyhow::bail!("Usage: cargo patch config set <key> <value>");
                        }
                    }
                    patcher.config_set(config_args[1], config_args[2])
                }
                Some("reset") => patcher.config_reset(),
                Some("path") => patcher.config_path(),
                Some(other) => anyhow::bail!(
                    "Unknown config subcommand: {other}\n\nUsage: cargo patch config [show|set|reset|path]"
                ),
            }
        }
        Some(other) => {
            anyhow::bail!("Unknown command: {other}\nRun with --help for usage.");
        }
    }
}

fn print_usage(patcher: &Patcher) {
    eprintln!("dylib-patcher — {} hook management", patcher.project.name);
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  cargo patch                Quit app + build + inject + sign + relaunch");
    eprintln!("  cargo patch --verify       Quit + build + inject + sign + relaunch + verify");
    eprintln!("  cargo patch --no-build     Skip build, use existing dylib");
    eprintln!("  cargo patch --stable       Target Zed stable instead of Preview");
    eprintln!("  cargo patch --dylib PATH   Use specific pre-built dylib");
    eprintln!("  cargo patch verify         Check hook health from logs (app must be running)");
    eprintln!("  cargo patch status         Show injected hooks, registry, artifact hashes");
    eprintln!("  cargo patch list           Same as status");
    eprintln!("  cargo patch remove         Quit + remove this hook + relaunch (keeps others)");
    eprintln!("  cargo patch restore        Quit + restore original binary + relaunch");
    if patcher.config_meta().is_some() {
        eprintln!("  cargo patch config         Show config + available options");
        eprintln!("  cargo patch config set K V Set a config field");
        eprintln!("  cargo patch config reset   Reset config to defaults");
        eprintln!("  cargo patch config path    Print config file path");
    }
    eprintln!("  cargo patch --help         Show this help");
    eprintln!();
    eprintln!("Target: {}", patcher.target.app_path.display());
    eprintln!("Dylib:  {}", patcher.default_dylib_path().display());
    eprintln!();
    eprintln!("Note: When running inside the target app (e.g., from Zed's terminal),");
    eprintln!("      the app is automatically quit, patched, and relaunched.");
}
