# Native JADX pipeline port

Latest scoped update: [JADX-guided loop exit comparison](validation/jadx-loop-comparison.md)
and [pinned APK coverage evidence](validation/coverage-jadx-loops.json).
The 2026-09-25 loop batch integrates graph-based escape checks and distinct
header/exit register liveness into the current renderer. It does not complete
the general SSA-to-region pipeline or certify a whole upstream visitor as ported.

Reference: JADX 1.5.6, commit `28ff15e4ae69950aebea110a13e5ab895d234dfc`,
[`Jadx.getRegionsModePasses`](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/Jadx.java).
The active implementation checklist is [jadx-gap-inventory.md](jadx-gap-inventory.md).
Runtime and production analysis remain Rust only.

## Historical implementation notes

The [canonical gap inventory](jadx-gap-inventory.md) supersedes the earlier
2026-09-21 status table, implementation ordering and 87.88% baseline. It records
all 63 pinned visitor positions, verified RDX integration, remaining behavior
and completion gates. The sections below retain earlier implementation evidence;
their counts and limitations describe the increment when recorded, not necessarily
the current renderer. Current corpus evidence is linked at the top of this file.

### Shared method context: first integration increment

`src/native_method.rs::MethodAnalysis` now owns decoded operands, CFG, SSA,
signature-bound calls and SSA call values for one borrowed immutable method.
Constructor and optional type analysis use that same context, preventing downstream
passes from accidentally combining independently built analysis inputs.
The existing renderer's lazy constructor-analysis path uses this context with the
same pass order. It still caches only constructor bindings, rather than retaining
all analysis graphs for each displayed method. Type inference is not added to the
search/render hot path. This is plumbing for stage 1, not completion of that stage
or a claim that the general emitter now uses inferred SSA types.

Constructor fixtures compare shared-context results against independently assembled
passes, including aliases, phis, exception state and unresolved owners. Optional
type analysis compares both success and error results: several constructor-only
fixtures intentionally lack matching parameter signatures. Missing code and
truncated instructions must fail explicitly.

Validation: 579 regular tests passed, 28 opt-in tests ignored; all five
Play Store backup/callback/provider regressions passed separately. Strict
all-target Clippy and formatting passed. Logs: `target/validation/shared-method-{tests,clippy,apk}.log`.
No application bundle was replaced for this internal refactor.

### Shared invoke binding in Java emission

The ordinary invoke emitter now consumes `native_calls::bind_invocation` for
receiver, argument registers/types, method target and return type. This shares the
same binder used before SSA call-value binding, replacing the emitter's independent
wide-argument cursor and metadata interpretation. Calls stay at their original
emission positions; typed-null, overload, super/private dispatch and source links
retain their existing handling. Full SSA/type inference is not executed for every
rendered method, and general SSA-to-expression integration remains open.

The full analysis path and per-invoke path are compared on compact/range calls,
wide arguments, duplicate registers and discarded results. Invalid operand counts,
noncontiguous wide pairs, bad descriptors and invalid pool references fail.
The existing move-result checks still validate result placement/type in the emitter.

Five full Play Store class outputs are byte-identical before/after. Six opt-in APK
regressions pass, including preservation of SystemJobService's five working methods.
A three-run median search comparison over the same 2,000 classes found identical
matches: cold 0.943 s before / 0.954 s after; warm 3.61 ms / 3.96 ms. This bounded
measurement is not proof of whole-corpus performance parity. Evidence and binary
hashes: `target/validation/call-binding/summary.json`.

Validation: 580 regular tests passed (29 opt-in tests ignored), six APK tests
passed separately, and all ten call-binding tests passed with the final operand
boundary guard. Strict all-target Clippy, release build, formatting and diff
checks passed. Logs are in `target/validation/call-binding/`. The installed GUI
bundle is unchanged; this batch changes shared internal call binding.

At the call-binding checkpoint, SystemJobService had four region/allocation fallbacks: `b` (distinct
protected exits), `onCreate` (constructor effects), `onStartJob` (multiple try
regions), and `onStopJob` (monitor-enter). These were not fixed by call binding; the following region increment addresses them.

### Monitor regions and bounded exception integration

