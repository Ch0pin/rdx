//! CLI: DEX to Java decompiler.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;
use dex_decompiler::{
    build_deobf_rename_map, default_config, exported_classes_from_manifest_xml,
    extract_android_manifest_from_apk, load_android_rules, load_dexes_from_path,
    load_dexes_from_paths, load_mapping_file, looks_like_text_xml,
    mapping_format_from_path, merge_rename_maps, parse_dex, save_mapping_file, scan_dex_parallel,
    scan_dex_semgrep_with_progress, scan_pending_intents_dex_parallel, scan_xml_semgrep,
    solve_dexes, write_issues_json, DecompilationMode, Decompiler, DecompilerOptions,
    DeobfuscateOptions, DexFile, EncodedMethod, RenameMap, SolveOptions, TaintConfig,
};
use indicatif::{ProgressBar, ProgressStyle};

/// Parse offset as decimal or 0x-prefixed hex.
fn parse_offset(s: &str) -> Result<u32, std::num::ParseIntError> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex.trim(), 16)
    } else {
        s.parse::<u32>()
    }
}

/// Simple name: last component after the final dot (e.g. "TestExceptions" from "tests.androguard.TestExceptions").
fn simple_class_name(full: &str) -> &str {
    full.rsplit('.').next().unwrap_or(full)
}

/// Build RenameMap from CLI --rename-*, optional --deobf, and optional --mapping.
fn build_rename_map(args: &Args, dexes: &[&DexFile]) -> Option<RenameMap> {
    let mut map = RenameMap::default();
    let mut any = false;
    if let Some(ref path) = args.mapping {
        match load_mapping_file(std::path::Path::new(path)) {
            Ok(m) => {
                map = merge_rename_maps(map, m);
                any = true;
            }
            Err(e) => {
                eprintln!("warning: failed to load mapping {}: {e}", path);
            }
        }
    }
    if args.deobf {
        let mut opts = DeobfuscateOptions::default();
        if let Some(n) = args.deobf_min {
            opts.min_len = n;
        }
        if let Some(n) = args.deobf_max {
            opts.max_len = n;
        }
        map = merge_rename_maps(map, build_deobf_rename_map(dexes, &opts));
        any = true;
    }
    for s in &args.rename_package {
        if let Some((k, v)) = s.split_once('=') {
            map.package
                .insert(k.trim().to_string(), v.trim().to_string());
            any = true;
        }
    }
    for s in &args.rename_class {
        if let Some((k, v)) = s.split_once('=') {
            map.class.insert(k.trim().to_string(), v.trim().to_string());
            any = true;
        }
    }
    for s in &args.rename_method {
        if let Some((k, v)) = s.split_once('=') {
            map.method
                .insert(k.trim().to_string(), v.trim().to_string());
            any = true;
        }
    }
    for s in &args.rename_field {
        if let Some((k, v)) = s.split_once('=') {
            map.field.insert(k.trim().to_string(), v.trim().to_string());
            any = true;
        }
    }
    for s in &args.rename_variable {
        if let Some((key, rest)) = s.split_once(':') {
            if let Some((old_var, new_var)) = rest.split_once('=') {
                map.variable
                    .entry(key.trim().to_string())
                    .or_default()
                    .insert(old_var.trim().to_string(), new_var.trim().to_string());
                any = true;
            }
        }
    }
    if any {
        Some(map)
    } else {
        None
    }
}

fn parse_decompilation_mode(s: &str) -> std::result::Result<DecompilationMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "auto" | "restructure" => Ok(DecompilationMode::Restructure),
        "simple" => Ok(DecompilationMode::Simple),
        "fallback" => Ok(DecompilationMode::Fallback),
        other => Err(format!(
            "unknown decompilation mode '{other}' (expected restructure|simple|fallback)"
        )),
    }
}

/// Find the first EncodedMethod in the DEX for the given class and method name.
/// Tries exact class name first; if not found, matches any class with the same simple name
/// (so "tests.androguard.TestExceptions" also matches "androguard.test.TestExceptions" in the DEX).
fn find_method(dex: &DexFile, class_name: &str, method_name: &str) -> Option<EncodedMethod> {
    let want_simple = simple_class_name(class_name);

    // First pass: exact match
    for class_def_result in dex.class_defs() {
        let class_def = class_def_result.ok()?;
        let class_type = dex.get_type(class_def.class_idx).ok()?;
        let name = dex_decompiler::java::descriptor_to_java(&class_type);
        if name != class_name {
            continue;
        }
        if let Some(enc) = find_method_in_class(dex, &class_def, method_name) {
            return Some(enc);
        }
    }

    // Fallback: match by simple name (e.g. "TestExceptions" in any package)
    for class_def_result in dex.class_defs() {
        let class_def = class_def_result.ok()?;
        let class_type = dex.get_type(class_def.class_idx).ok()?;
        let name = dex_decompiler::java::descriptor_to_java(&class_type);
        if simple_class_name(&name) != want_simple {
            continue;
        }
        if let Some(enc) = find_method_in_class(dex, &class_def, method_name) {
            return Some(enc);
        }
    }
    None
}

fn find_method_in_class(
    dex: &DexFile,
    class_def: &dex_decompiler::ClassDef,
    method_name: &str,
) -> Option<EncodedMethod> {
    let class_data_opt = dex.get_class_data(class_def).ok()?;
    let class_data = class_data_opt.as_ref()?;
    for encoded in class_data
        .direct_methods
        .iter()
        .chain(class_data.virtual_methods.iter())
    {
        let info = dex.get_method_info(encoded.method_idx).ok()?;
        if info.name == method_name {
            return Some(encoded.clone());
        }
    }
    None
}

