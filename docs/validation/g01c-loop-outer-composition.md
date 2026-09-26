# G01-C-loop-outer-composition validation

Date: 2026-09-26. Gaps M01, M07, M08, M09, M06. Complete for bounded outer-loop composition.

## Scope

Acyclic forward branches before/after/around one canonical loop, including bypass
paths. Outer planning treats the loop as an atomic region. Full cyclic CFG
remains authoritative for dominance and liveness; existing body plans and
snapshot carried-value synchronization remain. External body/latch entries and
joins inside the loop must reject. Root validation rederives ownership/plans;
selected inconsistencies cannot retry raw operands.

This extends bounded adaptations of pinned JADX BlockProcessor and DominatorTree,
revision `28ff15e4ae69950aebea110a13e5ab895d234dfc`; it is not full visitor parity.

## Verification

Two new private tests passed; all 61 shared private tests passed. Five layouts
cover both loop-arm orientations, prefix/tail diamonds, terminal bypass return,
ordinary body branching and four body break/continue edges. Complete raw
instruction/legacy-target poisoning preserves text, frame and nonempty links.
Nine baseline-backed plan/order/ownership/operand and canonical CFG mutations
reject; malformed canonical external entries into body/latch cannot retry the
still-valid original raw stream. Raw interior-entry routes remain legacy.

Ten finite public fixtures and nine malformed cases exercise skipped/taken loops,
zero iterations, outer returns, live post-loop counters, wide/reference merges,
null/raw-float exits, and exact effect/exception ordering. The independent JVM
oracle covers **311,040 outcomes**, including **188,160 effect/failure scenarios**
and 3,840 additional header trace checks. Sol implemented production/fixtures;
Astra independently reviewed source/private/public oracle; parent inspected
projection/guard changes and metadata rejection. All final gates passed.

## Boundaries

Multiple/nested loops, distinct exits, terminal loop-body arms and unsupported
exception/allocation routes remain outside selected scope. General SSA/type-driven
emission and full block transformations remain open. No whole visitor or delivery
unit closes. No release, GUI deployment, commit or push.

## Next increment

G01-C-disjoint-loop-composition: bounded nonoverlapping canonical loops within
existing forward regions, before nested-loop ownership. Each loop owns its
header/latch/exit/special edges/body plans. Outer planning treats each as atomic;
full cyclic liveness spans all loops. Test sequential/alternative loops,
cross-loop wide/reference values, skipped effects and cross-owner edge rejection.
Nested loops remain legacy. Global single-loop assumptions must become per-region.

## Pinned corpus review

The same Play Store APK (SHA-256
`c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`)
now reconstructs **260,227 / 268,289** concrete methods, up from 260,225.
Fallbacks decrease **8,064 → 8,062**; only `goto crosses region boundary`
changes (98 → 96), and diagnostic reason samples are unchanged.

The exact gains are `j$.util.stream.n5.tryAdvance(Ljava/lang/Object;)Z` and
`chee.d(I)Lchef;`. Independent static comparison with baseline DEX found no
discrepancy. The latter duplicates the loop text in mutually exclusive outer
arms; a runtime path executes one copy. These actual APK methods were not
independently executed; the finite synthetic JVM matrix supplies runtime evidence.
This is pinned-corpus reconstruction, not universal coverage or semantic accuracy.

Local review/source artifacts are under `target/validation/g01c-loop-outer-*`,
including `g01c-loop-outer-corpus-gains-review.md`. Final report and source/binary
hashes are recorded in the companion JSON.

## Final gates

- Default full suite: **855 passed, 0 failed, 92 ignored**, 92 test targets, exit 0.
- Eight-target gate with ignored JVM checks enabled: **25 passed, 0 failed,
  0 ignored**, including eight JVM checks.
- Strict Clippy, formatting, release build and diff whitespace checks passed.
- Three alternating search runs per binary over the same 300 classes returned
  exactly the same 53 matches, without errors, skipped classes or limits. Cold
  medians were 235.14 → 236.98 ms; warm medians 1.286 → 1.209 ms. These local
  samples do not establish a general speed improvement or regression.
