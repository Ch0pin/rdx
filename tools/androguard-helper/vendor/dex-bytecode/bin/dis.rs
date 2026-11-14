//! Simple CLI to disassemble Dalvik bytecode (raw instruction bytes).
//!
//! Input can be: a file path, stdin (-), or hex bytes (--hex).
//! Use --blocks to show basic-block structure with arrows and colors in the terminal.

use std::fs;
use std::io::{self, BufWriter, IsTerminal, Read, Write};
use std::path::Path;

use clap::Parser;
use colored::Colorize;
use dex_bytecode::{basic_blocks, branch_targets, cfg_edges, decode_all};
use log::LevelFilter;
use simple_logger::SimpleLogger;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorWhen {
    Always,
    Never,
    Auto,
}

impl std::str::FromStr for ColorWhen {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "always" | "yes" | "true" => Ok(ColorWhen::Always),
            "never" | "no" | "false" => Ok(ColorWhen::Never),
            "auto" => Ok(ColorWhen::Auto),
            _ => Err("must be always, never, or auto".into()),
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Disassemble Dalvik bytecode from a file, stdin, or hex."
)]
struct Args {
    /// Input: file path, or "-" for stdin (default: "-" if no --hex)
    #[arg(short, long)]
    input: Option<String>,

    /// Hex-encoded bytecode to decode (e.g. "700001000000" or "70 00 01 00")
    #[arg(long, conflicts_with = "input")]
    hex: Option<String>,

    /// Start offset in bytes (default: 0)
    #[arg(short, long, default_value = "0")]
    offset: usize,

    /// Show labels at branch targets (e.g. :L00000010)
    #[arg(short, long)]
    labels: bool,

    /// Show basic-block structure with arrows and colors in the terminal (implies --labels)
    #[arg(short = 'b', long)]
    blocks: bool,

    /// Output control-flow graph in DOT (Graphviz) format instead of disassembly
    #[arg(long)]
    dot: bool,

    /// When to use colors: always, never, or auto (default: auto = TTY only)
    #[arg(long, default_value = "auto", value_name = "WHEN")]
    color: ColorWhen,

    /// Omit address column in disassembly
    #[arg(long)]
    no_addresses: bool,

    /// Omit trailing comment (instruction/block count)
    #[arg(short, long)]
    quiet: bool,

    /// Enable debug logs
    #[arg(short, long)]
    debug: bool,
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.replace(|c: char| c.is_whitespace(), "");
    if s.len() % 2 != 0 {
        return Err("hex string must have an even number of nibbles".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "invalid hex digit".into()))
        .collect()
}

fn read_stdin() -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
}

