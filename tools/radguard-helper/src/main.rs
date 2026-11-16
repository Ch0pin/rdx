//! RDX adapter. One DEX/class per process; upstream algorithms are unchanged.
use anyhow::{Context, Result, ensure};
use radguard::{Decompiler, writer::SourceOutput};
use serde_json::json;
use std::io::Write;

const BACKEND: &str = "radguard-6535e121a5923b54111619001de7f25872575895-rdx1";

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") {
        println!("{BACKEND}");
        return Ok(());
    }
    ensure!(args.len() == 3, "usage: rdx-radguard-helper DEX CLASS");
    ensure!(
        std::fs::metadata(&args[1])?.len() <= 256 * 1024 * 1024,
        "DEX exceeds 256 MiB"
    );
    let bytes = std::fs::read(&args[1])?;
    let decompiler = Decompiler::parse(&bytes)?.without_metadata();
    let descriptor = format!("L{};", args[2].replace('.', "/"));
    let class = decompiler
        .dex()
        .class(&descriptor)
        .context("class unavailable in owning DEX")?;
    let name = args[2].rsplit('.').next().context("empty class name")?;
    let mut methods = Vec::new();
    for method in class.methods() {
        let resolved = decompiler.dex().method(method.method_idx)?;
        ensure!(resolved.class == descriptor, "unexpected method owner");
        // RadGuard's presentation descriptor inserts spaces between parameters.
        // Rebuild from raw type descriptors so legal spaces inside names survive.
        let parameters = resolved
            .parameter_type_indices
            .iter()
            .map(|index| decompiler.dex().type_descriptor(*index))
            .collect::<radguard::Result<Vec<_>>>()?;
        let id = method_id(&args[2], resolved.name, &parameters, resolved.return_type);
        let output =
            decompiler.write_method(SourceOutput::default(), &descriptor, name, method, &[]);
        methods.push(match output {
            Ok(output) => json!({"id":id,"java":output.source}),
            Err(error) => json!({"id":id,"error":error.to_string()}),
        });
    }
    let output = serde_json::to_vec(
        &json!({"protocol":1,"backend":BACKEND,"class":args[2],"methods":methods}),
    )?;
    ensure!(output.len() <= 8 * 1024 * 1024, "result exceeds 8 MiB");
    std::io::stdout().lock().write_all(&output)?;
    Ok(())
}

fn method_id(owner: &str, name: &str, parameters: &[&str], return_type: &str) -> String {
    format!("{owner}.{name}({}){return_type}", parameters.concat())
}

#[cfg(test)]
mod tests {
    #[test]
    fn raw_descriptors_keep_unicode_and_spaces_without_parameter_separators() {
        assert_eq!(
            super::method_id(
                "sample.Ω class",
                "call 方法",
                &["Lx/has space;", "[I", "Lδ/型;"],
                "V"
            ),
            "sample.Ω class.call 方法(Lx/has space;[ILδ/型;)V"
        );
    }
}
