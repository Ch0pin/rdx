# Native search measurements

The former Java-worker search implementation was removed. Its benchmark numbers
must not be applied to the new native engine: the native corpus contains every
DEX class, and its output mixes disassembly with the Java methods supported by
the current native renderer. It is not the same corpus as full JADX Java output.

`cargo build --release --bins` builds the native application and sample plugin.
`target/release/rdx --benchmark-search-current FILE QUERY 200000` measures project
load separately from cold and repeat native representation searches. It verifies
cold/warm result tuples and reports skipped/limited coverage. Package exclusions
are disabled in this benchmark.

`scripts/benchmark_memory.py OUTPUT.json COMMAND ...` is development-only Python
tooling, not an application dependency. It samples total process-tree RSS every
200 ms. RSS is not macOS Activity Monitor's memory footprint; do not compare the
metrics as if they were identical or infer Java-source parity from native timings.

The new engine is in-process Rust; there is no JVM memory component. Matching
and index handling remain Rust. Current source-generation capability limits are
listed in [native-engine.md](native-engine.md).

## Initial native-only migration measurement (2026-09-20)

`native-zoom-search.json` under `target/validation` records an all-package absent
literal scan of 97,592 DEX classes (including nested definitions): load 1.696 s,
cold scan 8.434 s, repeat 1.514 s, zero errors/skips/limits. Peak sampled process-tree
RSS was 800.5 MiB; no Java process is launched. The repeat scan rendered 20,652
sources again because the bounded index did not retain the entire larger native
representation corpus. These are single local CLI measurements, not GUI footprint
or reconstructed-Java performance. The former JADX source-owner corpus had fewer
entries and different content, so a speedup ratio would be misleading.

## Expanded native renderer measurement (2026-09-20)

The loop/switch/array/constructor revision searched all 97,592
classes with an absent literal, no errors/skips or result truncation. Load:
1.571 s; cold: 15.286 s; repeat:
2.321 s. The repeat still rendered
15,862 sources because the index is bounded.

The OS-reported child-process maximum RSS was 860.8 MiB. This
uses macOS getrusage maximum RSS, not the earlier polling sampler or Activity
Monitor's memory footprint. It is a single CLI run, not a GUI memory claim or
a JADX comparison. Corpus coverage is mixed Java/DEX. Evidence:
`target/validation/native-completion-search.json` and its `.memory.json` companion.

## Native metadata/numeric/exception expansion (2026-09-20)

With 548,948 reconstructed concrete methods (77.75% coverage), the full
97,592-class absent-text workload completes in 17.093 seconds cold and
2.607 seconds repeated; load is 1.758 seconds. There are zero errors or skips
and exact cold/repeated result parity. Child peak RSS is 941.625 MiB, measured
through macOS `getrusage`, not Activity Monitor footprint.

Evidence: `target/validation/native-accuracy-search.json` and
`target/validation/native-accuracy-search-memory.json`. This searches mixed
Java/DEX and emits more Java than the preceding revision; it establishes no
speed or memory advantage over JADX's complete output.
