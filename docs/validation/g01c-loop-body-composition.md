# G01-C-loop-body-composition validation

Date: 2026-09-26. Gaps: M01, M07, M08, M09, M06. Complete for the bounded scope; G01 and whole visitor positions remain open.

## Production change

Existing sequential/nested forward conditional regions inside one selected
natural loop now consume canonical branch plans. The loop keeps one header
guard, one unconditional latch and its sole guard exit. Body branches may join
at the latch, including an empty conditional where taken and fallthrough both
reach it. Prefix/header/tail conditions, branches to the header/exit, terminal
body arms, nested loops and multiple latches retain legacy routing.

The full cyclic CFG remains authoritative for dominance, natural-loop membership
and fixed-point liveness. A temporary planning copy removes only the canonical
latch backedge. Forward postdominators and region closure compute body joins;
each resulting plan must stay within the body through the latch. The header
guard is excluded from body plans. Root validation checks cached plans against
decoded data along with the CFG and loop metadata. Missing/corrupt selected plans
reject reconstruction rather than retrying raw branch operands.

This composes the prior bounded adaptations of pinned JADX
[BlockProcessor.markLoops](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockProcessor.java)
and [DominatorTree](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/DominatorTree.java).
It is not full visitor parity or general loop-region reconstruction. Existing
32 MiB/16 million liveness limits and conservative retain-all behavior remain.

## Review and private tests

Sol implemented production and private tests; Astra independently reviewed the
code and tests. Parent inspected integration and corpus behavior. Four new
private tests establish selection, exact plan counts/joins, nonempty navigation
links and final frame equality under poisoned raw words and legacy targets.
They cover a body diamond, empty condition at the latch, sequential and nested
diamonds including a forward skip-arm goto, and restored float literal merging.
Six canonical plan/edge/operand corruptions reject against a successful baseline.
The float private test proves static merge/operand ownership, not execution of
both runtime arms.

Review found that initially requiring fallthrough strictly before the latch
rejected the valid empty-condition boundary. The inclusive boundary and a
selected-path poisoning regression now cover it. A loop-only terminal scan avoids
allocating a terminal list for every straight-line method.

## Public semantic and regression gates

The new public target passed three tests, including its optional JVM test: eleven
positive fixtures and eight rejection cases. The independent oracle checks
**3,006 outcomes**, including **2,016 effect/failure scenarios** across six loop
limits and three selector modes. It verifies wide arithmetic/identity swaps,
reference identity, dead incompatible versus live merged values, latch joins,
header-defined exits, both restored-float arms and exact exception identity.

Public source/oracle review passed. The combined loop-composition, natural-loop,
header-liveout and numeric-invariant run passed thirteen tests, including four
optional JVM tests. Strict all-target Clippy, formatting, whitespace checks and
the final release build passed. Existing full regression: **830 passed, zero failed, 87 ignored**. This command
started on frozen production before Cargo discovered the new integration target;
that target passed separately with its JVM test. The thirteen-test focused run
overlaps existing tests; its count is not additive to the default-suite count.

## Corpus and search

The frozen production report matches the baseline exactly: 260,225 reconstructed
/ 268,289 concrete methods, 8,064 fallbacks, including identical reason counts
and samples. This is one APK's reconstruction rate, not universal coverage or
semantic accuracy. APK SHA-256:
`c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.

The baseline binary is `target/validation/rdx-before-g01c-loop-body-composition`.
Exact search results and navigation coordinates match in three alternating pairs
over 300 classes (53 matches, no errors, skips or limits). Final cold median: 231.205 ms before to 227.805 ms after; warm median:
1.280 ms to 1.245 ms. This small local sample is not a universal performance claim.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.
[Machine-readable evidence and source/binary hashes](g01c-loop-body-composition.json).

## Boundaries and next increment

Raw eligibility scans and NOP validation remain. Excluded control-flow,
constructor/allocation, switch/payload and handler/monitor paths retain their
existing routing. General SSA/type-driven emission and complete upstream block
transformations remain open. G01 and full visitor positions are not closed.

Next: **G01-C-loop-break** — one conditional body edge to the existing common
exit, retaining the single unconditional latch and current body diamonds.
Canonical exit-edge metadata must govern break condition and exit-value
synchronization. Continue, terminal arms, nested loops and distinct exit
destinations remain excluded. No release, GUI deployment, commit or push.