Validation completed: 589 regular tests passed (31 opt-in/ignored), plus 8 real-APK
regressions. Strict Clippy, formatting and the release build passed. The reported
SystemJobService now reconstructs 9/9 methods with zero fallbacks; the Expedia Hilt
componentManager also reconstructs. Verified release binaries were installed into
`target/RDX.app`. Evidence and binary hashes: `target/validation/regions-validation.json`.
These results do not establish full-corpus coverage or universal semantic equivalence.

`native_java/synchronized.rs` follows the pinned `SynchronizedRegionMaker` model:
identify monitor entry, traverse normal edges to releases, reconstruct the body,
and omit proven compiler cleanup. This is a partial port with explicit extra
restrictions, not full synchronized/region parity. Decoded instruction writes and
throwing effects validate the lock and protected ranges. Catch-all metadata and
normal predecessor checks exclude unmatched exits, overwritten locks, external
entries, unrelated protected effects and non-exact cleanup. Nested monitors,
multiple normal releases and mixed ordinary/monitor exception regions still fail
conservatively. Synthetic self-protected release handlers are recognized.

The emitter preserves escaping values through declarations outside the synchronized
body and assignments inside it. It emits real Java `synchronized`, retaining lock
acquisition/release on exceptional paths. Method modifiers are separate from this
new monitor reconstruction. A metadata prefilter preserves the cheap rejection of
ordinary multi-try methods and avoids whole-method analysis on ordinary calls.

Two bounded exception improvements complete the reported SystemJobService class:
multiple protected exits can include only nonthrowing moves/constants/return tails;
effectful tails and potentially throwing return conversions remain rejected.
Constructor capture lowering may run within one unchanged protected interval only
when no mutable handler-observed snapshot is needed. Staging across a catch boundary
remains forbidden. Existing readable allocation timing policy still applies.

Tests include six modeled monitor scenarios (normal, null lock, exceptional call,
for detached and interleaved cleanup), six missing-monitor negative controls,
malformed-lock/control-flow rejections, and constructor normal/catch comparisons
with the previously documented allocation relocation excluded. These are bounded
Rust models, not Android/JVM execution. Existing handler-observed capture tests
remain part of the regression suite.

Bounded search check: identical matches and no errors over 2,000 classes. Median
cold time was 0.978 s (previous call-binding checkpoint 0.954 s); warm time 3.24 ms.
Three repetitions, load excluded. This does not establish whole-corpus parity.
Artifacts: `target/validation/regions-search-summary.json` and individual runs.

Real APK targets: SystemJobService's `onStartJob`, `onStopJob`, `b`, `onCreate`,
and Expedia Hilt_NonDismissibleBannerActivity's `componentManager`. This increment
does not complete general SSA emission, nested try reconstruction or full JADX
region parity.

### Implementation order and completion gates

1. **Connect the analysis pipeline to a shared typed method representation.**
   Preserve instruction offsets, identities, exception edges, invoke owners and
   unresolved type constraints. Gate: general emission consumes this representation,
   with tests for wide values, aliasing, calls and exceptional pre-write state.
2. **Port block normalization and region construction as dependent passes.**
   Include synchronized, nested try/catch/finally, loops, switches and multiple
   exits. Use pinned RegionMakerVisitor and its makers as the reference. Gate:
   explicit region-edge validation and positive/negative monitor-release tests,
   including the Hilt shape; no class-name-specific rewrites.
3. **Port expression and variable passes onto that representation.** Follow
   constructor, constants, type repair, shrink, simplify and scope dependencies.
   Gate: evaluation order, null checks, dispatch, overloads and exception behavior
   tested; preserve the documented readable-allocation policy explicitly.
4. **Port remaining class-level and source refinements.** Work through every
   Missing inventory row: signatures, generics, anonymous classes, enums, field
   initialization, Kotlin/debug names and renaming. Gate: whole-class output,
   annotations/imports and source navigation tests, not merely accepted methods.
5. **Validate the integrated pipeline and then make it the default.** Keep the
   existing renderer available during migration. Gate: pinned corpus comparison,
   semantic negative controls, and search/memory/navigation regressions. Report
   remaining failures honestly; no universal 100% claim from a finite corpus.

Stages may contain several commits, but each delivery must finish one stated
pass and its tests. Updating this table requires the implementation, integration
call site and validation evidence together. A standalone helper or isolated
sample fix does not close a visitor. No runtime Java dependency is introduced.

## Measured starting point (2026-09-20)

Full APK scans, no package exclusions, current release renderer:

