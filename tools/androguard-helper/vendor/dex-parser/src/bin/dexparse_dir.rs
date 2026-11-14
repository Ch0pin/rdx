//! Parse all DEX files in a directory and print time per file.
//! With the `disasm` feature, also disassembles all method bytecode (via dex-bytecode) and reports parse + disasm times.

use clap::Parser;
use dex_parser::is_dex;
use dex_parser::DexFile;
#[cfg(feature = "disasm")]
use dex_parser::DexHelper;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "dexparse-dir")]
#[command(about = "Parse all DEX files in a directory with time measurement")]
struct Args {
    /// Directory to scan for DEX files
    #[arg(short, long)]
    dir: String,

    /// Scan subdirectories recursively
    #[arg(short, long)]
    recursive: bool,

    /// Only consider files with .dex extension (default: detect by DEX magic in file content)
    #[arg(long)]
    by_extension: bool,
}

fn collect_dex_paths(dir: &Path, recursive: bool, by_extension: bool) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                out.extend(collect_dex_paths(&path, true, by_extension));
            }
        } else if path.is_file() {
            let include = if by_extension {
                path.extension().map_or(false, |e| e == "dex")
            } else {
                let mut head = [0u8; 4];
                fs::File::open(&path)
                    .and_then(|mut f| f.read_exact(&mut head))
                    .is_ok() && is_dex(&head)
            };
            if include {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[cfg(feature = "disasm")]
fn disasm_all_methods(dex: &DexFile, data: &[u8]) -> Result<usize, dex_bytecode::DexError> {
    let helper = DexHelper::from_dex(dex);
    let mut total_insns = 0usize;
    for method in helper.methods() {
        let Ok(m) = method else { continue };
        if let Some(ref code) = m.code_item {
            let insns = code.insns_slice(data);
            let instructions = dex_bytecode::decode_all(insns, 0)?;
            total_insns += instructions.len();
        }
    }
    Ok(total_insns)
}

fn main() {
    let args = Args::parse();
    let dir = Path::new(&args.dir);

    if !dir.is_dir() {
        eprintln!("Not a directory: {}", args.dir);
        std::process::exit(1);
    }

    let paths = collect_dex_paths(dir, args.recursive, args.by_extension);
    if paths.is_empty() {
        eprintln!(
            "No DEX files found in {} ({})",
            args.dir,
            if args.by_extension {
                "no .dex files; default is to detect by magic"
            } else {
                "no file had DEX magic"
            }
        );
        std::process::exit(1);
    }

    let note = if args.by_extension { " (.dex only)" } else { " (by magic)" };
    println!("Parsing {} DEX file(s) in {}{} ...\n", paths.len(), args.dir, note);

    let mut total_ns: u128 = 0;
    let mut ok = 0usize;
    let mut err = 0usize;

    for path in &paths {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  {}  ERROR reading: {}", path.display(), e);
                err += 1;
                continue;
            }
        };

        let start_parse = Instant::now();
        let result = DexFile::parse(&bytes);
        let parse_elapsed = start_parse.elapsed();

        match result {
            Ok(dex) => {
                ok += 1;
                #[cfg(feature = "disasm")]
                {
                    let start_disasm = Instant::now();
                    let disasm_result = disasm_all_methods(&dex, &bytes);
                    let disasm_elapsed = start_disasm.elapsed();
                    total_ns += parse_elapsed.as_nanos() + disasm_elapsed.as_nanos();
                    match disasm_result {
                        Ok(ins_count) => {
                            println!(
                                "  {:10.2} ms parse  {:10.2} ms disasm  {}  (classes={} methods={} strings={} insns={})",
                                parse_elapsed.as_secs_f64() * 1000.0,
                                disasm_elapsed.as_secs_f64() * 1000.0,
                                path.display(),
                                dex.header.class_defs_size,
                                dex.header.method_ids_size,
                                dex.header.string_ids_size,
                                ins_count,
                            );
                        }
                        Err(e) => {
                            println!(
                                "  {:10.2} ms parse  {}  (classes={} methods={} strings={})  DISASM ERROR: {}",
                                parse_elapsed.as_secs_f64() * 1000.0,
                                path.display(),
                                dex.header.class_defs_size,
                                dex.header.method_ids_size,
                                dex.header.string_ids_size,
                                e,
                            );
                            total_ns += parse_elapsed.as_nanos();
                        }
                    }
                }
                #[cfg(not(feature = "disasm"))]
                {
                    total_ns += parse_elapsed.as_nanos();
                    println!(
                        "  {:12.2} ms  {}  (classes={} methods={} strings={})",
                        parse_elapsed.as_secs_f64() * 1000.0,
                        path.display(),
                        dex.header.class_defs_size,
                        dex.header.method_ids_size,
                        dex.header.string_ids_size,
                    );
                }
            }
            Err(e) => {
                err += 1;
                total_ns += parse_elapsed.as_nanos();
                println!("  {:12.2} ms  {}  ERROR: {}", parse_elapsed.as_secs_f64() * 1000.0, path.display(), e);
            }
        }
    }

    println!();
    println!(
        "Total: {} ok, {} errors, {:.2} ms overall",
        ok,
        err,
        (total_ns as f64) / 1_000_000.0
    );
}
