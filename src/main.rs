mod code_fonts;
mod code_view;
mod icons;
mod manifest_links;
mod native_coverage;
mod navigation;
mod search;
mod search_benchmark;
mod search_index;
mod search_window;
mod settings;
mod ui;
mod usages;
mod usages_window;
mod word_occurrences;

use anyhow::Result;
use rdx::engine::{DecompilerEngine, NativeEngine};
use rdx::engines::ENGINES;
use std::path::Path;

fn main() -> Result<()> {
    let _search_cleanup = search_index::SessionCleanup::new();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let explicit_engine = args.first().is_some_and(|a| a == "--engine");
    if explicit_engine {
        anyhow::ensure!(
            args.len() >= 2,
            "Use --engine native before a headless command"
        );
        anyhow::ensure!(
            args[1] == "native",
            "Only the native Rust engine is available"
        );
        args.drain(..2);
        anyhow::ensure!(
            matches!(
                args.first().map(String::as_str),
                Some("--list" | "--decompile" | "--native-coverage" | "--native-cfg-audit")
            ),
            "Use --engine native with --list, --decompile, --native-coverage, or --native-cfg-audit; the GUI also uses native Rust"
        );
    }
    match args.first().map(String::as_str) {
        Some("mcp") if args.len() == 1 => rdx::mcp::run_stdio()?,
        Some("--engines") if args.len() == 1 => {
            for engine in ENGINES {
                println!(
                    "{}\t{} {}{}",
                    engine.id.key(),
                    engine.name,
                    engine.version,
                    if engine.alpha {
                        " (alpha; limited reconstruction)"
                    } else {
                        ""
                    }
                );
            }
        }
        Some("--list") if args.len() == 2 => {
            let mut engine = NativeEngine::start()?;
            for class in engine.open(Path::new(&args[1]))?.classes {
                println!("{class}");
            }
        }
        Some("--decompile") if args.len() == 3 => {
            let mut engine = NativeEngine::start()?;
            engine.open(Path::new(&args[1]))?;
            println!("{}", engine.decompile(&args[2])?);
        }
        Some("--native-coverage") if (2..=3).contains(&args.len()) => {
            native_coverage::execute(Path::new(&args[1]), args.get(2).map(String::as_str))?;
        }
        Some("--native-cfg-audit") if (2..=3).contains(&args.len()) => {
            native_coverage::execute_cfg(Path::new(&args[1]), args.get(2).map(String::as_str))?;
        }
        Some("--benchmark-search-current") if (3..=4).contains(&args.len()) => {
            let limit = args
                .get(3)
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(200_000);
            search_benchmark::execute_current(Path::new(&args[1]), &args[2], limit)?;
        }
        Some("--benchmark-search") if (3..=4).contains(&args.len()) => {
            let limit = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(128);
            search_benchmark::execute(Path::new(&args[1]), &args[2], limit)?;
        }
        Some("--help") => println!(
            "RDX\n  rdx mcp  (stdio MCP gateway)\n  rdx [FILE.apk|FILE.dex]\n  rdx --engines\n  rdx [--engine native] --list FILE\n  rdx [--engine native] --decompile FILE CLASS\n  rdx [--engine native] --native-coverage FILE [CLASS_PREFIX]\n  rdx [--engine native] --native-cfg-audit FILE [CLASS_PREFIX]\n  rdx --benchmark-search FILE QUERY [CLASS_LIMIT]\n  rdx --benchmark-search-current FILE QUERY [CLASS_LIMIT]\n\nGUI and CLI use the native Rust engine. Java reconstruction is alpha; unsupported methods are displayed as explicitly labeled DEX disassembly."
        ),
        _ if args.len() <= 1 && !args.first().is_some_and(|a| a.starts_with('-')) => {
            let initial = args.first().map(std::path::PathBuf::from);
            let app_icon =
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/rdx.png"))?;
            eframe::run_native(
                "RDX — Java & Android decompiler",
                eframe::NativeOptions {
                    viewport: eframe::egui::ViewportBuilder::default()
                        .with_icon(app_icon)
                        .with_inner_size([1280.0, 820.0])
                        .with_min_inner_size([800.0, 500.0]),
                    ..Default::default()
                },
                Box::new(move |cc| Ok(Box::new(ui::App::new(cc, initial)))),
            )
            .map_err(|e| anyhow::anyhow!("GUI initialization failed: {e}"))?;
        }
        _ => anyhow::bail!("Invalid arguments. Use rdx --help"),
    }
    Ok(())
}