| Input | Concrete methods | Accepted by renderer | DEX fallbacks | Accepted |
| --- | ---: | ---: | ---: | ---: |
| Zoom | 706,072 | 637,880 | 68,192 | 90.34% |
| Expedia | 1,175,337 | 1,067,088 | 108,249 | 90.79% |

These numbers measure method-renderer acceptance, not semantic correctness,
whole-class validity, or parity with JADX. Abstract/native declarations are excluded.
Each fallback reports only its first failure; fixing that failure may expose another.
The APKs share dependencies, so counts across them do not represent unique methods.

Reports and input/binary SHA-256 identities are in
`target/validation/jadx-port-baseline-{zoom,expedia,inputs}.json`.

The largest observed categories explain why individual rendering patches are insufficient:

| First failure | Zoom | Expedia | Required foundational work |
| --- | ---: | ---: | --- |
| Effectful instruction between allocation and constructor | 14,907 | 30,152 | SSA identity, constructor analysis, effect-preserving expression construction |
| Loop requires a single forward exit guard | 6,846 | 16,019 | General CFG, loop analysis and regions |
| Loop changes register type | 58 | 14,645 | SSA and type inference per value rather than per register |
| Unsupported loop interior edge | 5,336 | 6,205 | Dominators, loop edges and structured regions |
| Nested or multiple try regions | 4,972 | 5,301 | Exception-aware CFG and exception regions |

## Pass mapping and delivery gates

Upstream order matters. A later stage must consume explicit analysis from its
predecessors; it must not rediscover register identities through text substitutions.

| Order | Upstream passes | Current RDX state | Native port and acceptance gate |
| --- | --- | --- | --- |
| 1 | `CheckCode`, `AttachTryCatchVisitor`, `AttachMethodDetails`, `ProcessInstructionsVisitor` | Register/operand decoder and method-signature/result binding implemented | Complete call-site metadata, remaining symbolic operands and block integration; preserve source offsets and exception metadata |
| 2 | `BlockSplitter`, `BlockProcessor`, `BlockFinisher` | Separate basic-block and dominator stages implemented; renderer still uses its old `Graph` | Complete synthetic block transformations and loop metadata; integrate after typed instruction IR and SSA are ready |
| 3 | `SSATransform`, `MoveInlineVisitor`, `ConstructorVisitor`, `InitCodeVariables` | Word SSA, call constraints and constructor analysis implemented; bounded nested allocation emission consumes SSA constructor identities; general renderer still uses mutable registers | Stable value identities, phi insertion/renaming, wide-register semantics, def/use chains, constructor receiver tracking; validate branch joins, loops, nested allocation and exceptions |
| 4 | `MarkFinallyVisitor`, `ConstInlineVisitor`, `TypeInferenceVisitor`, `FixTypesVisitor`, `FinishTypeInference` | Bounded SSA assignment/use-bound propagation implemented; unresolved cases remain explicit | Constraint-based type propagation, exception/finally analysis and effect-safe constants; validate incompatible joins and boolean/reference zero ambiguities |
| 5 | `RegionMakerVisitor`, `IfRegionVisitor`, `ReturnVisitor`, `CleanRegions`, `CheckRegions`, `LoopRegionVisitor`, `SwitchBreakVisitor` | Direct recursive rendering for supported patterns | Region IR over analyzed CFG, including multiple exits, switches and try/catch/finally; every emitted edge must correspond to original behavior |
| 6 | `GenericTypesVisitor`, `CodeShrinkVisitor`, `SimplifyVisitor`, `ProcessVariables`, `ApplyVariableNames`, `PrepareForCodeGen` and code generation | Existing Java text emitter, import shortening and symbol links | Structured emission with source/reference maps, scope-correct declarations and expression effects; retain manifest/search/navigation regressions |

This table groups related passes; it is not the complete list of upstream visitors.
The pinned upstream list is authoritative for dependency ordering, including later
enum, anonymous-class, field-initializer, Kotlin and generic-type refinements.

## Integration rules

1. Keep byte offsets, raw symbol identities and handler order attached to IR nodes.
2. Land each stage with invariant tests and corpus diagnostics before switching the GUI renderer to it.
3. Distinguish graph acceptance, Java emission acceptance and semantic validation in reports.
4. Never remove a fallback guard merely to improve the reported percentage.
5. Preserve upstream notices and record exact source mappings for adapted algorithms.

