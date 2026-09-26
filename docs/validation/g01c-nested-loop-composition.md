# G01-C-nested-loop-composition validation

Date: 2026-09-26. Gaps M01, M07, M08, M09, M06. Complete for bounded nested-loop composition.

## Scope

Bounded parent-owned canonical nested pretest loops. Inner loops act as atomic
regions during enclosing-body planning, while original cyclic CFG remains
authoritative for dominance and liveness. Validate parent and innermost edge
ownership, including ownership after a child exits. Scope is limited to four
nested levels, 32 total loops and 1,024 controls. Deeper nesting retains legacy
handling. Body planning shares a work budget across depth projections.

Pinned JADX revision `28ff15e4ae69950aebea110a13e5ab895d234dfc`,
BlockProcessor.markLoops/registerLoops/processNestedLoops and DominatorTree are
algorithm references. Runtime remains native Rust. This is not full visitor parity.

## Verification

Fifteen public fixtures and twelve malformed cases passed the focused gate.
An independent JVM oracle covers **198,801 outcomes**, including **138,240
effect/failure scenarios** and a separate 81-state depth-four matrix. Cases
include zero/multiple inner iterations, sibling children, skipped children,
child exit at parent latch, child changes to parent counters, owner-local
breaks, wide/reference/null/raw-float values and exact exception prefixes.

Sol implemented production and the independent fixture suite; Astra reviewed
the source/oracles. Parent inspected parent assignment, ownership recovery and
depth projections. All ten combined JVM checks passed (31 tests, zero failures).
All 69 shared private tests passed, including five new nested-loop tests. The
final frozen full suite passed **867 tests, zero failures, 94 ignored** across
94 targets. The selected JVM gate ran separately with ignored checks included.
Strict Clippy (all targets/features, warnings denied), formatting and release
build passed. The final binary is byte-identical to the corpus comparison build.

The 300-class search sample returned the same 53 matches and navigation data
in all three alternating baseline/current runs. Cold median: 0.224074 →
0.224130 seconds; warm median: 0.001235 → 0.001230 seconds. These idle local
samples show no material change, not a general performance guarantee.

An initial full run captured an unfinished private fixture; its save/restore
layout was corrected before the final passing run. A duplicate test-helper
branch was simplified for Clippy without altering fixture bytecode.

## APK comparison

The pinned Play Store corpus remains **260,228 / 268,289 reconstructed** with
**8,061 fallbacks**. This net-zero result has two changes:

- `bnuk.Z(I[DII)V` now reconstructs. Static DEX/source review found no discrepancy
  in nested scan order, swaps, bound updates or NaN comparison behavior. The
  actual APK method was not independently compiled/executed.
- `kkd.c(JIII)V` now rejects with `loop changes retained literal`. Its previous
  Java was invalid: it assigned values to literal expressions including `50`,
  `33554431`, `25` and `-1125899873288193L`. Conservative fallback removes invalid
  output; a separate literal/materialization increment can address this later.

## Boundaries

Cross-owner/labeled exits, alternate exits, intersecting/irreducible regions and
terminal loop bodies remain outside selected scope. Malformed selected metadata
cannot retry raw operands. No whole visitor or delivery unit closes from this
increment. Corpus reconstruction is not universal coverage or semantic-equivalence
proof. No release, deployment, commit or push.

## Next increment

**G01-C-posttest-loop**: one canonical conditional-latch loop with a sole
fallthrough exit. Verify mandatory first iteration and ordered effects before
adding extra exits or nesting. The canonical inventory retains the exact scope.
