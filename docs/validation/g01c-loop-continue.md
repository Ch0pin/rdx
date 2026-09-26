# G01-C-loop-continue validation

Date: 2026-09-26. Gaps M01, M07, M08, M09, M06. Complete for the bounded conditional-backedge scope.

## Production scope

One conditional body edge may return to the same header alongside one
unconditional latch, forward body diamonds and at most one common-exit break.
Canonical metadata identifies the conditional continue separately from the latch.
Both sources must be dominated by the same header. Header values synchronize
with snapshot semantics before emitting continue. Root validation recomputes
metadata; inconsistent selected data cannot retry raw operands.

The planning copy removes the taken continue edge but retains its fallthrough.
The complete cyclic CFG remains authoritative for dominance and liveness.

The pinned JADX BlockProcessor.markLoops classifies a successor dominating its
predecessor as a loop header. RDX extends its bounded adaptation of this rule and
DominatorTree; it does not port all block transformations. LoopRegionMaker's
synthetic-block continue insertion was inspected as a comparison only, not
implemented wholesale. Pinned revision:
`28ff15e4ae69950aebea110a13e5ab895d234dfc`.

## Validation

Four new private tests passed; all 54 shared private tests passed. They cover
simple continue, nested combined break/continue and fallthrough at the latch
under full raw instruction/target poisoning, with identical text, frame and
nonempty navigation links. Eight baseline-backed metadata/operand/edge mutations
reject. Isolated liveness demonstrates a header-read value becomes live at the
continue edge and loses that contribution when only the taken edge is removed.
Restored literals remain valid only when restoration precedes every backedge;
the before-restoration continue counterexample invalidates the proof and rejects
rendering. Private structural fixtures are not runtime termination evidence.

Ten public finite fixtures and nine malformed cases passed. The independent JVM
oracle checked **45,150 outcomes**, including **43,218 effect/failure scenarios**
and 42 additional header trace checks. Coverage includes zero/normal iterations,
skipped effects and second swaps, wide/reference identity, nested selection,
break-before-continue and continue-before-break priority, header reexecution,
null/raw-float exits and exact exception identity.

The combined six-target gate passed all **19 tests**, including six explicitly
enabled JVM checks. Sol implemented production and fixtures; Astra independently
reviewed production, private tests and public oracle. Parent inspected integration
and corpus behavior. The full default suite passed **844 tests**, zero failures, with 90 optional
tests ignored. The six targeted JVM checks were enabled separately. Strict
Clippy (all targets, warnings denied), formatting and the final locked release
build passed. Search results follow below.

## Corpus and performance

Final release build passed. The final pinned Play Store report is byte-identical
to the previous report: **260,225 / 268,289** reconstructed concrete methods,
**8,064 fallbacks**, including identical reasons and samples. This is one APK's
reconstruction rate, not universal coverage or semantic accuracy. APK SHA-256:
`c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256:
`54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.
Baseline binary: `target/validation/rdx-before-g01c-loop-continue`.
Binary/source/test hashes are recorded in the companion JSON.
Three alternating before/after search pairs (`getClass`, 300 classes) returned
identical 53 matches and corpus metadata, with no errors, limits or skipped items.
Cold medians: 0.220620s before / 0.220304s after; warm medians: 0.001185s /
0.001179s. This is a local sample, not a universal performance guarantee.
Existing navigation regression tests passed; no GUI smoke test was run.
`git diff --check` passed.


## Boundaries

Additional continues, alternate headers, terminal arms, nested loops and multiple
breaks remain on existing paths. Allocation, exception, monitor and switch routes
retain existing selection. General SSA/type-driven emission remains open. No
whole visitor or delivery unit closes. No release, GUI deployment, commit or push.

## Next increment

G01-C-loop-edge-composition replaces the single optional edge caches with a
bounded canonical collection of conditional breaks/continues targeting this same
header/common exit. Acceptance covers sequential/nested multiple edges, skipped
effects, wide/reference snapshots, all-edge liveness/invariant proofs, and
duplicate/cross-region metadata rejection. One unconditional latch remains.
Distinct headers/exits, unconditional body exits and terminal arms stay outside
the increment. This removes edge-count restrictions before nested-loop ownership.