fn main() {
    let args = Args::parse();

    let level = if args.debug {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    SimpleLogger::new().with_level(level).init().unwrap();

    let data: Vec<u8> = if let Some(hex) = &args.hex {
        match parse_hex(hex) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Invalid hex: {}", e);
                std::process::exit(1);
            }
        }
    } else if args.input.as_deref() == Some("-") || args.input.is_none() {
        read_stdin().expect("read stdin")
    } else {
        let path = Path::new(args.input.as_deref().unwrap());
        fs::read(path).expect("read input file")
    };

    if data.is_empty() {
        eprintln!("No input data to decode.");
        std::process::exit(1);
    }

    if args.offset >= data.len() {
        eprintln!(
            "Offset {} past end of data ({} bytes)",
            args.offset,
            data.len()
        );
        std::process::exit(1);
    }

    let slice = &data[args.offset..];
    let base = args.offset;
    let show_blocks = args.blocks;
    let show_labels = args.labels || args.blocks;
    let use_color = args.color == ColorWhen::Always
        || (args.color == ColorWhen::Auto && io::stdout().is_terminal());

    match decode_all(slice, 0) {
        Ok(instructions) => {
            if args.dot {
                let blocks = basic_blocks(&instructions, slice, 0);
                let edges = cfg_edges(&instructions, slice, 0);
                let start_to_idx: std::collections::BTreeMap<u32, usize> = blocks
                    .iter()
                    .enumerate()
                    .map(|(i, b)| (b.start_offset, i))
                    .collect();
                println!("digraph cfg {{");
                println!("  rankdir=TB;");
                println!("  node [shape=box, fontname=\"monospace\"];");
                for (i, b) in blocks.iter().enumerate() {
                    let end_str = if b.end_offset == u32::MAX {
                        "end".to_string()
                    } else {
                        format!("0x{:08x}", base as u32 + b.end_offset)
                    };
                    println!(
                        "  b{} [label=\"block {}\\n0x{:08x}..{}\"];",
                        i,
                        i,
                        base as u32 + b.start_offset,
                        end_str
                    );
                }
                for (from, to) in &edges {
                    if let (Some(&from_idx), Some(&to_idx)) =
                        (start_to_idx.get(from), start_to_idx.get(to))
                    {
                        println!("  b{} -> b{};", from_idx, to_idx);
                    }
                }
                println!("}}");
                return;
            }
            // One pass: precompute branch targets per instruction and label set (avoids N branch_targets in the loop).
            let mut targets_per_ins: Vec<Vec<u32>> = Vec::with_capacity(instructions.len());
            let mut label_offsets = std::collections::BTreeSet::new();
            for ins in &instructions {
                let targets = branch_targets(slice, ins.offset as usize);
                if show_labels {
                    for &t in &targets {
                        label_offsets.insert(t);
                    }
                }
                targets_per_ins.push(targets);
            }

            let blocks = if show_blocks {
                basic_blocks(&instructions, slice, 0)
            } else {
                Vec::new()
            };

            // Map block start offset (relative to slice) -> block index
            let block_start_to_index: std::collections::BTreeMap<u32, usize> = blocks
                .iter()
                .enumerate()
                .map(|(i, b)| (b.start_offset, i))
                .collect();

            let mut out = BufWriter::new(io::stdout().lock());

            for (i, ins) in instructions.iter().enumerate() {
                let rel_offset = ins.offset as u32;
                let abs_offset = (ins.offset as usize) + base;
                let targets = &targets_per_ins[i];

                if show_blocks {
                    let block_idx = block_start_to_index.get(&rel_offset).copied();
                    if let Some(idx) = block_idx {
                        let block = &blocks[idx];
                        if block.start_offset == rel_offset {
                            // Block header
                            if use_color {
                                let header = format!("╭── block {} ", idx);
                                let _ = write!(out, "{}", header.cyan().bold());
                                if block.successors.is_empty() {
                                    let _ = writeln!(out, "{}", "  (no successors)".dimmed());
                                } else {
                                    let succ_str: Vec<String> = block
                                        .successors
                                        .iter()
                                        .map(|&o| format!(":L{:08x}", o as usize + base))
                                        .collect();
                                    let _ = writeln!(
                                        out,
                                        "{} {}",
                                        "→".yellow(),
                                        succ_str.join(", ").green()
                                    );
                                }
                            } else {
                                let _ = write!(out, "╭── block {} ", idx);
                                if block.successors.is_empty() {
                                    let _ = writeln!(out, "  (no successors)");
                                } else {
                                    let succ_str: Vec<String> = block
                                        .successors
                                        .iter()
                                        .map(|&o| format!(":L{:08x}", o as usize + base))
                                        .collect();
                                    let _ = writeln!(out, "→ {}", succ_str.join(", "));
                                }
                            }
                        }
                    }
                }

                if show_labels && label_offsets.contains(&rel_offset) {
                    let label = format!(":L{:08x}", abs_offset);
                    if use_color {
                        let _ = writeln!(out, "{}", label.green().bold());
                    } else {
                        let _ = writeln!(out, "{}", label);
                    }
                }

                let abs_offset_usize = abs_offset;
                let line = if args.no_addresses {
                    format!(
                        "{:20} {} {}",
                        format!("{:02x}", ins.opcode()),
                        ins.mnemonic(),
                        ins.operands()
                    )
                } else {
                    format!(
                        "{:08x}  {:20} {} {}",
                        abs_offset_usize,
                        format!("{:02x}", ins.opcode()),
                        ins.mnemonic(),
                        ins.operands()
                    )
                };

                if show_blocks {
                    let prefix = "│ ";
                    if use_color {
                        let is_branch = !targets.is_empty();
                        if is_branch {
                            let _ = write!(out, "{}", prefix);
                            let _ = writeln!(out, "{}", line.yellow());
                            for t in targets {
                                let target_abs = *t as usize + base;
                                let _ = writeln!(
                                    out,
                                    "│   {} {}",
                                    "→".bright_cyan(),
                                    format!(":L{:08x}", target_abs).green()
                                );
                            }
                        } else {
                            let _ = write!(out, "{}", prefix);
                            let _ = writeln!(out, "{}", line);
                        }
                    } else {
                        let _ = write!(out, "{}", prefix);
                        let _ = writeln!(out, "{}", line);
                        for t in targets {
                            let target_abs = *t as usize + base;
                            let _ = writeln!(out, "│   → :L{:08x}", target_abs);
                        }
                    }
                } else {
                    let _ = writeln!(out, "{}", line);
                }
            }

            if show_blocks {
                if use_color {
                    let _ = writeln!(out, "{}", "╰──".cyan());
                } else {
                    let _ = writeln!(out, "╰──");
                }
            }

            if !args.quiet {
                let _ = writeln!(out, "\n; {} instruction(s)", instructions.len());
                if show_blocks {
                    let _ = writeln!(out, "; {} basic block(s)", blocks.len());
                }
            }

            let _ = out.flush();
        }
        Err(e) => {
            eprintln!("Disassembly error: {}", e);
            std::process::exit(1);
        }
    }
}
