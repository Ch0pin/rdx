# Optional RadGuard component attribution

RadGuard 0.1.4 by WeiMin Cheng (MG1937) is included as a separately built native helper.
Upstream: https://github.com/MG1937/radguard/tree/6535e121a5923b54111619001de7f25872575895
Exact revision: `6535e121a5923b54111619001de7f25872575895` (Apache-2.0; license added 2026-11-16).
The repository LICENSE is retained here and with the vendored source. All upstream source-file notices are preserved.

RadGuard describes its output as Java-like pseudocode and is a Rust implementation of the DAD pipeline.
The separate helper calls RadGuard directly; ASC is not added as an RDX fallback engine.

RDX modification (2026-11-16): `src/decompiler.rs::write_method` is made public so the adapter can request exact encoded methods without parsing Java class text. This visibility-only change leaves upstream algorithms unchanged. `method-api.patch` records the change. `vendored-source-audit.json` records upstream and patched SHA-256 hashes for every retained source file. The source is vendored under `tools/radguard-helper/vendor/radguard`; no upstream test fixtures or binaries are included.

`dependency-audit.json` records locked registry dependencies, declared licenses and preserved notice files under `dependencies`. Embedded libdeflate and mimalloc notices are included. RadGuard uses libdeflater 1.26.1 and mimalloc 0.1 with default features disabled. Release packaging verifies the backend identity, source hashes and lockfile audit before copying the helper. The helper is distributed on macOS/Linux, where RDX enforces resident-memory limits.