/// Format registers for emulator VM state (integers in hex).
fn format_registers_short_hex(regs: &[dex_decompiler::emulator::RegisterInfo]) -> String {
    regs.iter()
        .filter(|r| !matches!(r.value, dex_decompiler::emulator::Value::Unset))
        .map(|r| format!("{}={}", r.name, r.value.display_short_hex()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Run emulator step-by-step, printing each instruction and VM state (colored, hex for integers).
fn run_emulate_verbose(emu: &mut dex_decompiler::emulator::Emulator) -> Result<()> {
    while !emu.finished && emu.step_count < emu.max_steps {
        let result = match emu.step() {
            Ok(r) => r,
            Err(e) => {
                println!("{}", format!("Execution error: {}", e).red());
                break;
            }
        };
        let ins = &result.instruction;
        let snap = &result.state_after;
        println!(
            "{}   {}  {}  {}",
            format!("Step {}:", result.step_count).cyan().bold(),
            format!("0x{:04x}", ins.offset).yellow(),
            ins.mnemonic.green(),
            ins.operands.white()
        );
        println!("  {} {}", "->".dimmed(), result.description.white());
        let regs = format_registers_short_hex(&snap.registers);
        if !regs.is_empty() {
            println!("  {} {}", "Registers:".dimmed(), regs);
        }
        if !snap.heap.is_empty() {
            let heap_str: Vec<String> = snap
                .heap
                .iter()
                .map(|h| match &h.object {
                    dex_decompiler::emulator::HeapObjectKind::Array {
                        element_type,
                        values,
                    } => {
                        let vals: Vec<String> = values
                            .iter()
                            .map(|v| v.display_short_hex())
                            .take(5)
                            .collect();
                        let more = if values.len() > 5 { "..." } else { "" };
                        format!(
                            "@{}={}[{}]({}){}",
                            h.index,
                            element_type,
                            values.len(),
                            vals.join(", "),
                            more
                        )
                    }
                    dex_decompiler::emulator::HeapObjectKind::Instance { class, .. } => {
                        format!("@{}={}", h.index, class)
                    }
                })
                .collect();
            println!("  {} {}", "Heap:".dimmed(), heap_str.join(", "));
        }
        println!();
    }
    Ok(())
}

/// Run emulator with a progress bar; message shows current instruction and register state (hex).
fn run_emulate_progress(emu: &mut dex_decompiler::emulator::Emulator) -> Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    let pb = ProgressBar::new(emu.max_steps as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .expect("template")
            .progress_chars("=>-"),
    );
    while !emu.finished && emu.step_count < emu.max_steps {
        let result = match emu.step() {
            Ok(r) => r,
            Err(e) => {
                pb.finish_with_message(format!("Error: {}", e));
                return Ok(());
            }
        };
        pb.set_position(result.step_count as u64);
        let ins = &result.instruction;
        let regs = format_registers_short_hex(&result.state_after.registers);
        let reg_short = if regs.len() > 60 {
            format!("{}...", &regs[..60])
        } else {
            regs
        };
        let msg = format!(
            "0x{:04x} {} {} | {}",
            ins.offset, ins.mnemonic, ins.operands, reg_short
        );
        pb.set_message(msg);
    }
    pb.finish_with_message(format!("Done ({} steps)", emu.step_count));
    Ok(())
}

/// Run emulator step-by-step; after each instruction print state and wait for Enter (colored, hex).
fn run_emulate_interactive(emu: &mut dex_decompiler::emulator::Emulator) -> Result<()> {
    use std::io::{self, BufRead, Write};
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = io::stdout();
    while !emu.finished && emu.step_count < emu.max_steps {
        let result = match emu.step() {
            Ok(r) => r,
            Err(e) => {
                println!("{}", format!("Execution error: {}", e).red());
                break;
            }
        };
        let ins = &result.instruction;
        let snap = &result.state_after;
        println!(
            "{}   {}  {}  {}",
            format!("Step {}:", result.step_count).cyan().bold(),
            format!("0x{:04x}", ins.offset).yellow(),
            ins.mnemonic.green(),
            ins.operands.white()
        );
        println!("  {} {}", "->".dimmed(), result.description.white());
        let regs = format_registers_short_hex(&snap.registers);
        if !regs.is_empty() {
            println!("  {} {}", "Registers:".dimmed(), regs);
        }
        if !snap.heap.is_empty() {
            let heap_str: Vec<String> = snap
                .heap
                .iter()
                .map(|h| match &h.object {
                    dex_decompiler::emulator::HeapObjectKind::Array {
                        element_type,
                        values,
                    } => {
                        let vals: Vec<String> = values
                            .iter()
                            .map(|v| v.display_short_hex())
                            .take(5)
                            .collect();
                        let more = if values.len() > 5 { "..." } else { "" };
                        format!(
                            "@{}={}[{}]({}){}",
                            h.index,
                            element_type,
                            values.len(),
                            vals.join(", "),
                            more
                        )
                    }
                    dex_decompiler::emulator::HeapObjectKind::Instance { class, .. } => {
                        format!("@{}={}", h.index, class)
                    }
                })
                .collect();
            println!("  {} {}", "Heap:".dimmed(), heap_str.join(", "));
        }
        print!("  {} ", "[Enter to continue, q+Enter to stop]".dimmed());
        stdout.flush()?;
        let mut line = String::new();
        stdin.read_line(&mut line)?;
        if line.trim().eq_ignore_ascii_case("q") {
            println!("{}", "Stopped by user.".yellow());
            break;
        }
        println!();
    }
    Ok(())
}

/// Split params: use semicolon as top-level separator so array params can contain commas.
/// E.g. "5;10" = two ints, "[B]1,2,3;[B]0,0" = two byte arrays.
fn split_params_top_level(s: &str) -> Vec<String> {
    s.split(';')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Parse one array token e.g. "[1,2,3]", "[B]0,1,2", or "[B]1,2,3,4,5" (no trailing ]).
/// Returns (element_type, values).
fn parse_array_token(token: &str) -> Option<(String, Vec<dex_decompiler::emulator::Value>)> {
    let t = token.trim();
    if !t.starts_with('[') {
        return None;
    }
    let after_open = t[1..].trim_start();
    let (elem_ty, rest) = if let Some(close) = after_open.find(']') {
        let ty = after_open[..close].trim();
        let after = after_open[close + 1..].trim_start();
        match ty {
            "B" | "byte" => ("B".to_string(), after),
            "I" | "int" => ("I".to_string(), after),
            _ => {
                // "[1,2,3]" form: list is between [ and last ]
                if t.ends_with(']') {
                    ("I".to_string(), t[1..t.len() - 1].trim())
                } else {
                    ("I".to_string(), after_open)
                }
            }
        }
    } else {
        return None;
    };
    let values: Vec<dex_decompiler::emulator::Value> = if rest.is_empty() {
        vec![]
    } else {
        rest.split(',')
            .map(|s| {
                let s = s.trim().trim_end_matches(']');
                s.parse::<i32>()
                    .map(dex_decompiler::emulator::Value::Int)
                    .unwrap_or(dex_decompiler::emulator::Value::Int(0))
            })
            .collect()
    };
    Some((elem_ty, values))
}

/// Parse --emulate-params into (params, initial_heap). Supports int, long, string, null, and arrays:
/// "[1,2,3]" = int array, "[B]0,1,2" or "[byte]0,1,2" = byte array. Arrays are pushed to initial_heap
/// and params get Value::Ref(index).
fn parse_emulate_params(
    s: &str,
) -> (
    Vec<dex_decompiler::emulator::Value>,
    Vec<dex_decompiler::emulator::HeapObject>,
) {
    use dex_decompiler::emulator::{HeapObject, HeapObjectKind, Value};
    let tokens = split_params_top_level(s);
    let mut params = Vec::new();
    let mut initial_heap = Vec::new();
    for token in tokens {
        let t = token.trim();
        if t.eq_ignore_ascii_case("null") {
            params.push(Value::Null);
        } else if let Some((elem_ty, values)) = parse_array_token(t) {
            let obj = HeapObject {
                kind: HeapObjectKind::Array {
                    element_type: elem_ty.clone(),
                    values,
                },
            };
            initial_heap.push(obj);
            params.push(Value::Ref(initial_heap.len() - 1));
        } else if let Ok(v) = t.parse::<i32>() {
            params.push(Value::Int(v));
        } else if let Ok(v) = t.parse::<i64>() {
            params.push(Value::Long(v));
        } else {
            params.push(Value::Str(t.trim_matches('"').to_string()));
        }
    }
    (params, initial_heap)
}

fn dalvik_descriptor(class: &str) -> String {
    let n = class.trim();
    if n.starts_with('L') && n.ends_with(';') {
        return n.to_string();
    }
    format!("L{};", n.replace('.', "/"))
}

fn run_getclass(args: &Args, class: &str) -> Result<()> {
    if args.input.len() != 1 {
        anyhow::bail!("--getclass expects exactly one input path");
    }
    let path = &args.input[0];
    let bytes = fs::read(path).with_context(|| format!("read {path}"))?;
    let mode =
        parse_decompilation_mode(&args.decompilation_mode).map_err(|e| anyhow::anyhow!(e))?;
    let options = DecompilerOptions {
        only_package: args.only_package.clone(),
        exclude: args.exclude.clone(),
        show_bytecode: args.show_bytecode,
        rename_map: None,
        mode,
        use_debug_names: !args.no_debug_info,
        resource_map: None,
    };
    let java = dex_decompiler::getclass_java(&bytes, class, options)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(ref out) = args.output {
        fs::write(out, &java).with_context(|| format!("write {out}"))?;
    } else {
        print!("{java}");
    }
    Ok(())
}

fn run_findrefs(args: &Args, kind: &str) -> Result<()> {
    let value = args
        .findrefs_value
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--findrefs requires --findrefs-value"))?;
    let class_desc = args
        .findrefs_class
        .as_ref()
        .map(|c| dalvik_descriptor(c));
    let exact_class = !args.findrefs_fuzzy_class;

    // Single APK/DEX path: parallel inflate+FastRef (ASC-shaped). Multi-input
    // falls back to sequential load.
    let hits: Vec<(usize, dex_decompiler::FastRefSite)> = if args.input.len() == 1 {
        dex_decompiler::findrefs_on_apk_path(
            std::path::Path::new(&args.input[0]),
            kind,
            value,
            class_desc.as_deref(),
            exact_class,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        let owned = load_dexes_from_paths(
            &args
                .input
                .iter()
                .map(std::path::Path::new)
                .collect::<Vec<_>>(),
        )?;
        dex_decompiler::findrefs_kind_on_dexes(
            &owned,
            kind,
            value,
            class_desc.as_deref(),
            exact_class,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_iter()
        .flat_map(|(i, sites)| sites.into_iter().map(move |s| (i, s)))
        .collect()
    };

    if kind == "extends" || kind == "impl" {
        println!("{kind} {value:?}: {} class(es)", hits.len());
        for (_i, s) in hits.iter().take(500) {
            println!("  {}", s.class_name);
        }
        return Ok(());
    }

    println!("{kind} {value:?}: {} site(s)", hits.len());
    for (i, s) in hits.iter().take(200) {
        println!(
            "  DEX[{i}] {}#{}  @0x{:x}  pool={}",
            s.class_name, s.method_name, s.file_offset, s.pool_idx
        );
    }
    if hits.len() > 200 {
        println!("  ... ({} more)", hits.len() - 200);
    }

    if args.fast_xref && kind == "method" {
        // Optional caller expansion still needs a full DEX load for the pools hit.
        let owned = load_dexes_from_paths(
            &args
                .input
                .iter()
                .map(std::path::Path::new)
                .collect::<Vec<_>>(),
        )?;
        let mut seen = std::collections::HashSet::new();
        for (dex_i, s) in &hits {
            if !seen.insert(s.pool_idx) {
                continue;
            }
            let Some(dex) = owned.get(*dex_i) else { continue };
            let info = dex_decompiler::xref::find_method_callers_fast(dex, s.pool_idx)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "  callers of pool {}: {} (fast-xref)",
                s.pool_idx,
                info.callers.len()
            );
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    if let Some(ref kind) = args.findrefs {
        return run_findrefs(&args, kind);
    }

    if let Some(ref class) = args.getclass {
        return run_getclass(&args, class);
    }

    // Emulate: run method with given params and print console output + return value.
    if let Some(ref emulate_spec) = args.emulate {
        let (class_name, method_name) = match emulate_spec.split_once('#') {
            Some((c, m)) => (c.trim(), m.trim()),
            None => anyhow::bail!("--emulate must be CLASS#METHOD (e.g. com.example.Main#foo)"),
        };
        let mut encoded = None;
        let mut dex = None;
        for path in &args.input {
            let data = fs::read(path).with_context(|| format!("read {}", path))?;
            let d = parse_dex(&data).context("parse DEX")?;
            if let Some(enc) = find_method(&d, class_name, method_name) {
                encoded = Some(enc);
                dex = Some(d);
                break;
            }
        }
        let (dex, encoded) = match (dex, encoded) {
            (Some(d), Some(e)) => (d, e),
            _ => anyhow::bail!(
                "method {}#{} not found in any of {} DEX file(s)",
                class_name,
                method_name,
                args.input.len()
            ),
        };
        let decompiler = Decompiler::new(&dex);
        let (params, initial_heap) = args
            .emulate_params
            .as_deref()
            .map(parse_emulate_params)
            .unwrap_or_else(|| (Vec::new(), Vec::new()));
        let mut emu = decompiler
            .build_emulator(&encoded, params.clone(), initial_heap)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let max_steps = args.emulate_max_steps.unwrap_or(10_000);
        emu.max_steps = max_steps;

        println!(
            "{}",
            format!("=== Emulate {}#{} ===", class_name, method_name)
                .cyan()
                .bold()
        );
        if !params.is_empty() {
            println!("{} {:?}", "Params:".dimmed(), params);
        }
        println!("{} {}", "Max steps:".dimmed(), max_steps);
        println!();

        if args.emulate_interactive {
            run_emulate_interactive(&mut emu)?;
        } else if args.emulate_verbose {
            run_emulate_verbose(&mut emu)?;
        } else if args.emulate_progress {
            run_emulate_progress(&mut emu)?;
        } else {
            match emu.run_to_end() {
                Ok(()) => {}
                Err(e) => println!("{}", format!("Execution error: {}", e).red()),
            }
        }

        if !emu.console_output.is_empty() {
            println!("\n{}", "--- Console output ---".dimmed());
            for line in &emu.console_output {
                print!("{}", line);
            }
            println!();
        }

        if let Some(ref v) = emu.return_value {
            println!("{}", "--- Return value ---".dimmed());
            println!("{}", v.display_short_hex());
        }
        if let Some(ref e) = emu.exception {
            println!("{}", "--- Exception ---".dimmed());
            println!("{}", e.red());
        }

        println!("{}", "--- Summary ---".dimmed());
        println!("Steps: {}", emu.step_count);
        println!("Finished: {}", emu.finished);
        return Ok(());
    }

    // Data flow / tainting: show reads and writes for a value in a method.
    // With multiple DEX inputs (e.g. classes.dex, classes2.dex), the method is searched in order.
    if let Some(ref taint_method) = args.taint_method {
        let (class_name, method_name) = match taint_method.split_once('#') {
            Some((c, m)) => (c.trim(), m.trim()),
            None => anyhow::bail!(
                "--taint-method must be CLASS#METHOD (e.g. com.example.Main#onCreate)"
            ),
        };
        let mut encoded = None;
        let mut dex = None;
        for path in &args.input {
            let data = fs::read(path).with_context(|| format!("read {}", path))?;
            let d = parse_dex(&data).context("parse DEX")?;
            if let Some(enc) = find_method(&d, class_name, method_name) {
                encoded = Some(enc);
                dex = Some(d);
                break;
            }
        }
        let (dex, encoded) = match (dex, encoded) {
            (Some(d), Some(e)) => (d, e),
            _ => anyhow::bail!(
                "method {}#{} not found in any of {} DEX file(s): {}",
                class_name,
                method_name,
                args.input.len(),
                args.input.join(", ")
            ),
        };
        let decompiler = Decompiler::new(&dex);
        let owned = decompiler
            .value_flow_analysis(&encoded)
            .context("value_flow_analysis (method has no code?)")?;

        let (result, title) = if !args.taint_api.is_empty() {
            let result = owned.value_flow_from_api_sources(&args.taint_api);
            let title = format!("Value flow from API sources: {}", args.taint_api.join(", "));
            (result, title)
        } else if let (Some(taint_offset), Some(taint_reg)) = (
            args.taint_offset
                .as_ref()
                .and_then(|s| parse_offset(s).ok()),
            args.taint_reg,
        ) {
            let result = owned
                .analysis()
                .value_flow_from_seed(taint_offset, taint_reg);
            let title = format!(
                "Value flow from seed (offset=0x{:x}, reg=v{})",
                taint_offset, taint_reg
            );
            (result, title)
        } else {
            anyhow::bail!(
                "with --taint-method specify either --taint-offset and --taint-reg, or --taint-api PATTERN (e.g. --taint-api getLastLocation)"
            );
        };

        println!("{}", title);
        println!("  Writes (defs): {} locations", result.writes.len());
        for (off, reg) in &result.writes {
            println!("    0x{:04x}  v{}", off, reg);
        }
        println!("  Reads (uses): {} locations", result.reads.len());
        for (off, reg) in &result.reads {
            println!("    0x{:04x}  v{}", off, reg);
        }
        return Ok(());
    }

    // PendingIntent vulnerability scan: parallel over methods in all DEX files.
    if args.scan_pending_intent {
        let mut all_findings = Vec::new();
        for path in &args.input {
            let data = fs::read(path).with_context(|| format!("read {}", path))?;
            let dex = parse_dex(&data).context("parse DEX")?;
            all_findings.extend(scan_pending_intents_dex_parallel(&dex));
        }
        println!("PendingIntent scan: {} finding(s)", all_findings.len());
        for f in &all_findings {
            let risk = if f.base_intent_empty && f.dangerous_destination {
                "RISK"
            } else if f.base_intent_empty || f.dangerous_destination {
                "review"
            } else {
                "ok"
            };
            println!(
                "  [{}] {}#{} @ 0x{:x}  base_empty={} dest={}",
                risk,
                f.class_name,
                f.method_name,
                f.invoke_offset,
                f.base_intent_empty,
                f.destination_kind
            );
        }
        return Ok(());
    }

    // Mariana-Trench–style global taint solver (models + rules + interprocedural traces).
    if args.taint_solve {
        let mut config = if let Some(path) = &args.taint_config {
            TaintConfig::from_path(Path::new(path)).context("load taint config")?
        } else {
            default_config()
        };
        if let Some(extra) = &args.taint_config_extra {
            let more =
                TaintConfig::from_path(Path::new(extra)).context("load extra taint config")?;
            config.merge(more);
        }
        let mut opts = SolveOptions::default_android();
        if let Some(n) = args.taint_max_iterations {
            opts.max_iterations = n;
        }
        if args.taint_include_framework {
            opts.exclude_prefixes.clear();
        }
        opts.priority_entry_classes
            .extend(args.taint_priority_entry.iter().cloned());

        let mut owned_dexes = Vec::new();
        for path in &args.input {
            let p = Path::new(path);
            // Prefer APK multi-DEX loader; fall back to bare DEX.
            match load_dexes_from_path(p) {
                Ok(dexes) => {
                    // Best-effort: seed priority entries from decoded text manifests.
                    if opts.priority_entry_classes.is_empty() {
                        if let Ok(data) = fs::read(p) {
                            if let Ok(manifest) = extract_android_manifest_from_apk(&data) {
                                if looks_like_text_xml(&manifest) {
                                    if let Ok(xml) = std::str::from_utf8(&manifest) {
                                        opts.priority_entry_classes
                                            .extend(exported_classes_from_manifest_xml(xml));
                                    }
                                }
                            }
                        }
                    }
                    owned_dexes.extend(dexes);
                }
                Err(_) => {
                    let data = fs::read(path).with_context(|| format!("read {}", path))?;
                    let dex = parse_dex(&data).context("parse DEX")?;
                    owned_dexes.push(dex);
                }
            }
        }
        let refs: Vec<&DexFile> = owned_dexes.iter().collect();
        let result = solve_dexes(&refs, &config, &opts).context("taint solve")?;
        println!(
            "Taint solve: {} issue(s) (methods={}, edges={}, iterations={})",
            result.report.stats.issues,
            result.report.stats.methods_analyzed,
            result.report.stats.call_edges,
            result.report.stats.iterations
        );
        for issue in &result.issues {
            println!(
                "  [rule {}] {} | {} -> {} @ {}",
                issue.rule_code,
                issue.rule_name,
                issue.source_kind,
                issue.sink_kind,
                issue.callable
            );
            for frame in &issue.trace {
                let off = frame
                    .offset
                    .map(|o| format!(" @ 0x{o:x}"))
                    .unwrap_or_default();
                println!(
                    "    - {}#{}{}  [{}] {}",
                    frame.class_name, frame.method_name, off, frame.kind, frame.description
                );
            }
        }
        if let Some(out) = &args.taint_output {
            write_issues_json(&result.report, Path::new(out)).context("write taint output")?;
            println!("wrote {}", out);
        }
        return Ok(());
    }

    // Vulnerability scan: parallel detectors on every method.
    if args.scan_vulns {
        let mut all_findings = Vec::new();
        let logging = if args.taint_api.is_empty() {
            None
        } else {
            Some(args.taint_api.as_slice())
        };
        for path in &args.input {
            let data = fs::read(path).with_context(|| format!("read {}", path))?;
            let dex = parse_dex(&data).context("parse DEX")?;
            all_findings.extend(scan_dex_parallel(&dex, logging));
        }
        println!("Vulnerability scan: {} finding(s)", all_findings.len());
        for f in &all_findings {
            let cwe = f
                .cwe
                .as_ref()
                .map(|c| format!("  {}", c))
                .unwrap_or_default();
            println!(
                "  [{}] {} — {}{}  {}#{}  sink @ 0x{:x}",
                f.severity, f.category, f.title, cwe, f.class_name, f.method_name, f.sink_offset
            );
            if !f.message.is_empty() {
                println!("      {}", f.message.lines().next().unwrap_or(""));
            }
            if !f.source_desc.is_empty() || !f.sink_desc.is_empty() {
                println!("      source: {}  →  sink: {}", f.source_desc, f.sink_desc);
            }
        }
        return Ok(());
    }

    // Native Semgrep-style Android rules (general + OWASP MASTG + optional custom YAML/dir).
    if args.scan_semgrep {
        eprintln!("Semgrep (native): loading rules…");
        let rules_path = args.semgrep_rules.as_ref().map(Path::new);
        let rules = load_android_rules(rules_path).map_err(|e| anyhow::anyhow!(e))?;
        eprintln!(
            "Semgrep (native): {} rule(s) loaded{}",
            rules.len(),
            match rules_path {
                Some(p) => format!(" from {}", p.display()),
                None => " (general Android + OWASP MASTG)".to_string(),
            }
        );
        let mut all = Vec::new();
        for path in &args.input {
            let p = Path::new(path);
            eprintln!("Semgrep (native): reading {}…", path);
            let data = fs::read(p).with_context(|| format!("read {}", path))?;

            // Plaintext XML / decoded AndroidManifest.xml input.
            if looks_like_text_xml(&data)
                || p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
            {
                eprintln!(
                    "Semgrep (native): scanning XML {} ({} rule{})…",
                    path,
                    rules.len(),
                    if rules.len() == 1 { "" } else { "s" }
                );
                if let Ok(text) = std::str::from_utf8(&data) {
                    all.extend(scan_xml_semgrep(text, path, &rules));
                }
                continue;
            }

            eprintln!("Semgrep (native): loading DEX from {}…", path);
            let dexes =
                load_dexes_from_path(p).with_context(|| format!("load DEX/APK {}", path))?;
            eprintln!("Semgrep (native): {} DEX file(s) in {}", dexes.len(), path);
            for (dex_i, dex) in dexes.iter().enumerate() {
                let label = if dexes.len() == 1 {
                    path.to_string()
                } else {
                    format!("{}[{}]", path, dex_i)
                };
                let n_rules = rules.len();
                let use_bar = std::io::IsTerminal::is_terminal(&std::io::stderr());
                let pb = if use_bar {
                    let pb = ProgressBar::new(0);
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template(&format!(
                                "{{spinner:.green}} Semgrep ({n_rules} rules) [{{bar:40.cyan/blue}}] {{pos}}/{{len}} {{msg}}"
                            ))?
                            .progress_chars("=>-"),
                    );
                    pb.enable_steady_tick(std::time::Duration::from_millis(100));
                    Some(Arc::new(pb))
                } else {
                    None
                };
                let last_pct = std::sync::atomic::AtomicUsize::new(0);
                let progress = {
                    let pb = pb.clone();
                    let label = label.clone();
                    move |done: usize, total: usize, method: &str| {
                        if done == 0 {
                            eprintln!(
                                "Semgrep (native): scanning {} with {} rule{} ({} method{})…",
                                label,
                                n_rules,
                                if n_rules == 1 { "" } else { "s" },
                                total,
                                if total == 1 { "" } else { "s" }
                            );
                        }
                        if let Some(ref pb) = pb {
                            pb.set_length(total as u64);
                            pb.set_position(done as u64);
                            pb.set_message(method.to_string());
                        } else if total > 0 {
                            let pct = done.saturating_mul(100) / total;
                            let prev = last_pct.load(std::sync::atomic::Ordering::Relaxed);
                            if pct >= prev + 10 || done == total {
                                if last_pct
                                    .compare_exchange(
                                        prev,
                                        pct,
                                        std::sync::atomic::Ordering::Relaxed,
                                        std::sync::atomic::Ordering::Relaxed,
                                    )
                                    .is_ok()
                                {
                                    eprintln!(
                                        "Semgrep (native): {} ({} rules)… {}/{} ({}%)",
                                        label, n_rules, done, total, pct
                                    );
                                }
                            }
                        }
                    }
                };
                all.extend(scan_dex_semgrep_with_progress(dex, &rules, Some(progress)));
                if let Some(pb) = pb {
                    pb.finish_with_message(format!("done ({label})"));
                } else {
                    eprintln!("Semgrep (native): finished {}", label);
                }
            }

            // XML/MASTG manifest rules need decoded text XML (APK stores binary AXML).
            if let Ok(manifest) = extract_android_manifest_from_apk(&data) {
                if looks_like_text_xml(&manifest) {
                    eprintln!("Semgrep (native): scanning {}!AndroidManifest.xml…", path);
                    if let Ok(text) = std::str::from_utf8(&manifest) {
                        all.extend(scan_xml_semgrep(
                            text,
                            &format!("{path}!AndroidManifest.xml"),
                            &rules,
                        ));
                    }
                } else {
                    eprintln!(
                        "Semgrep (native): {} embeds binary AndroidManifest.xml (AXML); XML/MASTG manifest rules need a decoded text manifest",
                        path
                    );
                }
            }
            // Sibling decoded manifest (e.g. apktool / unpacked APK tree).
            if let Some(parent) = p.parent() {
                let sibling = parent.join("AndroidManifest.xml");
                if sibling.is_file() && sibling != p {
                    if let Ok(bytes) = fs::read(&sibling) {
                        if looks_like_text_xml(&bytes) {
                            eprintln!("Semgrep (native): scanning sibling {}…", sibling.display());
                            if let Ok(text) = std::str::from_utf8(&bytes) {
                                all.extend(scan_xml_semgrep(
                                    text,
                                    &sibling.display().to_string(),
                                    &rules,
                                ));
                            }
                        }
                    }
                }
            }
        }
        println!(
            "Semgrep (native) scan: {} rule(s), {} finding(s)",
            rules.len(),
            all.len()
        );
        for f in &all {
            let off = f
                .sink_offset
                .map(|o| format!(" @ 0x{:x}", o))
                .unwrap_or_default();
            println!(
                "  [{}] {}  {}#{}{}  {}  ({})",
                f.severity, f.rule_id, f.class_name, f.method_name, off, f.sink_desc, f.match_kind
            );
            if !f.message.is_empty() {
                println!("      {}", f.message.lines().next().unwrap_or(""));
            }
        }
        return Ok(());
    }

    // Decompile: DEX and/or APK inputs; multi-DEX merged into one output tree.
    let owned_dexes = load_dexes_from_paths(&args.input.iter().map(Path::new).collect::<Vec<_>>())
        .context("load DEX/APK inputs")?;
    let dex_refs: Vec<&DexFile> = owned_dexes.iter().collect();
    let mode =
        parse_decompilation_mode(&args.decompilation_mode).map_err(|e| anyhow::anyhow!(e))?;
    let rename_map = build_rename_map(&args, &dex_refs);
    if let Some(ref save_path) = args.save_mapping {
        if let Some(ref map) = rename_map {
            let path = Path::new(save_path);
            let fmt = mapping_format_from_path(path);
            save_mapping_file(path, map, fmt)
                .map_err(|e| anyhow::anyhow!("{}", e))
                .with_context(|| format!("write mapping {}", save_path))?;
        } else {
            eprintln!("warning: --save-mapping ignored (no rename map; use --mapping / --deobf / --rename-*)");
        }
    }
    let options = DecompilerOptions {
        only_package: args.only_package.clone(),
        exclude: args.exclude.clone(),
        show_bytecode: args.show_bytecode,
        rename_map,
        mode,
        use_debug_names: !args.no_debug_info,
        resource_map: None,
    };
    let primary = dex_refs[0];
    let _ = primary;

    if let Some(ref json_path) = args.export_json {
        let mut combined = String::from("{\"classes\":[");
        let mut first = true;
        for dex in &dex_refs {
            let d = Decompiler::with_options(dex, options.clone());
            let part = d
                .export_json(args.json_bodies)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            // Merge class arrays from each DEX.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&part) {
                if let Some(arr) = v.get("classes").and_then(|c| c.as_array()) {
                    for c in arr {
                        if !first {
                            combined.push(',');
                        }
                        first = false;
                        combined.push_str(&c.to_string());
                    }
                }
            }
        }
        combined.push_str("]}");
        // Pretty-print if possible
        let pretty = serde_json::from_str::<serde_json::Value>(&combined)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or(combined);
        fs::write(Path::new(json_path), pretty).with_context(|| format!("write {}", json_path))?;
        if args.output_dir.is_none() && args.output.is_none() {
            return Ok(());
        }
    }

    if let Some(ref out_dir) = args.output_dir {
        let base_path = Path::new(out_dir);
        let pb = ProgressBar::new(0);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{bar:40.cyan/blue}] {pos}/{len} {msg}")?
                .progress_chars("=>-"),
        );
        let mut total = 0usize;
        for d in &dex_refs {
            total += Decompiler::with_options(d, options.clone())
                .collect_included_classes()
                .map_err(|e| anyhow::anyhow!("{}", e))?
                .len();
        }
        if total == 0 {
            pb.finish_with_message("no classes to decompile");
            return Ok(());
        }
        pb.set_length(total as u64);
        pb.set_message("starting…");
        let pb = Arc::new(pb);
        let progress = {
            let pb = Arc::clone(&pb);
            move |current: usize, total: usize, class_name: &str| {
                pb.set_length(total as u64);
                pb.set_position(current as u64);
                pb.set_message(class_name.to_string());
            }
        };
        Decompiler::decompile_dexes_to_dir_parallel(&dex_refs, options, base_path, Some(&progress))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        pb.finish_with_message(format!("done ({} DEX)", dex_refs.len()));
    } else {
        let mut java = String::new();
        for (i, dex) in dex_refs.iter().enumerate() {
            let d = Decompiler::with_options(dex, options.clone());
            let part = d.decompile().context("decompile")?;
            if i > 0 && !java.is_empty() && !part.is_empty() {
                java.push('\n');
            }
            java.push_str(&part);
        }
        if let Some(ref out_path) = args.output {
            fs::write(Path::new(out_path), java).with_context(|| format!("write {}", out_path))?;
        } else {
            print!("{}", java);
        }
    }
    Ok(())
}

#[derive(Parser, Debug)]
#[command(
    name = "dex-decompile",
    version,
    about = "DEX to Java decompiler (pure Rust)"
)]
struct Args {
    /// Input DEX and/or APK/ZIP path(s). APK entries yield all classes*.dex. May be repeated.
    #[arg(short, long, required = true, num_args = 1..)]
    input: Vec<String>,

