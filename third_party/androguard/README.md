# Optional Androguard component attribution

The separate optional helper includes Rust sources from:

| Repository | Exact revision | License |
| --- | --- | --- |
| https://github.com/androguard/dex-decompiler | f7fa4a4853d957ebc3c54355d128a3835ef5ae22 | Apache-2.0 |
| https://github.com/androguard/dex-bytecode | 7d40b753480131bff4d5c1786b5f698b630f2bac | Apache-2.0 (repository LICENSE) |
| https://github.com/androguard/dex-parser | 30bde726dd30a0d8c442ea7006666da2ef72b9bd | Apache-2.0 |
| https://github.com/androguard/apk-parser | 95ec9b00b4e2e30d16ac98c09868e5d6b58d3fdb | Apache-2.0 (repository LICENSE) |

Vendored source is in `/Users/ch0pin/Desktop/Git-Projects/rdx/tools/androguard-helper/vendor`. Each repository license is preserved there; the Apache license is also included here. Upstream authors and source-file copyright notices remain intact. No radguard code or dependency is used.

The upstream ASC fast path adapts algorithms by **WeiMin Cheng (MG1937)** and **Zhihan Lin (0chencc)**, *Droid ASC: R8 Compiler Optimization as a DeCompiler Primitive*, https://github.com/MG1937/ASC (Apache-2.0). Preserve this acknowledgement when redistributing the optional helper.

RDX modifications (2026-11-14): `dex-decompiler/src/decompile/mod.rs::shorten_java_names` preserves qualified names for independent method insertion; exact change is recorded in `name-preservation.patch`. Cargo manifests point to pinned vendored sibling sources and disable unused APK signing/CLI/AXML optional features. RDX's separate helper adapter emits raw descriptor IDs and explicit per-method errors. Upstream parsing and decompilation algorithms otherwise remain unchanged.

`dependency-audit.json` records registry versions, checksums, declared licenses and preserved license/notice files. Corresponding license texts are under `dependencies`. The `colored` dependency uses MPL-2.0; its unmodified corresponding source is supplied under `mpl-source`, with upstream availability at https://crates.io/crates/colored. MPL applies to its covered files; the optional helper and RDX retain their own licenses. This inventory covers the optional helper separately from RDX's existing dependency notices.

The locked dependency inventory was also checked for macOS x86_64 and Linux x86_64 on 2026-11-14. Both resolve the same 100 registry package versions as macOS arm64; the preserved notices cover these release targets. Windows helper distribution remains disabled.
