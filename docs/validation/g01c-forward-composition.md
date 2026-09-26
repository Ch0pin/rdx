# G01-C-forward-composition validation

Date: 2026-09-26. Gaps: M01, M07, M09, M06. Complete for the bounded scope. G01 and whole visitor positions remain open.

## Production change

Eligible sequential/nested forward conditions now consume per-branch canonical
plans. `ControlFlowGraph::forward_postdominators` computes immediate
postdominators for a forward DAG using a virtual terminal exit and successor-chain
intersection. This is a bounded reverse-DAG adaptation of the Cooper/Harvey/Kennedy
approach in pinned JADX
[DominatorTree](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/DominatorTree.java),
not full upstream visitor parity.

A postdominator provides a safe join ceiling. Canonical region-closure checks find
the earliest valid boundary while allowing terminal paths, preserving legacy
early-return behavior for composed branches. The previously integrated single
conditional keeps its established postdominator join and source layout. Production
validates cached plans against canonical edges before rendering. Selected malformed
shared data cannot retry raw operands.

Decoded live-in bitsets union successor states and walk each block backward with
write-kill/read-gen transfer, including both words of wide operands. They govern
merges at branch joins. Analysis is bounded to 32 MiB storage and 16 million work
units; budget exhaustion conservatively retains registers rather than dropping
state. Existing branch-count and rendering-depth limits remain.

## Verification

- Four new private tests cover sequential/nested/terminal-inner raw branch and
  legacy-target poisoning with nonempty navigation links, direct join live masks,
  14 malformed cached-plan/edge mutations, decoded read mutation and wide spans.
- The storage-budget test uses a synthetic helper-level 4,101-block CFG and proves
  conservative retain-all behavior. It deliberately exceeds the production branch
  limit and is not evidence of support for that method shape.
- Public matrix: 12 positive fixtures and eight rejection cases. Fixtures cover
  sibling/nested regions, early exits, bypassed apparent joins, overwritten second
  conditions, wide/reference liveouts and incompatible dead values.
- An independent JVM oracle checks 1,225 outcomes, including 425 effect/failure
  traces over signed boundary inputs. Exact exception identity and effect order
  are checked. Shared setup may appear in mutually exclusive Java arms; runtime
  traces prove it executes once when selected, and the shared tail is emitted once.
- Sol implemented production and public tests. Astra reviewed the design, final
  code, private tests and independent oracle. Parent inspected integration and ran
  full regression, lint, build, corpus and search gates.

## Gates

Full default suite: **819 passed, zero failed, 86 ignored**. The selected optional
JVM test also passed. Strict all-target Clippy, formatting, whitespace checks and
the final release build passed.

The final pinned Play Store report is byte-for-byte identical: 260,225 / 268,289
concrete methods reconstructed (96.994286%), 8,064 fallbacks. This measures one
APK's reconstruction, not universal coverage or semantic accuracy.
APK SHA-256: `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.

Three alternating paired search runs over 300 classes returned identical 53 matches
and source coordinates, without errors, skips or limits. Cold median: 228.994 ms
before to 227.121 ms after. Warm median: 1.260 ms to 1.240 ms. This small local
sample is not a universal performance claim.
[Machine-readable evidence and hashes](g01c-forward-composition.json).

## Boundaries and next increment

Constructors, loops/backedges, switches/payloads, handlers/monitors and
allocation-specific methods retain existing routes. Goto-only/prefix-goto shapes
remain legacy. Shared CFG selection still starts with a raw eligibility scan;
canonical operands, edges, plans and live masks govern selected rendering.
General SSA/type-driven emission and complete upstream block transformations remain
open. Full `MethodAnalysis` still has its existing raw CFG builder.

Next: **G01-C-natural-loop** — one existing reducible pretest loop with one dominated
backedge and one exit, integrating canonical loop metadata and decoded fixed-point
liveness. Nested loops, multiple latches, additional exits and protected/allocation
shapes stay outside that increment. No release, GUI deployment, commit or push.
