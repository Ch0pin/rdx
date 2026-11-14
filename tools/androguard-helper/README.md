# Optional isolated Androguard helper

This separate Rust manifest builds the JVM-free `androguard/dex-decompiler` backend. Normal RDX builds have no dependency on it. Fallback is off by default: View → Settings → Androguard fallback. Current macOS/Linux release packages include the helper beside the RDX executable (inside `RDX.app/Contents/MacOS` on macOS), so normal launch needs no environment variable. Windows packages omit the helper and report fallback unavailable. Developers may override discovery with the absolute `RDX_ANDROGUARD_HELPER` path.

From the repository directory, build the pinned helper:

```sh
cargo build --release --locked --manifest-path "$PWD/tools/androguard-helper/Cargo.toml"
```

The executable is `/Users/ch0pin/Desktop/Git-Projects/rdx/tools/androguard-helper/target/release/rdx-androguard-helper`. Distribute the contents of `/Users/ch0pin/Desktop/Git-Projects/rdx/third_party/androguard` with every optional helper binary, including dependency license texts and MPL corresponding source/availability notices. The helper is built through a separate manifest, then included in supported release packages. Ordinary RDX builds still have no helper dependency.

For local macOS packaging, `/Users/ch0pin/Desktop/Git-Projects/rdx/scripts/package_macos.py` discovers the release helper above by default. It accepts `--helper` for a matching architecture and `--output` for a separate app destination. Both RDX binaries and the helper are preflighted before writing the app; all three executables are signed before the bundle signature. Supported release packaging fails with a build instruction when the helper or its notices are missing.

```sh
python3 /Users/ch0pin/Desktop/Git-Projects/rdx/scripts/package_macos.py --binary-dir /Users/ch0pin/Desktop/Git-Projects/rdx/target/release --helper /Users/ch0pin/Desktop/Git-Projects/rdx/tools/androguard-helper/target/release/rdx-androguard-helper --output /Users/ch0pin/Desktop/Git-Projects/rdx/target/RDX.app
```

Cross-target CI builds use `--locked --target` for both manifests and pass the exact target-specific helper path to the packager. CI caches both Cargo workspaces/lockfiles and runs the helper on a DEX fixture on macOS/Linux. The same 100 registry dependencies and preserved notices have been audited for supported macOS and Linux targets; see the platform list in `/Users/ch0pin/Desktop/Git-Projects/rdx/third_party/androguard/dependency-audit.json`.

RDX renders natively first. Only failed methods in the opened class become alternate candidates, matched by original owner, name, parameters and return descriptor. Native methods retain their text. Alternate Java is marked as semantically unverified; original native reasons and DEX retrieval remain available. Constructors and overloads use their exact DEX IDs. Nested/generic/name-rewritten contexts and recognizable placeholder or undeclared-register output remain displayable with visible diagnostics. Every displayed alternate has a red unverified warning, then a collapsed exact Original DEX method, then the reconstruction. These diagnostics do not establish semantic equivalence.

The worker receives one owning DEX member, slices one class, emits per-method JSON, then exits. One worker runs per application process. Normal code search, indexing and bulk export use native output. Bulk alternate generation is intentionally unavailable. Interactive class reopening reuses a private project-session disk cache keyed by input SHA-256, class, pinned backend and fixed options; actual helper/protocol errors and cancellations are retried. Nonempty unverified output is cached with its diagnostics. The cache is limited to 64 MiB with LRU eviction, and its owner removes it when the last project/session clone is dropped. Work files are removed after each attempt. Abrupt operating-system termination can leave a temporary directory for OS cleanup.

Input DEX is limited to 256 MiB; response and stderr are each limited to 8 MiB; wall time and CPU time are limited to 30 seconds. macOS samples resident memory every 20 ms and terminates the worker above 512 MiB; allocation can overshoot between samples. Linux additionally enforces 1 GiB virtual-address/data limits. The RSS threshold is a sampled kill control, not an exact allocation cap. Windows and other platforms currently report fallback unavailable because worker memory enforcement is unsupported. Normal native operation remains available there.

For an explicit real-backend fixture check, build the helper first, then run the ignored test with `RDX_ANDROGUARD_HELPER` set:

```sh
RDX_ANDROGUARD_HELPER=/Users/ch0pin/Desktop/Git-Projects/rdx/tools/androguard-helper/target/release/rdx-androguard-helper cargo test --test androguard_real -- --ignored
```

`/Users/ch0pin/Desktop/Git-Projects/rdx/examples/androguard_comparison.rs` provides a bounded evaluator: `APK OUTPUT_DIRECTORY PREFIX MAX_CLASSES` (maximum 300 sampled classes, stops after five failing classes). It saves native output, alternate output, exact coverage/reasons, original DEX and first/cached latency. Run heavy builds and evaluations serially under the resource guard.