The historical first increments were basic-block construction and dominator analysis. They do
not replace the existing renderer or claim to eliminate its fallbacks; typed IR,
SSA, type inference and regions must follow before the new pipeline generates Java.

### First increment delivered

`src/native_cfg.rs` implements checked instruction boundaries, basic-block splitting,
normal and conservative exceptional edges, payload exclusion and unreachable-code
pruning. Handler entries are conservative reachability roots. Reserved opcodes,
truncation, invalid branches, forged payload pointers and invalid try ranges fail
explicitly. Successor deduplication uses a hash set rather than quadratic scans.

`--native-cfg-audit` runs this stage independently. The final corpus audit accepted
all 706,072 Zoom and 1,175,337 Expedia concrete methods with zero graph-construction
rejections. That is not evidence that all methods can be emitted as Java; the
renderer baseline above remains unchanged. Corpus audit artifacts are
`target/validation/jadx-port-cfg-{zoom,expedia}.json`.

Validation: 324 regular tests passed, 6 opt-in tests ignored; strict all-target
Clippy, formatting and release build passed. Eleven dedicated CFG tests exercise
diamonds, loops, packed/sparse switches, exceptional edges, payload alignment,
malformed targets and reserved/truncated instructions. No Java process is used.

### Second increment: dominators and dominance frontiers

`src/native_dominators.rs` ports the core algorithm in pinned JADX
`DominatorTree.java`: iterative immediate-dominator intersection followed by
predecessor walks to build dominance frontiers. The implementation exposes
predecessor lists, reverse postorder, immediate dominators, reachability and
constant-time dominance queries. Original block IDs remain stable.

Tree intervals replace full dominator bitsets to avoid quadratic storage. Both
normal and exceptional edges participate. A virtual predecessor handles loops
back to entry; disconnected handlers do not become artificial entry roots.
Analysis has explicit block, edge, work and frontier-entry budgets. A budget
failure remains a separately reported analysis rejection.

Validation compares every directed graph with 1–3 nodes and 280 seeded larger
graphs against an independent set-intersection oracle, including immediate
dominators, all-pairs dominance and frontiers computed from their definition.
Additional cases cover irreducible cycles, exceptional edges, duplicate edges,
entry self-loops, disconnected handlers, invalid inputs, budget exhaustion and a
20,000-block chain. The complete regular suite passes 331 tests (6 opt-in tests
ignored), with strict all-target Clippy, formatting and release build passing.

Full APK audits completed with zero graph or dominator-analysis rejections:

| Input | Methods analyzed | Dominance-frontier entries |
| --- | ---: | ---: |
| Zoom | 706,072 | 1,244,364 |
| Expedia | 1,175,337 | 2,255,900 |

Evidence: `target/validation/jadx-dominators-{zoom,expedia,validation}.json`;
the validation manifest pins the binary and APK hashes. These are analysis-stage
results, not an improvement to Java renderer acceptance or semantic coverage.

This stage still operates on the conservatively split CFG, not a completed JADX
`BlockProcessor`. Synthetic block transformations, instruction-level definitions
and uses, SSA renaming/phi nodes and region generation remain to be ported.

### Third increment: explicit instruction operands

`src/native_ir.rs` maps standard DEX instruction families to register reads/writes,
storage-width/reference categories, signed literals, pool references and throwing
effects, following pinned `InsnDecoder.java`. Branch and payload references retain
DEX code-unit offsets. Wide pairs are validated together; register identity and
duplicate invoke arguments are preserved. CFG and this decoder share the same
instruction-width/payload-length validation. Reserved opcodes, malformed targets,
truncation and register overflow are explicit failures.

This is an operand IR, not completed semantic/type analysis: pool indices are not
yet resolved; invoke arguments are raw register words until signatures group wide
parameters; call results have not been linked to move-result instructions. Switch
payloads remain in the underlying code item with validated references. `Bits32`
and `Wide64` describe storage, not inferred Java primitive types. The current
Java renderer is not changed by this increment.

Resource limits bound code units and cumulative decoded register operands to one
million each. The operand limit is necessary because a short sequence of range
calls can expand into over a million argument operands. Tests verify the limit,
signed constant bits, wide comparisons/shifts/conversions, check-cast definitions,
array/field effects, compact/range/polymorphic calls, method-handle/type writes,
branch/payload boundaries and malformed register accesses.

