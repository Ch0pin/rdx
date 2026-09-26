# G01-C-posttest-loop validation

Date: 2026-09-26. Gaps M01, M07, M08, M09, M06. Complete for the bounded scope.

## Scope

One canonical posttest loop with a conditional latch and sole fallthrough exit.
Prefix, body and exit tail are straight-line; there is no other condition/goto.
The body executes before its first condition check. Shared decoded operands,
loop membership and dominance must govern the selected route. Additional exits,
nesting and unsupported shapes retain established handling.

Pinned JADX revision `28ff15e4ae69950aebea110a13e5ab895d234dfc`: the
LoopRegionMaker condition-at-end path is the algorithm reference.

## Verification

The focused public gate passed 15 fixtures and 10 malformed cases. An independent
JVM oracle checked **723 method outcomes**, including **384 effect/failure
scenarios**: initially false conditions, signed boundaries, reference identity,
wide/reference swaps, body-only liveouts, raw float bits and exact exception
identity/trace prefixes. All eleven combined JVM checks passed (34 tests,
zero failures). Sol authored implementation/tests; Astra independently reviewed
the production changes, private fixtures, oracle and actual APK gains.

All 72 shared private tests passed, including three new tests and twelve
metadata mutations. Poisoned raw words/cached targets preserve selected output,
register frames and links; malformed boundary and budget cases reject.

The initial full suite passed every integration/doc-test target but failed one
old library text assertion expecting a retained boolean temporary and one slot.
The generated code was correct: cleanup inlined the boolean and introduced a
separate exit slot. The reviewed replacement proves condition-before-both-copies
and the returned exit slot. The entire library rerun passed **249 tests**, with
four ignored. Combining the unchanged integration targets and refreshed library:
**872 passed, zero remaining failures, 95 ignored, 95 targets**. The JSON retains
the initial full-run exit code and rerun details; this was not one pristine run.

Strict Clippy (all targets/features, warnings denied), formatting and release
build passed. The release binary is byte-identical to the corpus-comparison
build. The idle 300-class search sample returned the same 53 matches and
navigation data in three alternating pairs. Cold median: 0.235437 → 0.233103
seconds; warm median: 0.001244 → 0.001196 seconds. These are local samples,
not a general speed guarantee.

## Pinned APK comparison

Play Store reconstruction increases **260,228 → 260,236 / 268,289**; fallbacks
fall **8,061 → 8,053**, entirely in the undefined-register category (173 → 165).
The eight gains are `aztj.V`, `bpqd.a`, `cgtv.dg`, `chfh.e`,
`DesugarAtomicInteger.updateAndGet`, and the three AtomicReference helpers
`accumulateAndGet`, `getAndUpdate`, and `updateAndGet`. Exact descriptors are
recorded in the companion JSON.

Independent static DEX/source review found no discrepancy in mandatory iterator
access, retry read/operator/call order, recurrence arithmetic or old/new reference
return identity. The paired six-class diffs change exactly those methods plus
imports. These actual APK methods were not independently compiled or executed;
this is not a concurrency or whole-corpus equivalence proof.

## Boundaries

Native Rust implementation; pinned JADX is an algorithm reference only.
No full visitor or delivery unit closes from this bounded increment. Corpus
reconstruction does not measure universal coverage or semantic accuracy.
No release deployment, commit or push.

## Next increment

**G01-C-posttest-body-composition** adds bounded forward diamonds inside one
posttest loop while retaining the sole exit and complete cyclic proofs.
Additional exits/continues and outer/nested compositions remain deferred.
