//! RDX adapter; upstream algorithms are unchanged. One class per process.
use anyhow::{ensure, Context, Result};
use dex_decompiler::{parse_dex, slice_class_from_input, Decompiler, DecompilerOptions};
use serde_json::json;
use std::io::Write;

const BACKEND: &str = "androguard-f7fa4a4853d957ebc3c54355d128a3835ef5ae22-rdx2";

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") {
        println!("{BACKEND}");
        return Ok(());
    }
    ensure!(args.len() == 3, "usage: rdx-androguard-helper DEX CLASS");
    ensure!(
        std::fs::metadata(&args[1])?.len() <= 256 * 1024 * 1024,
        "DEX exceeds 256 MiB"
    );
    let input = std::fs::read(&args[1])?;
    let sliced = slice_class_from_input(&input, &args[2])?;
    drop(input);
    let dex = parse_dex(&sliced)?;
    let decompiler = Decompiler::with_options(&dex, DecompilerOptions::default());
    let mut methods = Vec::new();
    for class in dex.class_defs() {
        let class = class?;
        let owner = dex.get_type(class.class_idx)?;
        ensure!(
            owner == format!("L{};", args[2].replace('.', "/")),
            "unexpected sliced class"
        );
        let data = dex
            .get_class_data(&class)?
            .context("class has no method data")?;
        for encoded in data.direct_methods.iter().chain(&data.virtual_methods) {
            let info = dex.get_method_info(encoded.method_idx)?;
            // Raw descriptors are kept: display names lose distinctions in obfuscated DEX.
            let id = format!(
                "{}.{}({}){}",
                args[2],
                info.name,
                info.params.join(""),
                info.return_type
            );
            let result =
                decompiler.decompile_method(encoded, args[2].rsplit('.').next(), Some(&args[2]));
            methods.push(match result {
                Ok(java) => json!({"id":id,"java":java}),
                Err(error) => json!({"id":id,"error":error.to_string()}),
            });
        }
    }
    let bytes = serde_json::to_vec(
        &json!({"protocol":1,"backend":BACKEND,"class":args[2],"methods":methods}),
    )?;
    ensure!(bytes.len() <= 8 * 1024 * 1024, "result exceeds 8 MiB");
    std::io::stdout().lock().write_all(&bytes)?;
    Ok(())
}
