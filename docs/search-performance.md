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