Validation: 343 regular tests passed, 6 opt-in tests ignored; strict all-target
Clippy, formatting and release build passed. The new audit reports instruction
decoding acceptance independently from CFG and dominator acceptance. Exception
edges in the current CFG remain conservative; the decoder's throwing information
is available for later block/SSA integration and is not yet used to prune them.

Full corpus instruction audits completed without rejections:

| Input | Methods decoded | Instructions decoded |
| --- | ---: | ---: |
| Zoom | 706,072 | 7,932,272 |
| Expedia | 1,175,337 | 17,419,953 |

Evidence: `target/validation/jadx-ir-{zoom,expedia,validation}.json`, including
freshly verified APK and release-binary hashes. Graph and dominator audits also
still report zero failures. These are decoder acceptance counts, not Java
reconstruction or semantic-equivalence coverage.

Method-signature and call-result binding is implemented in the next increment;
remaining block transformations and loop analysis are still open within stage 1.

### Fourth increment: signature-aware call binding

`src/native_calls.rs` resolves method pool references and effective prototypes,
separates instance receivers, groups contiguous wide argument words, and attaches
typed adjacent move-result destinations. Static, virtual, direct, super, interface,
polymorphic and filled-array producers are supported. Array owners such as
`[I.clone()` are valid. Nonvoid results may be discarded. Invalid arity, wide pairs,
pool references/descriptors, orphan/wrong-kind/void results and control-flow entries
into move-result are explicit failures. Argument expansion remains bounded.

This pass does not resolve runtime virtual dispatch, perform SSA or replace the
existing Java renderer. Invoke-custom remains explicitly unsupported because
native call-site metadata is not yet retained. The corpus audit reports binding
acceptance separately from decoding and graph analysis.

Full corpus audits accepted signature/result binding for all 706,072 Zoom and
1,175,337 Expedia methods, with zero rejections. They bound 2,075,849 and 4,672,463
call/array producers respectively. Evidence is in
`target/validation/jadx-calls-{zoom,expedia}.json`. These are analysis counts;
they do not establish semantic Java reconstruction coverage.

### User-visible annotations and usages correction

The parser now retains shared annotation items/sets/directories, charging memory
before allocation. Class-only annotations allocate no per-member slots; parameter
slots exist only where annotated. Java output displays build/runtime annotations
on classes, fields, methods and parameters, including JavascriptInterface markers
on methods with DEX fallback bodies. Type/enum links and declaration offsets are
preserved. System annotations remain metadata (Throws still emits a throws clause);
unsupported Java values receive explicit comments. Adjacent fields no longer
have an extra blank line.

Find usages previously accumulated the source cost of every scanned class, even
when no hits retained that source. It now returns no source document for a class
without occurrences, and charges the 32 MiB budget only for actual result documents.
The retained-source and 1,000-result limits still apply. A streaming regression
passes 40 MiB of nonmatching source before finding a later match; separate tests
preserve the real retention cap and cancellation behavior. Reference completeness
is still limited to the native renderer's available symbol mappings.

### Fifth increment: register SSA and call-value constraints

`src/native_ssa.rs` adapts the pinned `SSATransform` sequence of live-in-pruned
iterated dominance-frontier phi placement and dominator-tree renaming. Every
assignment gets a stable word identity. Parameter and undefined local inputs are
explicit; entry-loop phis include the initial input. Wide operands preserve both
word identities, including overlapping moves and partial overwrites. These are
not yet inferred Java variables or proof of valid wide-value coherence.

Protected writes commit in synthetic normal-success blocks so exceptional edges
retain the pre-write value. Definitions expose their actual normalized block ID;
original PC-to-block mappings remain attached to the read/throw block. Unreachable
instructions are omitted explicitly. Storage and work are bounded, including
input graph and operand expansion; dominator analysis has its own separate bound.

`src/native_call_values.rs` attaches signature receiver, argument and result types
to those SSA identities. Repeated arguments retain the same reaching value;
move-result writes get a new identity even when they reuse the receiver register.
Calls omitted by reachability are counted separately. This connects signature
binding to SSA rather than leaving the two stages independent.

Remaining work in this stage includes phi simplification, typed parameter seeds,
wide-value coherence checks, move/constructor analysis and SSA-based type inference.
Region construction and Java-emitter integration remain required before this
pipeline replaces the current Java generator. The full-Java coverage objective
is still open; analysis acceptance is not substituted for it.

