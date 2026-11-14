//! CLI for the DEX parser: parse a DEX file and print header, classes, methods, fields, and optionally strings.

use clap::Parser;
use dex_parser::{DexFile, DexHelper};
use std::fs;
use std::process;

#[derive(Parser, Debug)]
#[command(name = "dexparser")]
#[command(about = "DEX Parser", long_about = None)]
struct Args {
    /// Input DEX file
    #[arg(short, long)]
    input: String,

    /// Extract and print all strings from the DEX
    #[arg(short, long)]
    strings: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() {
    let args = Args::parse();

    let bytes = match fs::read(&args.input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error reading {}: {}", args.input, e);
            process::exit(1);
        }
    };

    let dex = match DexFile::parse(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Parse error: {}", e);
            process::exit(1);
        }
    };

    let helper = DexHelper::from_dex(&dex);

    // Header
    let h = &dex.header;
    println!("header:");
    println!("  file_size={} header_size={} endian_tag=0x{:08x}", h.file_size, h.header_size, h.endian_tag);
    println!("  string_ids_size={} type_ids_size={} proto_ids_size={}", h.string_ids_size, h.type_ids_size, h.proto_ids_size);
    println!("  field_ids_size={} method_ids_size={} class_defs_size={}", h.field_ids_size, h.method_ids_size, h.class_defs_size);
    println!();

    // Classes
    println!("CLASSES:");
    for class in helper.classes() {
        match class {
            Ok(c) => println!("  CLASS {} (super: {})", c.name, c.superclass_name),
            Err(e) => println!("  CLASS error: {}", e),
        }
    }
    println!();

    // Methods
    println!("METHODS:");
    for method in helper.methods() {
        match method {
            Ok(m) => {
                println!("  METHOD {} {} -> {} ({:?})", m.info.class, m.info.name, m.info.return_type, m.method_type);
                if args.verbose {
                    println!("    params: {:?} method_idx={} code_off={}", m.info.params, m.method_idx, m.code_off);
                }
                if let Some(ref code) = m.code_item {
                    println!("    CODE debug_info_off={} insns_size={} ({} bytes)", code.debug_info_off, code.insns_size, code.insns_size as usize * 2);
                }
            }
            Err(e) => println!("  METHOD error: {}", e),
        }
    }
    println!();

    // Fields
    println!("FIELDS:");
    for field in helper.fields() {
        match field {
            Ok(f) => println!("  FIELD {} {} {} ({:?})", f.info.class, f.info.name, f.info.typ, f.field_kind),
            Err(e) => println!("  FIELD error: {}", e),
        }
    }
    println!();

    // Strings (optional)
    if args.strings {
        println!("STRINGS:");
        for (idx, s) in helper.strings().enumerate() {
            match s {
                Ok(text) => println!("  [{}] {}", idx, text.replace('\n', "\\n")),
                Err(e) => println!("  [{}] error: {}", idx, e),
            }
        }
    }
}
