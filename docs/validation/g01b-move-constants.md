# G01-B-start: shared move and numeric constant operands

**Completed 2026-09-25 for the bounded scope. G01-B and G01 remain open.**
Tracked inventory gaps: M01 (input validation), M06 (instruction normalization).
This extends the shared front end from [G01-A](g01a-shared-front-end.md).
[Machine-readable evidence](g01b-move-constants.json) pins hashes and measurements.

## Production integration

`src/native_java/method.rs::decoded_move` and `decoded_constant` consume shared
read/write registers and literals for all nine move encodings (0x01..0x09) and
eight numeric constant encodings (0x12..0x19). The selected shared route does not
retry raw operands on error. Operand counts, kinds, register widths and literal
presence/range are checked. Source values are captured before assignment, preserving
overlapping wide moves and overwrites. Signed and high16 literals preserve their
32/64-bit interpretation, including floating-point negative zero and NaN payloads.
The eligibility boundary and all legacy rendering branches remain unchanged.

This is integration of RDX's existing decoded IR, not a claim to port the complete
upstream CheckCode or ProcessInstructionsVisitor. Their pinned source mappings
and remaining acceptance criteria stay in the canonical inventory.

## Validation

- Full default suite: **769 passed, 0 failed, 78 ignored**. Ignored tests are not counted as passed.
- New optional JVM test: **passed, 80 behavioral assertions** across 14 move cases
  and 12 numeric constant cases. Checks include object identity, signed extrema,
  high registers, overwrite snapshots, wide overlap, negative zero and quiet NaN bits.
- Private production tests poison raw operands after shared decoding to prove
  consumption of decoded operands; nine malformed shared mutations reject.
- Independent code review, strict all-target Clippy, formatting, diff checks and
  release binary build passed. Commands and hashes are in the JSON record.
- Pinned Play Store corpus: **260,225/268,289 methods reconstructed (96.994286%)**,
  8,064 fallbacks. Entire diagnostic report equals the G01-A baseline, including samples.

## Search performance and navigation

Three alternating before/after runs searched `getClass` across the same 300 classes.
All 53 matches and their source coordinates matched exactly; no errors, skipped
classes or limits. Cold median: **230.245 ms before → 232.557 ms after (+1.0%)**.
Warm median: **1.253 ms → 1.246 ms**. This small local sample does not establish
universal performance or accuracy.

## Remaining work and exact resume point

**Resume G01-B-reference-constants:** migrate const-string, const-string/jumbo and
const-class through shared operands and pool references; preserve escaping,
class-resolution effects and navigation. Follow with result/return and other
operand families in separately validated increments. Shared CFG for branches and
handlers, SSA and type-driven emission remain open. No visitor or G01 unit closes
from this increment, and no GUI deployment or release was performed.
