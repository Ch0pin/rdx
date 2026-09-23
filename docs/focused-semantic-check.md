# Focused semantic check — 2026-09-21

Result: no unexpected mismatch in the tested subset. One previously accepted
allocation-timing difference was reproduced. This is not an engine-wide semantic
accuracy percentage.

## Behavioral results

| Pattern | Scope | Result |
|---|---|---|
| Boolean XOR | 12 boolean truth-table cases plus 36 integer boundary cases, across both register opcode forms and operand orders | 48 exact matches |
| Catch boundary | Normal completion, failure inside protection, failure after protection, matching and nonmatching exception types | 5 exact matches |
| Constructor preparation | One/two arguments; normal completion, constructor failure, allocation failure; compares argument values, returns/errors and ordered traces | 6 exact matches |
| Super dispatch | Distinct parent/child implementation values and call identities | 1 exact match in a controlled dispatch model |
| Readable constructor staging | Normal completion and constructor failure, comparing traces after excluding the explicitly relocated allocation event | 2 matches under the stated allocation-timing allowance |
| Readable staging with allocation failure | Fail allocation before construction | Confirmed difference: generated source prepares arguments before allocation fails |

The allocation-failure fixture produces these traces:

- Original DEX: `new:sample.Box` → OutOfMemoryError.
- Generated source: `class:sample.First`, `class:sample.Second`, `new:sample.Box` → OutOfMemoryError.

The returned exception agrees, but the operations reached before failure differ.
This reproduces the tradeoff already accepted for readable staging; it must not
be counted as exact semantic equivalence.

Five deliberately incorrect outputs were detected: one widened catch, three
removed XOR negations, and one super-to-virtual dispatch replacement.

## Evidence and reproduction

- `cargo test --release focused_`: four test functions, containing the scenarios above.
- `cargo test --release --test native_exceptions --test native_allocation_captures --test native_allocation_lowering`: 51 tests passed. This overlaps two of the focused tests; counts must not be added as independent cases.
- `RDX_TEST_APK=/Users/ch0pin/Desktop/BugBounty/mango-db/com.android.vending.apk cargo test --release --test native_backup_accuracy -- --ignored`: five APK regression tests passed. These check reconstruction, retained calls, headers and metadata, not live Android execution.
- `cargo clippy --all-targets -- -D warnings`: passed.
- Logs: `target/validation/focused-semantics.log`, `semantic-regressions.log`, `semantic-apk-checks.log`, `semantic-clippy.log`.

## Scope and limits

The tests use small Rust evaluators for a deliberately limited DEX/generated-Java
subset. Unsupported syntax causes test failure. XOR uses the independent integer
operation/truth-table oracle; super dispatch uses controlled implementations.
Constructor/catch checks compare interpreted emitted source against the DEX
fixture, including constructor argument values and injected failures.

No JVM compiler, Android runtime or whole-APK behavioral execution was used.
Heap behavior, concurrency, arbitrary Android dependencies, all loop paths and
full-method compilation remain untested by this check. Production code was not
changed: this update adds tests and extends the test-only evaluators.