Validation: all 1,881,409 concrete methods in the two APKs pass SSA construction
and call-value linking. Reports: `target/validation/jadx-ssa-{zoom,expedia}.json`;
input/release identity and totals: `jadx-ssa-validation.json`. The full regular
suite passes 370 tests, with 7 opt-in tests ignored. Strict all-target Clippy,
formatting and release builds pass. Refreshed Java renderer counts remain exactly
the baseline at the top of this document; no emitter improvement is claimed.

### Sixth increment: type bounds and constructor identity

`native_types.rs` seeds signature, literal, return, field, allocation/cast,
arithmetic, handler and call bounds. Assignment alternatives flow forward across
moves/phis; use requirements flow backwards. The pass retains raw constant bits
and ambiguous null/boolean/numeric uses rather than inventing one Java type.
Unknown producers remain unknown through joins. Unused initial undefined slots
are excluded from active-value counters. Proven incoming reference supertypes
can resolve a join without changing the incoming values' types.

Wide operands are checked against parameter boundaries and assignment/phi origins,
including grouped call arguments. Partial overwrites remain explicit issues.
Actual exception handler types seed move-exception. Resource limits bound storage,
work and retained alternatives. Missing hierarchy, dynamic array components,
boolean bitwise results, general reference joins and conversion insertion remain
unresolved. A resolved value count is not a semantic Java coverage measurement.

`native_constructors.rs` follows move/phi receiver identities, checks successful
allocation dominance and distinguishes new-object, this and super calls. It keeps
allocation PCs and original invoke owners. For the observed f/g-to-parent-a
constructor pattern in IntegrationActivity, proven hierarchy allows an explicit
owner-retarget marker, matching the relevant JADX transformation's intent. Missing
or disproven relationships remain unresolved. Initialization-state verification,
exactly-once initialization and safe expression/statement rewriting remain open.
The analysis itself leaves instruction order unchanged. SSA now prunes conservative
handler edges from instructions marked non-throwing by the DEX decoder, avoiding
impossible register/type joins. Real throwing instructions retain pre-write
exception state and synthetic normal-success definitions.

The CLI analysis audit reports both passes separately, with bounded unresolved and
conflict reasons and samples. Java emission still uses the existing renderer;
SSA-to-region and region-to-Java integration remain required for the 100% target.

Ancestor constructor calls on `this` are also classified as super calls when the
hierarchy proves the relationship, matching pinned `ConstructorInsn` behavior.
Non-direct calls keep their actual owner and the explicit retarget marker. The
Zoom `ac0.a0 -> ac0.d -> ac0.a` sample invokes Object directly and exercises this
path. Identity classification still does not authorize an emitter rewrite.

Corpus-led refinements include numeric widening of a character/negative-integer
phi, verified Serializable/Iterable platform edges, and separate unresolved
reference-conversion requirements. A failed assignment relationship is not
silently turned into a resolved Java type. These refinements preserve original
bounds for the future cast/conversion insertion pass.

Final validation: 402 regular tests pass (7 opt-in ignored), as do strict all-target
Clippy, formatting and release builds. Both full APKs complete type and constructor
analysis without stage failures, type-conflict reports, wide-pair issues or
unresolved constructor origins. There remain 1,492,321 active unresolved SSA word
values. Final evidence and binary/input identities are in
`target/validation/types-constructors-validation.json`. Region/emitter integration
and the unresolved inference cases remain open; no 100% Java-output claim is made.

### Seventh increment: array listeners and first SSA-backed emission

Following pinned `TypeUpdate` array listeners, array bounds now propagate load
component assignments and store requirements through the bounded worklist.
Reference stores preserve runtime array-store checks. Unknown/truncated joins
remain unresolved; backwards array inference remains open.

The nested allocation path now consumes cached SSA constructor bindings and
requires matching allocation PC/type, no owner-retarget requirement, and an exact
ordered effect trace. Shared captures preserve child aliases and overloaded
constructor links. Crossed lifetimes, reordered sibling allocations, moved
effects, wide captures and exception regions remain conservative fallbacks.
This is a bounded connection to the existing renderer, not full region emission.

