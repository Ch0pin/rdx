# G01-C-natural-loop validation

Date: 2026-09-26. Gaps: M01, M07, M08, M09, M06. Complete for the bounded scope; G01 and whole visitor positions remain open.

## Production change

One existing reducible pretest loop now consumes decoded CFG edges, canonical
loop metadata and fixed-point liveness. The selected shape has one forward guard,
one unconditional backward latch, a straight-line body and a single guard exit.
Prefix, header and exit-tail instructions retain their original effect order.

Loop identification adapts the dominating-successor backedge rule in pinned JADX
[BlockProcessor.markLoops](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockProcessor.java).
The existing dominator analysis proves that the header dominates the latch and
all loop members. Reverse predecessor traversal establishes membership; additional
entries, exits and jumps are rejected. Cached CFG and loop metadata are checked
against decoded instructions before rendering. Selected invalid data cannot retry
raw rendering. This is a bounded adaptation, not full visitor parity.

Decoded liveness repeats successor-union and reverse kill/gen transfer to a fixed
point for cyclic graphs. Both words of wide operands are tracked. The existing
32 MiB storage and 16 million work budgets conservatively retain all registers
when exceeded. Forward DAGs retain their single-pass calculation.

## Defect found during validation

The new header-defined null fixture initially produced an invalid assignment to
`null` on the exit path. A retained raw-zero literal had changed representation
from integer to reference during exit casting. Selected-loop synchronization now
skips copying identical retained literal bits and rejects changed literals or
receivers. The independent JVM fixture remains a positive acceptance case.

Applying the new strict invariant checks globally initially reduced corpus
reconstruction by six methods. The checks were scoped to the selected shared-loop
path; legacy synchronization retains its existing behavior. The final corpus
report again matches the baseline exactly. This does not certify legacy invariant
handling or repair every possible legacy literal assignment.

## Verification

Public fixtures cover twelve loops and nine rejection cases. An independent JVM
oracle checks 672 outcomes, including 384 effect/failure scenarios: zero, one and
many iterations, integer/long overflow, reference swaps, final header values,
null exits, negative zero and NaN payloads. Header/body/tail trace order and exact
sentinel exception identity are checked.

The selected natural-loop, existing header-liveout and numeric-invariant targets
passed together: ten tests, zero failures, with all three optional JVM tests run.
Five private tests poison raw instructions and legacy targets for goto/8,
goto/16 and goto/32, preserving code, nonempty links and final register state.
They verify next-header-only liveness across the backedge, decoded move-based
invariant restoration, header-null exit casting, 13 malformed metadata/edge
mutations, non-dominated loop rejection and explicit legacy routing. Changed
retained literals reject reconstruction. The move-invariant poison case covers
opcode 0x01; it does not establish every move encoding's invariant behavior.

Sol implemented production and public tests; Astra independently reviewed the
production design, private tests and oracle. Parent inspected integration and
ran full gates. Strict all-target Clippy, formatting, whitespace checks and the
release build passed. Full default suite: **826 passed, zero failed, 87 ignored**.
The three selected optional JVM tests passed separately as described above.

## Corpus and search

Final pinned Play Store report: 260,225 / 268,289 concrete methods reconstructed
(96.994286%), 8,064 fallbacks. Entire report, including reason counts and samples,
is identical to the prior increment. This measures one APK's reconstruction,
not universal coverage or semantic accuracy.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.
APK SHA-256: `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.

Three alternating paired search runs over 300 classes returned identical 53
matches and source coordinates, without errors, skips or limits. Cold median:
222.615 ms before to 223.091 ms after. Warm median: 1.173 ms to 1.221 ms. This small
local sample is not a universal performance claim.

[Machine-readable evidence and source/binary hashes](g01c-natural-loop.json).

## Boundaries and next increment

Body conditionals, nested loops, multiple latches, extra exits, switches/payloads,
handlers/monitors, constructors and allocation-specific paths retain legacy
routing. The raw eligibility scan remains; selected production operands, edges,
metadata and live masks use canonical decoded data. Raw NOP validation also
remains. General SSA/type-driven
emission and full upstream block transformations remain open.

Next: **G01-C-loop-body-composition** — compose existing forward conditional
regions inside the single natural-loop body, retaining the sole guard exit and
unconditional latch. Initially exclude break/continue targets, terminal arms,
nested loops and additional latches. No release, GUI deployment, commit or push.
