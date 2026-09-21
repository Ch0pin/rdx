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
Resource IDs remain numeric: ARSC symbol lookup and manifest enum/flag names
are not yet ported. This is not the complete upstream resources subsystem.

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
This stage is exposed through a separate corpus audit; the GUI source renderer
still uses the existing register-value lowering. See
[`docs/jadx-port-plan.md`](../../docs/jadx-port-plan.md) for the remaining passes.

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
