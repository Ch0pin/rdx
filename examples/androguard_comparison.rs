//! Bounded real-backend evaluation; no GUI/project state is modified.
use anyhow::{Context, Result, ensure};
use rdx::{engine::NativeEngine, load_limits::LoadLimits};
use serde_json::json;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() >= 3, "APK OUTPUT [PREFIX] [MAX_CLASSES]");
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&output)?;
    let prefix = args.get(3).map_or("com.okx", String::as_str);
    let maximum = args.get(4).map_or(Ok(200usize), |s| s.parse())?.min(300);
    let exclude_reason = args
        .get(5)
        .map(|pattern| regex::Regex::new(pattern))
        .transpose()?;
    let mut engine = NativeEngine::start()?;
    let loaded = Instant::now();
    let project = engine.open_with_limits(&PathBuf::from(&args[1]), LoadLimits::Large)?;
    eprintln!(
        "loaded {} classes in {:?}",
        project.classes.len(),
        loaded.elapsed()
    );
    let candidates: Vec<_> = project
        .classes
        .iter()
        .filter(|class| {
            prefix.split(',').any(|prefix| class.starts_with(prefix)) && !class.contains('$')
        })
        .collect();
    ensure!(!candidates.is_empty(), "no classes for prefix {prefix}");
    let step = (candidates.len() / maximum.max(1)).max(1);
    let cancel = AtomicBool::new(false);
    let mut records = Vec::new();
    let mut scanned = 0;
    for class in candidates.iter().step_by(step).take(maximum) {
        scanned += 1;
        let began = Instant::now();
        let (native, coverage) = engine.decompile_native_class_source_report(class, &cancel)?;
        if coverage["counts"]["dex-fallback"].as_u64().unwrap_or(0) == 0 {
            continue;
        }
        if let Some(exclude) = &exclude_reason {
            let interesting = coverage["methods"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|method| {
                    method["status"] == "dex-fallback"
                        && !exclude.is_match(method["reason"].as_str().unwrap_or(""))
                });
            if !interesting {
                continue;
            }
        }
        let index = records.len();
        std::fs::write(output.join(format!("{index}-native.txt")), &native.source)?;
        engine.set_alternate_source_enabled(true);
        let (alternate, alternate_coverage) =
            engine.decompile_class_source_report(class, &cancel)?;
        let first_ms = began.elapsed().as_millis();
        let cached = Instant::now();
        let (_, cached_coverage) = engine.decompile_class_source_report(class, &cancel)?;
        let cached_ms = cached.elapsed().as_millis();
        std::fs::write(
            output.join(format!("{index}-alternate.txt")),
            &alternate.source,
        )?;
        let dex = engine.decompile_with_metadata(&format!("dex://{class}"))?;
        std::fs::write(output.join(format!("{index}-original-dex.txt")), dex.source)?;
        records.push(json!({"class":class,"native":coverage,"alternate":alternate_coverage,"cached":cached_coverage,"first_ms":first_ms,"cached_ms":cached_ms}));
        eprintln!(
            "{}: {}",
            class,
            records.last().context("record")?["alternate"]["alternate"]
        );
        if records.len() == 5 {
            break;
        }
    }
    std::fs::write(
        output.join("comparison.json"),
        serde_json::to_vec_pretty(
            &json!({"input":args[1],"scanned":scanned,"total_classes":project.classes.len(),"prefix":prefix,"records":records}),
        )?,
    )?;
    Ok(())
}
