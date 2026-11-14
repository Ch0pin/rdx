# apkparser (Rust)

Rust port of the Python [`apkparser`](../README.md) library. Parses Android APK / APKM files: lenient ZIP, signature (v1/JAR, v2, v3), binary manifest (AXML), and permissions.

## Quick start

```bash
cd apkparser-rs
cargo build --release
cargo run --bin apkparser -- ../tests/data/APK/TestActivity.apk --fingerprints -v
```

Real CLI output:

```text
=== ../tests/data/APK/TestActivity.apk ===
  kind: APK
  is_apk: true
  files: 10 entries
  manifest:
    package: tests.androguard
    versionCode: 1
    versionName: 1.0
    minSdkVersion: 9
    targetSdkVersion: 16
  signature:
    v1 (JAR): true
    v2: false
    v3: false
    v1 entry: META-INF/CERT.RSA
    v1 cert SHA-256: 6f5c31608f1f9e285eb6343c7c8af07de81c1fb2148b5349bec906444144576d
```

## Library examples

All snippets use `../tests/data/APK/TestActivity.apk` from the repo root.

### Load & inspect

```rust
use apkparser::{Apk, ApkOptions};

fn main() -> apkparser::Result<()> {
    let bytes = std::fs::read("../tests/data/APK/TestActivity.apk")?;
    let mut apk = Apk::from_bytes(
        &bytes,
        ApkOptions::default()
            .with_axml(true)
            .with_signature(true),
    )?;

    assert!(apk.is_apk());
    assert_eq!(apk.get_files().len(), 10);

    let m = apk.get_android_manifest().unwrap();
    assert_eq!(m.package.as_deref(), Some("tests.androguard"));
    assert_eq!(m.min_sdk_version, Some(9));
    assert_eq!(m.target_sdk_version, Some(16));
    assert_eq!(m.version_code, Some(1));
    assert_eq!(m.version_name.as_deref(), Some("1.0"));

    let dex = apk.get_file("classes.dex")?;
    assert!(dex.starts_with(b"dex\n"));

    let sig = apk.get_signature_mut().unwrap();
    assert!(sig.is_signed_v1());
    assert!(!sig.is_signed_v2());
    assert_eq!(sig.get_signature_name().as_deref(), Some("META-INF/CERT.RSA"));
    let cert = sig.get_certificate_der("META-INF/CERT.RSA")?.unwrap();
    // SHA-256: 6f5c31608f1f9e285eb6343c7c8af07de81c1fb2148b5349bec906444144576d
    assert_eq!(cert.len(), 489);
    Ok(())
}
```

### v1 + v2 signed APK

```rust
use apkparser::{Apk, ApkOptions};

let bytes = std::fs::read("../tests/data/APK/TestActivity_signed_both.apk")?;
let mut apk = Apk::from_bytes(
    &bytes,
    ApkOptions::default().with_axml(true).with_signature(true),
)?;
let sig = apk.get_signature_mut().unwrap();
assert!(sig.is_signed_v1() && sig.is_signed_v2());
assert_eq!(sig.get_signature_name().as_deref(), Some("META-INF/ANDROGUA.RSA"));
let certs_v2 = sig.get_certificates_der_v2()?;
assert_eq!(certs_v2.len(), 1);
```

### Permissions

```rust
use apkparser::{Apk, ApkOptions};

let bytes = std::fs::read("../tests/data/APK/a2dp.Vol_137.apk")?;
let apk = Apk::from_bytes(
    &bytes,
    ApkOptions::default().with_axml(true).with_permission(true),
)?;
let m = apk.get_android_manifest().unwrap();
assert_eq!(m.package.as_deref(), Some("a2dp.Vol"));
assert!(m.uses_permissions.contains(&"android.permission.BLUETOOTH".into()));
```

### APKM

```rust
use apkparser::{looks_like_apkm, unwrap_to_apk_bytes, ApkmArchive, Apk, ApkOptions};

let raw = std::fs::read("app.apkm")?;
assert!(looks_like_apkm(&raw));
let base = unwrap_to_apk_bytes(&raw)?;
let apk = Apk::from_bytes(&raw, ApkOptions::default().with_axml(true))?; // auto-unwraps

let archive = ApkmArchive::from_bytes(&raw)?;
archive.extract_apks_to(std::path::Path::new("./out"))?;
```

```bash
cargo run --bin apkparser -- app.apkm --list-splits
cargo run --bin apkparser -- app.apkm --extract-apks ./out
```

## CLI options

| Option | Description |
|--------|-------------|
| `PACKAGE...` | One or more APK / APKM path(s) |
| `--axml` / `--no-axml` | Parse AndroidManifest.xml (default: true) |
| `--signature` / `--no-signature` | Parse v1/v2/v3 signature (default: true) |
| `--permission` | Load AOSP permissions JSON |
| `-l, --list-files` | List ZIP entries (base APK if APKM) |
| `--list-splits` | List APKM base + split entries |
| `--extract-apks DIR` | Extract APKM `*.apk` members |
| `--fingerprints` | Certificate SHA-256 fingerprints |
| `-v, --verbose` | Verbose (e.g. list uses-permission) |

## Layout

- `src/lib.rs` — public API
- `src/zip.rs` — lenient ZIP (malformed Extra Field / tampered methods; see [Octo2](https://hatching.io/blog/triage-insights-ep4/))
- `src/signature/` — v1 / v2 / v3
- `src/manifest.rs` — AXML via `axmldecoder`
- `src/permissions.rs` — AOSP permissions by API level
- `src/apkm.rs` — APKM containers
- `src/apk.rs` — `Apk` + `ApkOptions`
- `src/main.rs` — CLI
- `tests/` — integration tests (`../tests/data/APK/`)

## Python parity

| Python | Rust |
|--------|------|
| `APK`, `OPTION_*` | `Apk`, `ApkOptions` |
| `apkparser.zip` | `apkparser::zip` |
| `apkparser.signature` | `apkparser::signature` |
| `apkparser.permissions` | `apkparser::permissions` |
| `is_android_raw` | `is_android_raw` |
| APKM helpers | `looks_like_apkm`, `unwrap_to_apk_bytes`, `ApkmArchive` |

Full Python docs and side-by-side examples: [../README.md](../README.md).

## Build and test

```bash
cargo build
cargo test
```
