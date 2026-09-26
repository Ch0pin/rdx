# G01-B-reference-constants

**Completed 2026-09-25 for bounded scope; G01-B and G01 remain open.**
Tracked gaps: M01 (input validation), M06 (instruction normalization).
[Machine-readable evidence](g01b-reference-constants.json) records hashes, test
results, corpus identity and raw timing samples.

## Production integration

`src/native_java/method.rs::decoded_reference_constant` now supplies destination
register and pool index for const-string, const-string/jumbo and const-class in
the eligible shared straight-line route. Checks reject missing/wrong pool kinds,
invalid operands and register widths, unexpected literals, lost may-throw metadata
and compact indices beyond 16 bits. Invalid shared inputs do not retry raw decoding.
String escaping still uses the existing emitter. Class constants still use
`operations::emit`, preserving local materialization at the instruction position
and type navigation links. Allocation-specific and other legacy paths are unchanged.
This integrates the existing attributed RDX decoded IR; it does not complete
JADX CheckCode or ProcessInstructionsVisitor.

Implementation and integration tests were delegated to separate Sol agents;
Astra reviewed independently. The parent inspected the changes and ran full gates.

## Validation

- Full default suite: **775 passed, 0 failed, 79 ignored**. Optional tests are not
  counted as passed. The new optional JVM test separately passed six behavioral checks.
- Private tests poison raw operands after decoding for all three encodings and
  reject 27 malformed shared cases. Four public tests check actual jumbo index
  65,536, destination v3, escaping/Unicode/surrogates, class links, overwritten
  references and retained class-literal ordering before a subsequent call.
- JVM checks verify string UTF-16 units, overwrite behavior, class identity,
  non-initialization, the subsequent call and later one-time initialization.
- Independent review, strict all-target Clippy, formatting, diff checks and release
  binary build passed. No GUI deployment or release was performed.
- Pinned Play Store corpus: **260,225/268,289 methods reconstructed (96.994286%)**,
  8,064 fallbacks. The entire report matches the previous G01-B-start baseline.

## Search comparison

Three alternating runs per binary, `getClass`, same 300 classes. All 53 matches
and source coordinates matched; no errors, skips or limits. Cold median:
**224.096 ms before → 228.876 ms after (+2.1%)**. Warm median:
**1.224 ms → 1.201 ms**. These local timings do not establish universal performance.

## Boundaries and exact resume point

String constants retain existing inline emission. This migration does not establish
complete fidelity for string-resolution timing. Class-literal source order is
preserved, but missing-class exception timing was not tested; the JVM ordered-call
case executes after its target class has already been resolved. It proves identity,
non-initialization and the call effect, not first-resolution failure order.

**Next: G01-B-results-returns.** Migrate move-result, move-result-wide,
move-result-object and return-family operands through shared decoded IR, retaining
pending-result ownership, type/width checks and constructor initialization guards.
Move-exception/handler integration and throw operands remain outside that scope.
Other operand families, shared CFG and SSA/type-driven emission remain open.