    /// Output Java file (single file; default: stdout)
    #[arg(short, long)]
    output: Option<String>,

    /// Output directory: dump all classes with package structure (e.g. out/com/example/MyClass.java)
    #[arg(short = 'd', long = "output-dir")]
    output_dir: Option<String>,

    /// Decompilation mode: restructure (default), simple, or fallback (jadx-like).
    #[arg(
        short = 'm',
        long = "decompilation-mode",
        default_value = "restructure"
    )]
    decompilation_mode: String,

    /// Disable use of DEX debug_info for local/parameter names.
    #[arg(long = "no-debug-info")]
    no_debug_info: bool,

    /// Auto-rename short / invalid / non-printable class, method, and field names.
    #[arg(long = "deobf")]
    deobf: bool,

    /// Min identifier length for --deobf (default 3).
    #[arg(long = "deobf-min", value_name = "N")]
    deobf_min: Option<usize>,

    /// Max identifier length for --deobf (default 64).
    #[arg(long = "deobf-max", value_name = "N")]
    deobf_max: Option<usize>,

    /// Load a ProGuard/R8, Tiny, or Enigma mapping file (applied before heuristic --deobf).
    #[arg(long = "mapping", value_name = "FILE")]
    mapping: Option<String>,

    /// Write the effective rename map (mapping + --deobf + --rename-*) to a mapping file.
    /// Format is sniffed from the path (`.tiny`, `enigma`, else ProGuard).
    #[arg(long = "save-mapping", value_name = "FILE")]
    save_mapping: Option<String>,

    /// Export class/method inventory as JSON to this path (use with --json-bodies for method Java).
    #[arg(long = "export-json", value_name = "FILE")]
    export_json: Option<String>,

    /// Include decompiled method bodies in --export-json output.
    #[arg(long = "json-bodies")]
    json_bodies: bool,

    /// Only decompile classes in this package (e.g. com.example). Classes in subpackages are included.
    #[arg(long = "only-package")]
    only_package: Option<String>,

    /// Exclude classes in this package (e.g. android). May be repeated. Supports trailing . or .*
    #[arg(long)]
    exclude: Vec<String>,

    /// Data flow / tainting: show where value (offset, reg) is read/written in method CLASS#METHOD.
    /// Use with --taint-offset and --taint-reg. Offset can be decimal or 0x-prefixed hex.
    #[arg(long = "taint-method", value_name = "CLASS#METHOD")]
    taint_method: Option<String>,

    /// Instruction byte offset for taint seed (decimal or 0x hex, e.g. 0 or 0x4).
    #[arg(long = "taint-offset", value_name = "OFFSET")]
    taint_offset: Option<String>,

    /// Register number for taint seed (e.g. 0 for v0).
    #[arg(long = "taint-reg", value_name = "REG")]
    taint_reg: Option<u32>,

    /// Taint returns of Android API methods (e.g. getLastLocation, FusedLocationProviderClient.getLastLocation).
    /// Use with --taint-method. May be repeated. Matches if the resolved method ref contains the pattern.
    #[arg(long = "taint-api", value_name = "PATTERN")]
    taint_api: Vec<String>,

    /// Scan all methods for PendingIntent creation sites (PITracker-like). Reports base Intent emptiness and destination.
    #[arg(long = "scan-pending-intent")]
    scan_pending_intent: bool,

    /// Emit raw DEX instructions as comments before each method body (for debugging).
    #[arg(long = "show-bytecode")]
    show_bytecode: bool,

    /// Run all vulnerability detectors on every method (intent spoofing, RCE dynamic loading, insecure logging, SQL, WebView, hardcoded secrets, IPC). Optional: combine with --taint-api to add logging sources.
    #[arg(long = "scan-vulns")]
    scan_vulns: bool,

    /// Run native Semgrep-style Android rules (general Android + OWASP MASTG by default; SSA/value-flow + Java/XML patterns).
    #[arg(long = "scan-semgrep")]
    scan_semgrep: bool,

    /// Semgrep YAML rules file or directory (default: general.yml + rules/semgrep/android/mastg/).
    #[arg(long = "semgrep-rules", value_name = "PATH")]
    semgrep_rules: Option<String>,

    /// Run the Mariana-Trench–style global taint solver (models, sanitizers, propagations, rules, interprocedural traces).
    #[arg(long = "taint-solve")]
    taint_solve: bool,

    /// JSON taint config (sources/sinks/propagations/sanitizers/rules). Default: embedded Android models.
    #[arg(long = "taint-config", value_name = "PATH")]
    taint_config: Option<String>,

    /// Extra JSON config merged on top of --taint-config / defaults.
    #[arg(long = "taint-config-extra", value_name = "PATH")]
    taint_config_extra: Option<String>,

    /// Write taint IssueReport JSON (SAPP-friendly) to this path.
    #[arg(long = "taint-output", value_name = "PATH")]
    taint_output: Option<String>,

    /// Max interprocedural fixpoint iterations (default 8).
    #[arg(long = "taint-max-iterations", value_name = "N")]
    taint_max_iterations: Option<usize>,

    /// Do not skip android/androidx/kotlin/java framework packages during taint solve.
    #[arg(long = "taint-include-framework")]
    taint_include_framework: bool,

    /// Prioritize these entry classes (Activity/Receiver/…) during taint seeding.
    /// Repeatable. When omitted and input is an APK with a decoded text manifest,
    /// exported components are used when recoverable.
    #[arg(long = "taint-priority-entry", value_name = "CLASS")]
    taint_priority_entry: Vec<String>,

    /// Emulate method CLASS#METHOD with optional params; print console output and return value to stdout.
    #[arg(long = "emulate", value_name = "CLASS#METHOD")]
    emulate: Option<String>,

    /// Comma-separated parameter values for --emulate (e.g. "5,10" or "\"hello\",42"). Strings can be quoted.
    #[arg(long = "emulate-params", value_name = "VALS")]
    emulate_params: Option<String>,

    /// Maximum emulation steps (default: 10000). Use with --emulate.
    #[arg(long = "emulate-max-steps", value_name = "N")]
    emulate_max_steps: Option<usize>,

    /// Emulation verbose mode: print every instruction and VM state (registers, heap) after each step.
    #[arg(long = "emulate-verbose")]
    emulate_verbose: bool,

    /// Emulation progress bar: show step count and current instruction + registers in the bar message.
    #[arg(long = "emulate-progress")]
    emulate_progress: bool,

    /// Emulation step-by-step: after each instruction, print state and wait for Enter before continuing.
    #[arg(long = "emulate-interactive")]
    emulate_interactive: bool,

    /// Rename package in decompiled output (OLD=NEW). May be repeated. E.g. --rename-package com.old=com.new
    #[arg(long = "rename-package", value_name = "OLD=NEW")]
    rename_package: Vec<String>,

    /// Rename class in decompiled output (OLD=NEW). May be repeated. OLD/NEW are full class names. E.g. --rename-class com.old.Main=com.new.MainActivity
    #[arg(long = "rename-class", value_name = "OLD=NEW")]
    rename_class: Vec<String>,

    /// Rename method (ClassName#methodName=NEW). May be repeated. E.g. --rename-method "com.old.Main#onCreate=myOnCreate"
    #[arg(long = "rename-method", value_name = "CLASS#METHOD=NEW")]
    rename_method: Vec<String>,

    /// Rename field (ClassName#fieldName=NEW). May be repeated. E.g. --rename-field "com.old.Main#count=mCount"
    #[arg(long = "rename-field", value_name = "CLASS#FIELD=NEW")]
    rename_field: Vec<String>,

    /// Rename variable in a method (ClassName#methodName:oldVar=newVar). May be repeated. E.g. --rename-variable "com.old.Main#onCreate:p0=context"
    #[arg(long = "rename-variable", value_name = "CLASS#METHOD:OLD=NEW")]
    rename_variable: Vec<String>,

    /// Locate CLASS in the input APK/DEX, slice a minimal DEX, decompile only it.
    #[arg(long = "getclass", value_name = "CLASS")]
    getclass: Option<String>,

    /// Fast findrefs kind: string | type | method | field | extends | impl
    #[arg(long = "findrefs", value_name = "KIND")]
    findrefs: Option<String>,

    /// Needle for --findrefs (substring for string/type, name for method/field).
    #[arg(long = "findrefs-value", value_name = "VALUE")]
    findrefs_value: Option<String>,

    /// Restrict method/field --findrefs to this class (Dalvik or Java name).
    #[arg(long = "findrefs-class", value_name = "CLASS")]
    findrefs_class: Option<String>,

    /// Treat --findrefs-class as a substring (fuzzy).
    #[arg(long = "findrefs-fuzzy-class")]
    findrefs_fuzzy_class: bool,

    /// Use the fast path for existing xref-style method caller queries.
    #[arg(long = "fast-xref")]
    fast_xref: bool,
}
