# G01-A: shared instruction boundaries and invocation binding

**Status: G01-A complete for the scope below. G01 and all full visitor IDs remain open.**

[Machine-readable evidence](g01a-shared-front-end.json).

## Scope

Tracked inventory IDs: M01 (instruction validation), M05 (method details), M06
(instruction preparation). This is an integration increment, not a new coverage
percentage target or a complete port of any JADX visitor.

`native_method::MethodFrontEnd` factors decoded IR and signature-bound calls out
of `MethodAnalysis`. Eligible Java rendering consumes its instruction offsets,
opcodes and widths, invoke register reads, receiver/argument bindings and result
ownership. Full constructor/SSA analysis uses the same front end. CFG construction,
SSA and inferred types are deliberately not made mandatory on this rendering hot
path; they remain subsequent inventory work.

Eligibility scans instruction boundaries, never operand bytes. Methods with
handlers, jumps, switches, monitor operations, new-instance, filled-new-array or
array payloads use the existing renderer path. Ordinary new-array remains eligible.
Once a method selects the shared path, decode/binding failures must reject Java
reconstruction rather than silently retry the old decoder.

Non-invocation operand lowering still reads raw words. The renderer still uses
mutable register values. Neither general decoded-operand emission nor shared CFG,
SSA-to-region or typed-expression emission is claimed here.

## Verification

Internal tests establish production-path eligibility, shared-layout/binding
consumption and failure when a shared binding is absent. Public renderer tests in
`tests/native_straight_line_pipeline.rs` cover ordinary and range invokes, wide
argument/result reuse, exact method navigation spans, invalid wide pairs,
incompatible result kinds and detached move-result instructions. The opt-in JVM
fixture executes 40 cases covering wide results, ordered effects and exception
identity.

The before binary was preserved. Both before and after full pinned Play Store
coverage reports are identical, including diagnostic categories and samples:
260,225 reconstructed, 8,064 fallbacks, 268,289 concrete methods. The APK SHA-256
is `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.

Three alternating before/after search runs, after compilation and regression
execution finished, covered the same 300 sampled classes and query `getClass`.
All 53 matches and source coordinates were identical, without skipped documents,
errors or result limits. Median cold search/render time was
0.229502 s before and 0.236856 s after;
warm medians were 0.001229 s and 0.001240 s.
These are local sample timings, not a general performance claim. Aggregate timings
and hashes are retained in the JSON evidence; raw runs are in ignored
`target/validation/g01a-search-final-*` files.

Final validation: **765 default tests passed, 0 failed, 77 optional tests ignored**.
Both selected JVM tests passed (the new fixture's 40 executions plus existing
quiet-NaN raw-bit checks). Strict Clippy, formatting, diff checks and the release
binary build passed. No GUI packaging or release deployment was performed.

The initial suite caught return/throw tail rejection omitted by the new route.
Review also caught decoder-supported but emitter-unsupported opcodes potentially
reaching an `unreachable!` branch. Both were fixed with negative regressions before
the final full-suite run. Underlying decoding/binding diagnostics remain visible.

## Remaining G01 work

Use the inventory to extend shared decoded operand consumption and then canonical
CFG integration for branches/handlers. Preserve the supported production path and
repeat the acceptance gates for each bounded increment. Do not close G01 based on
this straight-line integration alone.
