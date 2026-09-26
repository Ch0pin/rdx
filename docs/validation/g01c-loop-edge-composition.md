# G01-C-loop-edge-composition validation

Date: 2026-09-26. Gaps M01, M07, M08, M09, M06. Complete for the bounded same-loop edge scope.

## Production scope

Generalize the single optional conditional break/continue caches into a bounded
canonical collection targeting the same loop header/common exit. One unconditional
latch and forward body diamonds remain. Full cyclic CFG dominance, liveness and
restored-literal proofs must include every edge. The planning projection removes
only taken special edges, preserving actual fallthrough and snapshot carried-value
synchronization. Root revalidation rejects inconsistent selected metadata without
retrying raw operands.

This extends the bounded adaptation of pinned JADX BlockProcessor.markLoops and
DominatorTree, revision `28ff15e4ae69950aebea110a13e5ab895d234dfc`.
It does not establish full visitor or nested-loop parity.

Edges are ordered by instruction offset and classified explicitly as break or
continue. The existing 1,024 total-control-instruction limit also applies when
canonical metadata is rederived. Rendering and edge-kind/ordinary-branch exclusion
lookups use binary search. Review caught a full-IR scan per backedge in latch
classification; this was replaced with binary-search terminator lookup before
production freeze, avoiding that scaling cost when many continues are present.

## Verification

Five new private tests passed; all 59 shared private tests passed. Sequential
and nested two-break/two-continue fixtures survive full raw-instruction/target
poisoning with identical text, register frame and nonempty navigation links.
Nine baseline-backed mutations cover duplicate, reordered, missing, cross-region,
kind, target, fallthrough, operand and CFG metadata corruption.

Each of four taken edges independently contributes a live bit: removing only
that edge clears the recomputed contribution. Each edge also participates in
restored-literal validation: overwriting before the selected edge and restoring
after it invalidates the proof and rejects rendering. The control-budget fixture
accepts exactly 1,024 total controls (1,022 special edges plus guard and latch)
and rejects 1,025 through both canonical derivation and raw selected routing.
The boundary fixture proves routing/budget behavior, not JVM semantics.

Eight finite public fixtures and fourteen malformed cases passed. An independent
JVM oracle checked **229,120 outcomes**, including **188,160 effect/failure
scenarios** and 1,280 additional header trace checks. Independent limits/four-edge
predicates and three nested modes exercise competing-edge priority, skipped
updates, wide/reference snapshots, null/raw-float exits, header reexecution and
exact exception identity. The seven-target gate passed all **22 tests**, including
seven explicitly enabled JVM checks. Sol implemented production and fixtures;
Astra independently reviewed production/private/public oracle; parent reviewed
the source diff and oracle. The full default suite passed **851 tests**, zero failures, with 91 optional
tests ignored. The seven JVM checks were enabled separately. Strict Clippy
(all targets, warnings denied), formatting and the final release build passed.
Search results follow below.

## Corpus and performance

The final locked release build passed. The pinned Play Store report is byte-identical
to baseline: **260,225 / 268,289** reconstructed concrete methods, **8,064 fallbacks**,
including identical reasons and samples. This is one APK's reconstruction rate,
not universal coverage or semantic accuracy. APK SHA-256:
`c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256:
`54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.
Baseline: `target/validation/rdx-before-g01c-loop-edge-composition` and
`target/validation/g01c-loop-continue-coverage-final.json`.
Source/test/binary hashes are recorded in the companion JSON.
Three alternating before/after search pairs (`getClass`, 300 classes) returned
identical 53 matches and corpus metadata, with no errors, limits or skipped items.
Cold medians: 0.219604s before / 0.220922s after; warm medians: 0.001182s /
0.001187s. This is a local sample, not a universal performance guarantee.
Existing navigation regressions passed; no GUI smoke test was run.
`git diff --check` passed.

## Boundaries

Distinct headers/exits, unconditional body exits, terminal arms and nested loops
remain outside selected scope. Allocation, exception, monitor and switch routes
retain existing selection. General SSA/type-driven emission remains open.
No whole visitor or delivery unit closes. No release, GUI deployment, commit or push.

## Next increment

G01-C-loop-outer-composition: existing acyclic forward regions before/after or
around one canonical loop, including paths bypassing it. Treat that loop as an
atomic region for outer planning while retaining full cyclic liveness/dominance
and its current body plans. Reject external entries to body/latch. Test skipped/
taken loop joins, wide/reference exit values, effects and exceptions. Multiple
or nested loops remain outside the increment. This establishes region ownership
before nested-loop integration.