419 regular tests pass (7 ignored), including six nested production-emitter
regressions and eleven additional array-typing regressions. Full APK scans add
104 Zoom and 276 Expedia Java methods. Current acceptance is 637,984/706,072
(90.36%) and 1,067,364/1,175,337 (90.81%), respectively. Type audits resolve 70,700
additional SSA word values, leaving 1,421,621 unresolved, with no reported type
conflicts or analysis rejections. Evidence: `array-nested-validation.json` and
associated reports under `target/validation`. None of these figures establishes
semantic equivalence or 100% Java coverage.

### Constructor-argument follow-up: reported PBX methods

Structured input casts fix the mistaken treatment of converted caller locals as
unbound names. A narrowly recognized ignored `StringBuilder.append(String)`
result keeps all receiver aliases dependent on the append effect, following the
unchained builder-use pattern recognized by pinned `SimplifyVisitor`; actual
calls are retained rather than converted to concatenation. This remains a bounded
emitter integration, not a completed simplification/region pass.

Both reported PBX methods now emit Java with tested symbol links. Current corpus
acceptance rises to Zoom 639,480/706,072 (90.57%) and Expedia 1,070,241/1,175,337
(91.06%). Full tests, strict Clippy and the opt-in APK regression pass. See
`docs/validation.md` and `target/validation/pbx-validation.json` for evidence.

### Invalid class and package names: native display aliases

The next bounded naming increment follows pinned `RenameVisitor`'s distinction
between original names and source aliases. DEX-valid names that are illegal Java
identifiers (including Android desugared `$-CC` companions) now receive stable,
injective display aliases. Original descriptors and navigation labels remain
unchanged. The mapping is shared by declarations, constructors, fields, arrays,
annotations, type operands, and imports. Reserved alias-prefix names are encoded
too, preventing collisions without a global rename pass. Malformed descriptor
structure and oversized aliases remain errors.

The checked Play Store classes improve from 26/75 to 75/75 reconstructed methods:
`abfa` 4/5 -> 5/5, `ablf` 2/5 -> 5/5, and `j$.util.function` 20/65 -> 65/65.
The separate `aaco` prefix remains 28/42 because other reconstruction restrictions
still apply. These are selected-class measurements, not fresh full-APK coverage.
Artifacts: `target/validation/type-aliases/`.

A three-run 2,000-class search check retains the same matched texts and four hits,
with zero errors. Median cold search is 0.982 s and warm search 3.67 ms, versus
0.978 s and 3.24 ms in the preceding monitor-region checkpoint. This bounded
check does not establish performance for all inputs.

Validation: 592 full-suite tests passed (32 ignored); both annotation
checks passed in a subsequent focused run, including the new original-identity
regression. All 8 opt-in Play Store APK checks, strict Clippy, formatting, and
release build passed. Installed binaries in `target/RDX.app` match release hashes
recorded in `target/validation/type-aliases/validation.json`.

### LmdOverlayService constructor captures and synchronized iteration

`onCreate` now retains check-casts whose results are overwritten before the
constructor call. These captures are explicit effects in the existing staged
allocation renderer; the ordered effect comparison still must match. The bounded
straight-line allocation scan accepts up to 256 instructions (previously 128),
with expression-node, output-size, control-flow and exception-boundary guards
unchanged. The previously accepted readable allocation timing policy still applies.

`onDestroy` now reconstructs a loop inside a proven synchronized region. A DEX
protected interval may end before a nonthrowing latch or the exact matching
monitor release. Every other throwing body instruction must remain protected;
lock stability, sole release/rethrow cleanup and external-entry checks remain.
Ordinary exception-region loops are not enabled by this change.

The reported Play Store `LmdOverlayService` improves from 3/5 to 5/5 reconstructed
methods. Regressions cover discarded casts, long-window limits, synchronized
loops, uncovered throwing calls and modeled normal/null-lock/exception releases.

Validation: 599 full-suite tests passed (33 ignored), all 8 Play Store
APK regressions passed, and strict Clippy/format/release checks passed. The bundled
viewer also includes single-click word highlighting and Search excluded-packages
Remove All (32 focused UI/word tests passed). A three-run 2,000-class search check
retains identical matched texts and no errors: median cold 0.967 s, warm 3.33 ms.
Release binaries installed into `target/RDX.app`; hashes and logs are in
`target/validation/lmd/validation.json`. These are scoped checks, not global
coverage or semantic-equivalence claims.
