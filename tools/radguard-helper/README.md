# Isolated RadGuard helper

Pinned native Rust RadGuard adapter for optional per-method reconstruction in RDX.
Build using `cargo build --release --locked --manifest-path /Users/ch0pin/Desktop/Git-Projects/rdx/tools/radguard-helper/Cargo.toml --target-dir /Users/ch0pin/Desktop/Git-Projects/rdx/target`.
Release bundles place `rdx-radguard-helper` beside the RDX executable. There is no JVM requirement.

The helper accepts `DEX CLASS`, writes protocol-1 JSON, and reports exact DEX method descriptors.
RDX limits execution to 30 seconds, 512 MiB sampled resident memory, and 8 MiB output.
RadGuard emits Java-like pseudocode. Every inserted method remains an unverified reconstruction with its original DEX retained.

Upstream: https://github.com/MG1937/radguard/tree/6535e121a5923b54111619001de7f25872575895 (Apache-2.0).
The sole upstream source change makes the existing method writer public; see the packaged method-api.patch and source audit.
