# G01-C-loop-break validation

Date: 2026-09-26. Gaps: M01, M07, M08, M09, M06. Complete for the bounded common-exit scope.

## Production change

One conditional body edge may now reach the existing common loop exit while
retaining the single unconditional latch and forward body diamonds. Canonical
`SharedLoopExit` metadata distinguishes the body break from the header guard.
Root validation recomputes the exit metadata alongside loop membership and body
plans. The break reads decoded condition operands and synchronizes canonical
exit slots. Missing or inconsistent selected metadata rejects reconstruction;
it cannot retry raw branch operands.

The complete cyclic CFG keeps the break edge for dominance and fixed-point
liveness. The body-planning copy removes only its taken exit edge, retaining its
actual fallthrough (including fallthrough directly to the latch). The break is
excluded from ordinary diamond plans. Canonical metadata also determines whether
the renderer needs a separate exit frame.

This extends the bounded adaptations of pinned JADX
[BlockProcessor.markLoops](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockProcessor.java)
and [DominatorTree](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/DominatorTree.java).
It does not establish full visitor parity or general loop restructuring.

## Restored-literal exit correction

The restored-float public fixture initially failed with an integer-to-float
conversion error. A private comparison using the normal legacy graph and liveness
reproduced that rejection. Selected loops now reuse the refined invariant write
mask for exit slots: that proof checks the entry literal at every backedge and
every escape, including the break. Legacy synchronization retains its original
mask. This is a narrowly newly accepted pattern, not solely a source-parity
migration. A break-before-restoration counterexample both invalidates the proof
and rejects rendering; the positive survives raw operand poisoning and JVM checks.

## Verification

Four new private tests cover simple and nested breaks, fallthrough at the latch,
raw instruction/legacy target poisoning with exact text, state and nonempty links,
eight baseline-backed canonical cache/edge/operand mutations, and excluded routes.
An isolated liveness test proves the header-defined value is live at the break
because of the exit edge: removing only that edge clears the recomputed bit.
All fifty shared private tests passed.

Eight public fixtures and eight negatives passed. The independent JVM oracle
checks **9,600 outcomes**, including **8,064 effect/failure scenarios** and 48
additional header-trace checks. It covers zero/early/normal exits, writes before
and after a break, final header values, wide/reference swaps before breaking,
nested selection, null/raw-float exits and exact exception identity.

Sol implemented production and public fixtures; Astra independently reviewed
production, private tests and public oracle. Parent inspected integration and
corpus behavior. One combined test process was terminated by signal 9 before
producing results; no assertion failure was reported and the cause remains
unconfirmed. The unchanged bounded-job retry passed all 16 tests across five targets,
including five JVM tests. Clippy (all targets, warnings denied), formatting and
the final locked release build passed. The full default suite passed **838 tests**, zero failures, with 89 optional
tests ignored; it included the new break target. The five targeted JVM tests
were explicitly enabled in the separate 16-test combined gate.

## Corpus and search

The frozen production report matches the previous report exactly: 260,225
reconstructed / 268,289 concrete methods, 8,064 fallbacks, including all reasons
and samples. This is one APK's reconstruction rate, not universal coverage or
semantic accuracy. APK SHA-256:
`c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.

Baseline: `target/validation/rdx-before-g01c-loop-break` and
`target/validation/g01c-loop-body-composition-coverage-final.json`.
Three alternating before/after search pairs (`getClass`, 300 classes) returned
identical 53 matches and corpus metadata with no errors, limits or skipped items.
Cold medians: 0.237629s before / 0.238488s after; warm medians: 0.001304s /
0.001285s. This local sample does not establish universal performance. Existing
navigation regressions passed in the full suite; no GUI smoke test was run.
Final report is byte-identical to baseline. Report SHA-256:
`54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.
Binary/source/test hashes and timing data are recorded in the companion JSON.
`git diff --check` passed.

## Boundaries and next increment

Continue, body terminal arms, nested loops, multiple break edges and distinct
exit destinations remain on existing paths. Constructor/allocation, protected,
monitor and switch/payload paths retain their existing routing. Raw eligibility
and NOP validation remain. General SSA/type-driven emission and full upstream
block transformations are open; no whole visitor or delivery unit closes.

Next: **G01-C-loop-continue** — one conditional body edge to the same header,
retaining the unconditional latch, body diamonds and at most the existing common
exit break. Canonical metadata must distinguish this second dominated backedge
and synchronize header slots with snapshot semantics. Additional continues,
alternate headers, terminal arms and nested loops remain outside that increment.
No release, GUI deployment, commit or push.
