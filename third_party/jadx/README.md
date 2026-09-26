# JADX attribution and native DEX port

The initial Rust DEX parser in `src/native_dex.rs` adapts parsing logic from
[JADX v1.5.6](https://github.com/skylot/jadx/tree/v1.5.6), pinned to commit
`28ff15e4ae69950aebea110a13e5ab895d234dfc`.
JADX is copyright Skylot and its contributors; applicable Android Open Source
Project and other upstream notices are preserved in [NOTICE](NOTICE).

The reference implementation is the
[DEX input plugin](https://github.com/skylot/jadx/tree/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-plugins/jadx-dex-input/src/main/java/jadx/plugins/input/dex),
including `DexReader`, `sections/DexHeader`, `sections/SectionReader`,
`sections/DexClassData`, and `utils/Leb128`.

RDX modifications translate parser logic into Rust with checked byte access,
explicit error propagation, and RDX-owned class/member representations. The
native source emitter is new RDX code. This is an initial, partial parser and
native-engine implementation, not a complete port of JADX's decompilation
pipeline or a claim of equivalent output coverage.

The files below are unmodified copies downloaded from the pinned release:

- [LICENSE](LICENSE): <https://raw.githubusercontent.com/skylot/jadx/v1.5.6/LICENSE>
  (Git blob `8dada3edaf50dbc082c9a125058f25def75e625a`).
- [NOTICE](NOTICE): <https://raw.githubusercontent.com/skylot/jadx/v1.5.6/NOTICE>
  (Git blob `5c0b69a0f5298e0b329e33e860f7626f0c2c3891`).

The full upstream NOTICE is retained, including historical bundled-library and
icon notices. Retention does not mean that the Rust parser incorporates all of
those libraries or assets. The Java implementation and its runtime dependencies are no longer shipped.

Distributions containing the adapted parser must include the applicable license
and notices. Changes derived from additional upstream files should retain their
notices and extend this source mapping as the port grows.

## Native Android binary XML decoding

`src/native_resources.rs` maps the chunk dispatch, namespace, element and typed
attribute parsing of pinned JADX v1.5.6
[`jadx-core/src/main/java/jadx/core/xmlgen/BinaryXMLParser.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/xmlgen/BinaryXMLParser.java)
to native Rust. Its string-pool reader and bounded XML emitter are RDX code.
The decoder validates input/chunk/string boundaries, supports UTF-8/UTF-16
pools, and handles common Android typed attribute values without a Java runtime.
RDX also implements resource-table lookup and supported manifest enum/flag names. This is not the complete upstream resources subsystem.

## Native Java reconstruction

`src/native_java/` is RDX's conservative Rust register-value lowering and Java
emission layer over the adapted DEX reader. It handles supported typed instructions, forward branches, simple loops and
forward switches with explicit effect materialization and generated source mappings.
Array/type opcode handling follows the [AOSP DEX instruction specification](https://source.android.com/docs/core/runtime/dalvik-bytecode).
It is not a port of JADX's CFG/SSA/type-inference pipeline and does not claim
its reconstruction coverage. Unsupported methods retain native DEX output.

## Basic-block pipeline port

`src/native_cfg.rs` adapts the split/connect approach from pinned JADX
[`BlockSplitter.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockSplitter.java).
The Rust implementation operates directly on checked DEX code units, retains
goto instructions and original offsets, excludes payload data, and conservatively
isolates protected instructions for exceptional edges. It does not yet implement
JADX's synthetic block transformations, SSA or region construction. Dominator
analysis is a separate stage described below.
The raw stage remains available for full method analysis. G01-C-start additionally
builds blocks from shared decoded instructions and makes the GUI renderer consume
their edges and joins for one eligible forward conditional, while retaining the
existing register-value lowering. See the
[native engine documentation](../../docs/native-engine.md) for remaining integration.

`src/native_dominators.rs` adapts pinned
[`DominatorTree.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/DominatorTree.java):
the Cooper/Harvey/Kennedy iterative immediate-dominator algorithm, predecessor
intersection and dominance-frontier walks. Rust modifications preserve original
block IDs, use iterative reverse-postorder traversal, cap work/frontier storage,
and use tree intervals for dominance queries instead of per-block dominator
bitsets. A virtual entry predecessor handles back edges to the method entry.
Reachability is from the actual method entry over normal and conservative
exceptional edges; disconnected handler blocks are reported as unreachable.
This is dominator analysis, not the complete `BlockProcessor` transformation pass.

## Instruction operands for SSA

`src/native_ir.rs` adapts the instruction-family operand mapping from pinned
[`InsnDecoder.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/instructions/InsnDecoder.java).
Its checked raw DEX operand reader follows the AOSP instruction formats and is RDX
code. It retains offsets/opcodes, ordered register reads and writes, word widths,
literals, indexed references and conservative throwing behavior. Unlike upstream,
it does not yet resolve pool entries, merge call results or perform type inference.
Invocation arguments remain ordered raw register words until signature resolution.
`src/native_calls.rs` now resolves method pool entries and groups receiver/argument
words by their effective prototypes, following the same pinned `InsnDecoder`
invoke/result conventions. It handles wide arguments, array owners, polymorphic
secondary prototypes and filled arrays, and links adjacent typed move-result
instructions. Custom invokes explicitly reject missing call-site metadata.
This is signature binding, not virtual dispatch resolution or SSA.

The register categories describe storage width/reference constraints, not inferred
Java types. CFG and operand decoding share one instruction-width decoder; the old
source renderer remains separate while this analysis pipeline is built.

## Additional native metadata and typed lowering

`src/native_dex_metadata.rs` decodes encoded values, try/catch handler lists and
shared class/field/method/parameter annotation sets from the
[AOSP DEX format](https://source.android.com/docs/core/runtime/dex-format).
Checked offsets, allocation/work budgets, shared handler storage and the Rust
representations are RDX code. `src/native_java/annotations.rs` renders common Java
annotation values, retaining type/enum links and escaped strings/chars. Annotation
placement follows pinned [`AnnotationGen.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/codegen/AnnotationGen.java).
Build/runtime annotations are displayed, including on methods with DEX fallback
bodies. System annotations remain metadata; Throws also renders as a throws clause.
Unsupported Java values are marked explicitly. Annotation defaults and debug
metadata are not fully reconstructed.

`src/native_java/numeric.rs`, `strings.rs`, `liveness.rs` and the exception
renderer are RDX implementations over the DEX instruction semantics. Their
conservative fallback boundaries and independent Rust behavior fixtures are
documented in `docs/native-engine.md` and `docs/validation.md`. They do not
execute or embed upstream Java code.

## Native SSA

`src/native_ssa.rs` adapts live-in-pruned dominance-frontier phi insertion and
renaming from pinned JADX
[`SSATransform.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ssa/SSATransform.java).
Rust adaptations use iterative traversal, bounded word identities and synthetic
normal-success blocks to preserve pre-write exceptional state. The latter replaces
upstream post-renaming try-edge repair. Phi simplification remains unported;
partial type inference is described below. `native_call_values.rs` attaches
existing signature constraints to SSA words. No upstream Java executes.

## SSA type bounds and constructor identities

`src/native_types.rs` adapts the assignment/use-bound separation and propagation
sequence from pinned JADX [`TypeInferenceVisitor.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/typeinference/TypeInferenceVisitor.java)
and [`TypeUpdate.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/typeinference/TypeUpdate.java).
The bounded Rust worklist, word-pair checks, literal alternatives and explicit
unresolved/conflict results are RDX adaptations. This is partial inference:
array-element listeners now propagate load types and store constraints. General
backwards array inference, reference least upper bounds, generics and conversion
insertion remain incomplete.

`src/native_constructors.rs` follows SSA assignment chains as in pinned
[`ConstructorVisitor.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ConstructorVisitor.java).
RDX checks allocation dominance and retains origin and original invoked owner.
A differing owner is marked for retargeting only when the hierarchy proves it
is an ancestor of the allocation type. Chaining through a proven ancestor on
`this` follows pinned [`ConstructorInsn.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/instructions/ConstructorInsn.java). This analysis does not remove or move
instructions, establish initialization-state validity, or emit constructors.

The bounded nested-allocation path in `native_java/allocation_lowering.rs` now
consumes these SSA constructor bindings. Exact allocation/invoke identities and
an RDX effect-event check gate shared-capture Java expressions. This integration
is not a port of the full JADX region/code-generation pipeline; owner-retarget
emission, exception regions and general initialization-state verification remain
unsupported in this path.

Array assignment relationships in `native_hierarchy.rs` follow
[JLS 4.10.3](https://docs.oracle.com/javase/specs/jls/se8/html/jls-4.html#jls-4.10.3):
reference component covariance, invariant primitive components and the standard
Object/Cloneable/Serializable supertypes. Descriptor nesting is bounded; missing
external class relationships remain unknown.

The small platform hierarchy also includes verified interface edges for
[Throwable / Serializable](https://docs.oracle.com/javase/8/docs/api/java/lang/Throwable.html)
and [SQLException / Iterable](https://docs.oracle.com/javase/8/docs/api/java/sql/SQLException.html).
These facts avoid false negative subtype answers from the previous exception-only
parent graph; they do not constitute a complete Android platform classpath.

## Builder calls within allocation arguments

The bounded allocation decoder recognizes ignored `StringBuilder.append(String)`
results, guided by the unchained builder-use pattern in pinned
[`SimplifyVisitor.convertInvoke`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/SimplifyVisitor.java).
RDX retains actual constructor/append calls and exact effect traces; it does not
perform upstream's full string-concatenation transformation. Only the exact final
platform class and overload with its documented receiver-return contract are
accepted; arbitrary fluent-looking methods remain unsupported. See
[`StringBuilder.append(String)`](https://docs.oracle.com/en/java/javase/17/docs/api/java.base/java/lang/StringBuilder.html#append(java.lang.String)).

## Ancestor-owner superclass calls

Class `invoke-super` emission in `native_java/method.rs` follows the superclass
handling in pinned [`InsnGen.callSuper/getClassForSuperCall`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/codegen/InsnGen.java).
RDX proves strict superclass ancestry using its bounded immutable hierarchy,
rather than requiring the DEX method owner to equal the direct parent. It emits
`super.method(...)` and retains the original DEX signature in navigation metadata.
The receiver must still be the current instance. Interface defaults, enclosing
class qualified-super calls and incomplete/ambiguous ancestry remain unsupported.
Class/interface dispatch distinctions are specified by
[AOSP's invoke-kind documentation](https://source.android.com/docs/core/runtime/dalvik-bytecode).

## Readable allocation staging

Pinned JADX `ConstructorVisitor.processInvoke` removes the originating
`NEW_INSTANCE` and replaces the constructor invoke in place; `InsnGen` then emits
`new Class(arguments)`. Relevant upstream sources:

- [ConstructorVisitor lines 84–110](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ConstructorVisitor.java#L84-L110)
- [InsnGen constructor output](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/codegen/InsnGen.java#L727-L785)
- [InsnNode reorder classification](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/nodes/InsnNode.java#L244-L283)

RDX's native `allocation.rs` keeps its stricter expression reconstruction first.
For flat windows only, it can stage ordered capture declarations and place `new`
at the constructor position, matching the upstream readable reconstruction
approach. Unlike upstream's broader reorder classifications, RDX still checks
all recorded cast/call/read/string events in their original order. The intentional
allocation relocation can change class-initialization, linkage and allocation
failure timing; output coverage does not establish full semantic equivalence.

## Synchronized-region reconstruction

`src/native_java/synchronized.rs` adapts the entry/body/monitor-exit reconstruction
approach of pinned `jadx-core/src/main/java/jadx/core/dex/visitors/regions/maker/SynchronizedRegionMaker.java`.
The Rust implementation adds bounded CFG traversal, decoded register-write and
exception coverage checks, exact cleanup validation, and conservative rejection of
nested/multiple-release/mixed exception shapes. It is a partial implementation,
not a complete port of the upstream region maker. Original SPDX/license terms
and notices remain covered by the files above.

## Loop escape regions

`native_java/method.rs` follows the control-flow membership and exit-path approach
of pinned JADX's
[`LoopInfo.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/attributes/nodes/LoopInfo.java)
and
[`LoopRegionMaker.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/maker/LoopRegionMaker.java).
Instruction address order alone does not determine whether an edge leaves a
loop. RDX discovers its supported loop regions before validating escape paths,
then follows the original normal edges to reject reentry. Separately validated
downstream loops may remain on an escape path; this is not a termination proof.
Protected instructions require a separate exception-ownership proof and are
excluded from this lowering.

This is a bounded Rust adaptation of the approach, not a full port of JADX's
dominator-frontier outblock selection or loop visitors. RDX can duplicate a
shared continuation into mutually exclusive exits, subject to its existing
work and output limits. It does not introduce a Java runtime dependency.

## Class and package display aliases

`native_java/names.rs` follows the separation of original identity and valid
source aliases in pinned JADX's
[`RenameVisitor.java`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/rename/RenameVisitor.java),
particularly `checkClassName` and `checkPackage`. RDX uses its existing injective
UTF-8 hex alias scheme rather than JADX's configurable alias provider and global
collision pass. Headers, constructors, type operands and imports use aliases;
source links retain original DEX names. This is not a full RenameVisitor port.

The synchronized-region subset additionally handles a nonthrowing loop latch
outside the DEX protected interval and emits loops wholly inside a proven monitor
region. It retains the pinned maker's monitor-region separation while requiring
coverage for every throwing body instruction other than the proven release.

## Nested duplicate-cleanup reconstruction

`native_java/finally_regions.rs` uses the duplicated-cleanup recognition approach
of pinned JADX's [MarkFinallyVisitor.java](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/finaly/MarkFinallyVisitor.java).
This is a bounded Rust subset, not a full visitor port: one stable-input void
cleanup invocation, one normal copy, a catch-all rethrow, and an enclosing typed
catch with terminal paths. It checks original per-instruction exception dispatch,
control-flow boundaries, cleanup operands and source links before emitting.
Unsupported nested layouts still fall back.

Exact Android framework exception metadata is based on the platform declarations:
[ActivityNotFoundException](https://developer.android.com/reference/android/content/ActivityNotFoundException),
[RemoteException](https://developer.android.com/reference/android/os/RemoteException),
and [IBinder.transact](https://developer.android.com/reference/android/os/IBinder#transact(int,android.os.Parcel,android.os.Parcel,int)).

## Frida clipboard action

The method-snippet UI and logging format in `src/frida_snippet.rs` follow
[JADX FridaAction](https://github.com/skylot/jadx/blob/master/jadx-gui/src/main/java/jadx/gui/ui/action/FridaAction.java).
The Rust generator reads exact DEX symbol descriptors, always selects the exact
overload, uses positional argument names, and calls that captured overload.
Each snippet is scoped inside `Java.perform` so pasted snippets cannot overwrite
one another's method handles. Class-wide and field snippets are not implemented.

## G01-A shared front-end integration

`native_method::MethodFrontEnd` now shares the existing attributed
`native_ir` instruction decoder and `native_calls` signature binder between
full method analysis and eligible straight-line Java rendering. Production
rendering consumes decoded instruction boundaries and invocation bindings.
G01-B incrementally adds decoded move, numeric constant, reference constant,
move-result, return and arithmetic operands (109 comparison/unary/binary encodings).
Arithmetic preserves the existing native numeric lowering and boolean fast paths;
this is operand integration, not full ProcessInstructionsVisitor parity.
G01-B-fields adds shared field operands and pool identity for 28 instance/static
encodings, retaining existing resolution, effects and constructor guards.
G01-B-array-types adds 18 array/type encodings using normalized operands for
shared and legacy lowering, preserving existing exception and store checks.
G01-B-throw adds shared throw operands without changing exception/prologue logic.
G01-B is complete only for currently supported eligible straight-line operands.
G01-C-start builds canonical blocks and successors from decoded instructions,
following BlockSplitter split/connect and target-identity handling. Production
consumes those edges, the join, decoded branch operands and decoded tail liveness
for one eligible forward conditional. This includes terminal-arm and bypass shapes;
constructor branches and complex/excluded legacy shapes remain separate work.
Synthetic blocks, exception normalization and general SSA/type-driven emission
remain open. This integration adds no claim of full
`CheckCode`, `AttachMethodDetails` or `ProcessInstructionsVisitor` parity.
See [native engine documentation](../../docs/native-engine.md).

## G01-C forward composition

Production forward conditional rendering now consumes per-branch canonical plans,
reverse-DAG postdominators and terminal-aware region closure. Postdominator chain
intersection follows the Cooper/Harvey/Kennedy approach used by pinned
`DominatorTree.java`, applied to the reversed forward-only graph with a virtual
terminal exit. This bounded adaptation is not full visitor parity.
Decoded block live-in bitsets use successor union and reverse kill/gen transfer,
with conservative retain-all behavior when storage/work budgets are exhausted.
Existing single-condition layouts remain stable; sequential/nested forward
conditions now share the same decoded data. Loops, exceptions, constructors and
allocation-specific paths retain their existing routes.
See [native engine documentation](../../docs/native-engine.md).

## G01-C natural loop

The selected pretest-loop path adapts `BlockProcessor.markLoops`' dominating
successor rule for identifying a backedge. The existing dominator analysis and
reverse predecessor closure prove single-entry loop membership. The bounded
renderer accepts one guard, one unconditional latch, a straight-line body and
one guard exit. Canonical metadata is revalidated before emission. Decoded
liveness now iterates to a fixed point for this cyclic graph, with conservative
retain-all behavior on budget exhaustion.

This is not full BlockProcessor or loop-restructuring parity. Body conditionals,
multiple latches/exits, nested loops, exceptions and allocation-specific paths
retain legacy routing. See [native engine documentation](../../docs/native-engine.md).

## G01-C loop body composition

The selected single-loop path now composes forward body conditionals using the
existing canonical postdominator/region-closure planner. A planning copy cuts only
the latch backedge. Dominance, natural-loop membership and fixed-point liveness
continue to use the complete cyclic graph. Root validation recomputes body plans;
selected malformed plans cannot fall back to raw branch operands.

This combines the bounded BlockProcessor/DominatorTree adaptations described
above, not full upstream loop visitor parity. Body terminal arms, break/continue,
nested loops and multiple latches remain outside the selected path. See
[native engine documentation](../../docs/native-engine.md).

## G01-C common-exit loop break

The bounded BlockProcessor/DominatorTree adaptation now admits one conditional
body break to the existing guard exit. Canonical metadata validates that edge;
the planning copy removes its taken successor while the complete cyclic CFG
retains dominance and liveness authority. Exit slots preserve header and early
exit values, including proven restored literals. This is not full upstream
visitor parity. Continue, multiple breaks and nested loops remain outside the
selected scope. See [native engine documentation](../../docs/native-engine.md).

## G01-C conditional loop continue

The bounded BlockProcessor/DominatorTree adaptation admits one conditional
backedge to the same dominating header, alongside one unconditional latch and
optional common-exit break. Canonical edge metadata drives condition rendering
and snapshot header-slot synchronization. Planning cuts taken continue/break
edges while full cyclic liveness preserves them. LoopRegionMaker's synthetic
continue insertion was inspected as a reference, not ported wholesale. Multiple
conditional backedges and nested loops remain outside this increment. See
[native engine documentation](../../docs/native-engine.md).

## G01-C loop edge composition

The bounded BlockProcessor/DominatorTree adaptation now collects multiple
conditional break/continue edges targeting the same header/common exit, retaining
one unconditional latch. Root validation recomputes the ordered collection; full
cyclic dominance/liveness and restored-literal proofs include every edge. Forward
planning removes taken special edges, preserving actual fallthrough. Binary-search
lookups and the existing 1,024 total-control budget bound the widened route.
This is not full visitor or nested-loop parity. See
[native engine documentation](../../docs/native-engine.md).

## G01-C outer loop composition

The bounded single-loop BlockProcessor/DominatorTree adaptation now composes
acyclic regions before/after/around the loop, including bypasses. Outer planning
projects the loop to its header-to-exit edge and hides interior successors; the
complete cyclic graph remains the liveness/dominance authority. Canonical guard
selection uses the header terminator, and merged body/outer plans reject interior
joins or external body/latch entries. This is not full visitor or nested-loop
parity. See [native engine documentation](../../docs/native-engine.md).

## G01-C disjoint loop composition

The bounded BlockProcessor.markLoops/registerLoops adaptation now collects up to
32 nonoverlapping canonical pretest loops. Dominators are computed once; each
region owns its latch, guard, exit and conditional break/continue edges. Shared
body and atomic outer projections support sequential and alternative loops,
while original cyclic CFG liveness spans all regions. Root validation rederives
regions, owner-tagged edges and plans. Membership and body planning have shared
work budgets, in addition to the existing 1,024-control bound.

Nested/intersecting loops and unsupported exits remain on existing handling.
This is not full upstream visitor parity. See
[native engine documentation](../../docs/native-engine.md).

## G01-C nested loop composition

The native bounded adaptation uses BlockProcessor.processNestedLoops as a
reference for nearest-containing parent ownership. Up to four levels share
body-planning work budgets; immediate children collapse atomically during
parent planning while complete cyclic CFG remains authoritative for liveness
and dominance. Malformed parent/edge/branch caches reject without raw retry.
This does not complete the upstream visitor. See
[native engine documentation](../../docs/native-engine.md).

## G01-C posttest loop

The bounded native specialization references LoopRegionMaker.process(),
makeLoopRegion() and isExitAtLoopEnd() at the pinned revision. Canonical
membership/dominance and decoded conditional-latch operands drive a mandatory
straight-line body with one exit. Discarded type preview does not hoist runtime
effects, and distinct exit snapshots preserve body-only liveouts. This is not
complete condition-at-end region parity. See
[native engine documentation](../../docs/native-engine.md).
