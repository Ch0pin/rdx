//! Standalone source export: progress on stderr, terminal JSON on stdout.
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

pub const USAGE: &str = "rdx [--engine native] --export-source INPUT OUTPUT_DIRECTORY [--package-prefix PREFIX] [--resume]";

struct Options {
    input: PathBuf,
    output: PathBuf,
    package_prefix: String,
    resume: bool,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        ensure!(args.len() >= 2, "Usage: {USAGE}");
        ensure!(
            !args[0].starts_with("--") && !args[1].starts_with("--"),
            "Supply input and output paths before options. Usage: {USAGE}"
        );
        let mut result = Self {
            input: PathBuf::from(&args[0]),
            output: PathBuf::from(&args[1]),
            package_prefix: String::new(),
            resume: false,
        };
        let mut package_seen = false;
        let mut index = 2;
        while index < args.len() {
            match args[index].as_str() {
                "--resume" => {
                    ensure!(!result.resume, "Duplicate --resume");
                    result.resume = true;
                }
                "--package-prefix" => {
                    ensure!(!package_seen, "Duplicate --package-prefix");
                    index += 1;
                    let value = args
                        .get(index)
                        .context("--package-prefix requires a value")?;
                    ensure!(
                        !value.starts_with("--"),
                        "--package-prefix requires a value"
                    );
                    result.package_prefix = value.clone();
                    package_seen = true;
                }
                value => bail!("Unknown export option: {value}. Usage: {USAGE}"),
            }
            index += 1;
        }
        Ok(result)
    }
    fn absolute_output(&self) -> Result<PathBuf> {
        let path = if self.output.is_absolute() {
            self.output.clone()
        } else {
            std::env::current_dir()?.join(&self.output)
        };
        let name = path
            .file_name()
            .context("Output must name an export directory")?;
        let parent = path.parent().context("Missing output parent")?;
        let parent = parent
            .canonicalize()
            .with_context(|| format!("Output parent directory missing: {}", parent.display()))?;
        Ok(parent.join(name))
    }
}

pub fn execute(args: &[String]) -> Result<u8> {
    if args.len() == 1 && args[0] == "--help" {
        println!(
            "{USAGE}\nExport all loaded classes without opening a GUI or MCP connection.\nProgress goes to stderr; the final JSON summary goes to stdout.\nExit codes: 0 complete export, 1 setup/argument error, 2 incomplete export.\nSource may include labeled DEX fallback; inspect manifest coverage.\nUse a new output directory, or --resume for an interrupted export with the same APK and executable."
        );
        return Ok(0);
    }
    let options = Options::parse(args)?;
    let output = options.absolute_output()?;
    let start = Instant::now();
    eprintln!("Loading {:?}", options.input);
    let mut last_log = None;
    let mut last_errors = 0;
    let result = rdx::mcp::export_directory(
        &options.input,
        &output,
        &options.package_prefix,
        options.resume,
        |progress| {
            let errors = progress["failed"].as_u64().unwrap_or(0);
            let now = Instant::now();
            if last_log.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(2))
                || errors != last_errors
                || progress["worker_finished"] == true
            {
                eprintln!(
                    "[{}/{}] exported={}, errors={}, elapsed={}s, status={}",
                    progress["next_class"],
                    progress["total"],
                    progress["exported"],
                    errors,
                    start.elapsed().as_secs(),
                    progress["status"]
                );
                last_log = Some(now);
                last_errors = errors;
            }
        },
    )
    .with_context(|| format!("Export to {} failed", output.display()))?;
    if let Some(error) = result["error"].as_str() {
        eprintln!("Export error: {error}");
    }
    if let Some(error) = result["persistence_error"].as_str() {
        eprintln!("Checkpoint error: {error}");
    }
    eprintln!("Manifest: {}", result["manifest_path"]);
    eprintln!("Status: {}", result["status_path"]);
    serde_json::to_writer(std::io::stdout().lock(), &result)?;
    writeln!(std::io::stdout().lock())?;
    Ok(if result["complete"] == true { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_options_are_explicit_and_reject_conflicts() {
        let parse =
            |args: &[&str]| Options::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let options =
            parse(&["input.apk", "out", "--resume", "--package-prefix", "sample"]).unwrap();
        assert!(options.resume);
        assert_eq!(options.package_prefix, "sample");
        for invalid in [
            vec![],
            vec!["input.apk"],
            vec!["input.apk", "out", "--package-prefix"],
            vec!["input.apk", "out", "--resume", "--resume"],
            vec!["input.apk", "out", "--package-prefix", "--resume"],
            vec!["input.apk", "out", "--overwrite"],
        ] {
            assert!(parse(&invalid).is_err(), "{invalid:?}");
        }
    }
}
