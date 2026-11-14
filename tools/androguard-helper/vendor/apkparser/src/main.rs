//! CLI to test the apkparser library with APK files.

use apkparser::{is_android_raw, looks_like_apkm, Apk, ApkOptions, ApkmArchive};
use clap::Parser;
use sha2::Digest;
use std::path::PathBuf;
use std::process;

#[derive(Parser, Debug)]
#[command(name = "apkparser")]
#[command(about = "Parse and inspect Android APK / APKM files", long_about = None)]
struct Args {
    /// APK or APKM file path(s)
    #[arg(required = true, value_name = "PACKAGE")]
    apks: Vec<PathBuf>,

    /// Parse AndroidManifest.xml (package, version, permissions)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    axml: bool,

    /// Parse signature (v1/v2/v3, certificates)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    signature: bool,

    /// Load AOSP permissions (requires --axml and aosp_permissions data)
    #[arg(long, default_value_t = false)]
    permission: bool,

    /// List all files inside the APK (base APK when input is APKM)
    #[arg(short, long)]
    list_files: bool,

    /// For APKM inputs: list base + split APK entries
    #[arg(long)]
    list_splits: bool,

    /// For APKM inputs: extract base + split APKs to this directory
    #[arg(long, value_name = "DIR")]
    extract_apks: Option<PathBuf>,

    /// Show certificate SHA-256 fingerprints
    #[arg(long)]
    fingerprints: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let options = ApkOptions::default()
        .with_axml(args.axml)
        .with_signature(args.signature)
        .with_permission(args.permission);

    for path in &args.apks {
        if !path.exists() {
            eprintln!("Error: file not found: {}", path.display());
            process::exit(1);
        }

        let data = std::fs::read(path)?;
        let kind = is_android_raw(&data).unwrap_or("unknown");
        println!("=== {} ===", path.display());
        println!("  kind: {}", kind);

        if looks_like_apkm(&data) {
            let archive = match ApkmArchive::from_bytes(&data) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("Error parsing APKM {}: {}", path.display(), e);
                    process::exit(1);
                }
            };
            println!("  apkm base: {}", archive.base_name());
            println!("  apkm splits: {}", archive.split_names().len());
            if args.list_splits || args.verbose {
                for e in archive.entries() {
                    println!("    [{:?}] {} ({} bytes)", e.kind, e.name, e.size);
                }
            }
            if let Some(info) = archive.info_json() {
                if args.verbose {
                    println!("  info.json: {}", info);
                }
            }
            if let Some(ref dir) = args.extract_apks {
                let written = archive.extract_apks_to(dir)?;
                println!("  extracted {} apk(s) to {}", written.len(), dir.display());
            }
        }

        let mut apk = match Apk::from_bytes(&data, options) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Error parsing {}: {}", path.display(), e);
                process::exit(1);
            }
        };

        println!("  is_apk: {}", apk.is_apk());

        let files = apk.get_files();
        println!("  files: {} entries", files.len());

        if args.list_files {
            for name in files {
                println!("    {}", name);
            }
        }

        if args.axml {
            if let Some(m) = apk.get_android_manifest() {
                println!("  manifest:");
                if let Some(ref p) = m.package {
                    println!("    package: {}", p);
                }
                if let Some(v) = m.version_code {
                    println!("    versionCode: {}", v);
                }
                if let Some(ref v) = m.version_name {
                    println!("    versionName: {}", v);
                }
                if let Some(v) = m.min_sdk_version {
                    println!("    minSdkVersion: {}", v);
                }
                if let Some(v) = m.target_sdk_version {
                    println!("    targetSdkVersion: {}", v);
                }
                if !m.uses_permissions.is_empty() {
                    println!("    uses-permission: {} item(s)", m.uses_permissions.len());
                    if args.verbose {
                        for p in &m.uses_permissions {
                            println!("      - {}", p);
                        }
                    }
                }
                if !m.permissions.is_empty() {
                    println!("    permission (declared): {} item(s)", m.permissions.len());
                }
            } else {
                println!("  manifest: (not parsed or missing)");
            }
        }

        if args.signature {
            if let Some(sig) = apk.get_signature() {
                println!("  signature:");
                println!("    v1 (JAR): {}", sig.is_signed_v1());
                println!("    v2: {}", sig.is_signed_v2());
                println!("    v3: {}", sig.is_signed_v3());
                if sig.is_signed_v1() {
                    if let Some(name) = sig.get_signature_name() {
                        println!("    v1 entry: {}", name);
                        if args.fingerprints {
                            if let Ok(Some(cert)) = sig.get_certificate_der(&name) {
                                println!("    v1 cert SHA-256: {}", sha256_hex(&cert));
                            }
                        }
                    }
                }
                if (sig.is_signed_v2() || sig.is_signed_v3()) && args.fingerprints {
                    if let Some(sig_mut) = apk.get_signature_mut() {
                        if let Ok(certs_v2) = sig_mut.get_certificates_der_v2() {
                            for (i, c) in certs_v2.iter().enumerate() {
                                println!("    v2 cert[{}] SHA-256: {}", i, sha256_hex(c));
                            }
                        }
                        if let Ok(certs_v3) = sig_mut.get_certificates_der_v3() {
                            for (i, c) in certs_v3.iter().enumerate() {
                                println!("    v3 cert[{}] SHA-256: {}", i, sha256_hex(c));
                            }
                        }
                    }
                }
            } else {
                println!("  signature: (not parsed)");
            }
        }

        if args.permission {
            if let Some(perms) = apk.get_permissions() {
                println!(
                    "  permissions (AOSP): {} requested",
                    perms.permissions.len()
                );
                if args.verbose {
                    for p in perms.get_requested_aosp_permissions() {
                        println!("    - {}", p);
                    }
                }
            }
        }

        println!();
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
