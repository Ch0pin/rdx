# G01-C-disjoint-loop-composition validation

Date: 2026-09-26. Gaps M01, M07, M08, M09, M06. Complete for bounded disjoint-loop composition.

## Scope

Bounded nonoverlapping canonical pretest loops within existing forward regions.
Each region owns its header, latch, exit, body plans and conditional break/continue
edges. Original cyclic CFG remains authoritative for dominance and liveness;
body and atomic outer planning use projections. Maximum 32 disjoint loops and
1,024 total controls; nested/overlapping loops remain on existing handling.

Pinned JADX revision `28ff15e4ae69950aebea110a13e5ab895d234dfc`,
BlockProcessor.markLoops/registerLoops and DominatorTree are algorithm references.
This is bounded native Rust integration, not complete visitor parity.

## Verification

Fourteen finite public fixtures and twelve malformed cases passed. The independent
JVM oracle checks **131,328 outcomes** across 1,728 input states, including
**69,120 effect/failure scenarios** and **1,728 additional header traces**.
It covers sequential and alternative two/three-loop layouts, skipped loops,
break ownership, a first-loop value used solely by a later guard, and
wide/reference/null/raw-float values. The nine-target combined regression gate
passed **28 tests, zero failures or skips**, including nine JVM checks.

Sol implemented production and independent test fixtures; Astra reviewed both.
Parent inspected the region collection/projections and independently verified
combined-gate counts. All 64 shared private tests passed, including three new disjoint-loop tests.
Five layouts prove actual shared selection/rendering and raw instruction/target
poison independence, with nonempty links and frame equality. Adjacent loop
exit/header identity preserves a carried value read by the next header. Twelve
cache/order/owner and rebuilt-CFG mutations reject without raw retry. The nested
legacy fixture is validated as a CFG with two distinct backedges; a review caught
and corrected an earlier fixture that lacked the inner backedge. Loop counts
32/33 verify selected acceptance, raw legacy routing and canonical rejection.
All final regression/lint/build/search gates passed.

## Boundaries

Malformed selected metadata cannot retry raw operands. No whole visitor or
delivery unit closes from this increment. Corpus reconstruction is not universal
coverage or semantic-equivalence proof. No release, deployment, commit or push.

## Pinned corpus

The same Play Store APK (SHA-256
`c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`)
reconstructs **260,228 / 268,289** concrete methods, up from 260,227.
Fallbacks decrease **8,062 → 8,061**; only `goto crosses region boundary`
changes (96 → 95). All diagnostic reason samples are unchanged. The exact gain is `vvw.kI(Lbgtk;I)V`. Four canonical loops form alternative
growth/population pairs, with a size-one bypass. Independent static DEX/source
review found no discrepancy in branch selection, repeated reads, casts, updates
or shared tail ordering. Java duplicates one pair in mutually exclusive outer
arms (six textual loops); an execution path cannot run both copies.

This actual APK method was not independently compiled or executed. Runtime
evidence comes separately from synthetic JVM fixtures. Local audit, prefix scans
and before/after source are under `target/validation/g01c-disjoint-*`.

## Next increment

G01-C-nested-loop-composition: parent-owned canonical loops with inner loops
collapsed atomically for outer-body planning, while full CFG liveness and
dominance retain cycles. Enforce nesting/work bounds; validate parent ownership
and inner/outer carried values. Cross-owner/labeled break/continue, alternate
exits, intersecting/irreducible regions and terminal bodies retain legacy handling.

## Final gates

- Full default suite: **860 passed, 0 failed, 93 ignored**, 93 test targets, exit 0.
- Combined nine-target gate with JVM checks enabled: **28 passed, 0 failed,
  0 ignored**, including nine JVM checks.
- `cargo clippy --all-targets --all-features -- -D warnings`, formatting and
  diff whitespace checks passed. Final release build passed and is byte-identical
  to the binary used for corpus comparison.
- Three alternating search pairs after the full suite completed returned the
  same 53 matches over 300 classes, without errors, skips or limits. Cold medians
  were 243.93 → 244.86 ms; warm medians 1.412 → 1.320 ms. These local samples
  do not establish a general speed improvement or regression.

The companion JSON records source, test, binary, corpus and search artifact
hashes. Inventory presence remains 40/63 positions, with no fully closed visitor
or delivery unit; that is not an overall-effort completion percentage.
