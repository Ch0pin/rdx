# JADX → RDX decompilation gap inventory

**Canonical working backlog — audited 2026-09-25.** Resume **G01-C-posttest-body-composition** below; G01-A and the completed G01-B increments are listed with evidence below. Every implementation batch must name its gap IDs and update this inventory; a reported failing method becomes a regression fixture under its gap, not a separate untracked feature.

This inventory covers **63 transformation positions: 11 pre-decompile and 52 AUTO/RESTRUCTURE passes**, with 62 unique visitor classes. `CodeShrinkVisitor` intentionally occurs twice. All names, order and eight conditional gates were checked against [pinned JADX 1.5.6 Jadx.java](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/Jadx.java). Visitor sources were inspected alongside RDX source. [Machine-readable records](jadx-gap-inventory.json) retain upstream paths, annotation ordering metadata, status, gap, work unit and closure state.

**No full visitor is certified complete.** Partial means an algorithm subset or incomplete integration; Custom means related RDX behavior, not upstream equivalence; Missing means no dedicated equivalent identified. These are source-audit assessments. `closed: false` means acceptance evidence is still required, even where useful behavior already works. Upstream visitor order is separate from the proposed implementation delivery order below.

## Progress metrics

As of the current audit, **40/63 pass positions (63.5%) have some implementation**:
11 Partial and 29 Custom; 23 are Missing. This measures implementation presence,
not completed work. **0/63 complete visitor positions and 0/12 delivery units are
closed**; bounded increments are tracked separately below. These counts include
the two intentional CodeShrink positions and exclude the separately listed
supporting layers. There is no defensible effort-weighted whole-process completion
percentage yet. Do not substitute the pinned APK reconstruction rate for that
percentage or assign arbitrary half-credit to partial passes.

## The architectural gap

[`MethodAnalysis`](../src/native_method.rs) builds decoded IR, CFG, SSA and bound calls. Production constructor analysis uses it through `Graph::constructor_binding`; ordinary calls also use shared `bind_invocation`. Most Java control flow still uses a separate mutable-register [`Graph` renderer](../src/native_java/method.rs). [`InferredTypes`](../src/native_types.rs) is optional analysis, not a general emitter input. Existing analysis tests therefore do not establish that these invariants govern displayed Java.

The migration must integrate these foundations into production incrementally and preserve working output. Adding more analysis modules or deleting fallback guards does not close this gap.

## Delivery queue and acceptance gates

All units are open. “Blocked” means a planned dependency, not an external blocker. The listed tests are existing starting points, not sufficient proof of the remaining behavior. New positive, negative and semantic fixtures are required for each missing behavior. G01 is the next implementation unit; later work can be delegated once its inputs/interfaces are stable.

| Unit | Deliverable | Depends on | Completion evidence required | Existing fixture starting points |
| --- | --- | --- | --- | --- |
| **G01** (In progress) | Canonical instructions, calls and CFG | None | Migrate a bounded straight-line emission path to MethodAnalysis first, then branches; retain the existing path until parity is demonstrated. Validate operand boundaries, wide calls, exceptional edges, source offsets and hot-path performance. | [native_ir](../tests/native_ir.rs), [native_cfg](../tests/native_cfg.rs), [native_calls](../tests/native_calls.rs), [native_dominators](../tests/native_dominators.rs) |
| **G02** (Blocked) | SSA variables and typed emission | G01 | Make typed SSA values govern expression construction and Java locals. Validate branch/loop phis, register reuse, handler pre-write state, null/boolean/int ambiguity, arrays and wide words; compile and execute representative output. | [native_ssa](../tests/native_ssa.rs), [native_types](../tests/native_types.rs), [native_loop_header_liveout](../tests/native_loop_header_liveout.rs) |
| **G03** (Blocked) | Constructor and instruction normalization | G02 | Validate aliasing, this/super delegation, uninitialized values, argument captures and allocation/exception timing; integrate in typed expression emission. | [native_constructors](../tests/native_constructors.rs), [native_allocation_lowering](../tests/native_allocation_lowering.rs), [native_nested_allocations](../tests/native_nested_allocations.rs) |
| **G04** (Blocked) | Effect-aware expressions and shrinking | G03 | Validate move/constant substitution, arrays, boxing and simplification with ordered side-effect traces, throwing operands and numeric edge cases. Exercise both shrink positions. | [native_region_effects](../tests/native_region_effects.rs), [native_numeric_invariants](../tests/native_numeric_invariants.rs), [native_arrays](../tests/native_arrays.rs) |
| **G05** (Blocked) | Exception, finally and monitor regions | G04 | Normalize nested/shared handlers and cleanup ownership; verify cleanup exactly once on normal/exceptional exits, rethrows, nested monitors and checked exceptions. | [native_exceptions](../tests/native_exceptions.rs), [native_nested_cleanup](../tests/native_nested_cleanup.rs), [native_synchronized](../tests/native_synchronized.rs) |
| **G06** (Blocked) | General region construction and verification | G05 | Use shared region IR for if/switch/loops and an independent edge verifier. Test multiple latches, backward/shared exits, fall-through, string hash collisions, enum dispatch and nested labeled exits. | [native_control_flow](../tests/native_control_flow.rs), [native_multiple_latches](../tests/native_multiple_latches.rs), [native_shared_switch_tails](../tests/native_shared_switch_tails.rs), [native_backward_loop_exit](../tests/native_backward_loop_exit.rs) |
| **G07** (Blocked) | Class-level transformations | G06 | Validate anonymous/captured classes, enum state, field initialization, synthetic members and method inlining with cross-method usage invalidation, class initialization order and object identity checks. | [native_enums](../tests/native_enums.rs), [native_initializers](../tests/native_initializers.rs), [native_fields](../tests/native_fields.rs), [native_xrefs](../tests/native_xrefs.rs) |
| **G08** (Blocked) | Generics, overrides, names and access | G07 | Parse generic signatures and resolve hierarchy families; compile overload, bridge, shadowing, varargs and access fixtures; preserve original symbol/navigation identity. | [native_hierarchy](../tests/native_hierarchy.rs), [native_override_throws](../tests/native_override_throws.rs), [native_annotations](../tests/native_annotations.rs) |
| **G09** (Blocked) | Debug, Kotlin and naming metadata | G08 | Validate DEX debug programs and metadata, apply scopes/names only to surviving values, and test Kotlin supported-pattern boundaries and comment/span preservation. | [native_dex_metadata](../tests/native_dex_metadata.rs), [native_branch_navigation](../tests/native_branch_navigation.rs) |
| **G10** (Blocked) | Constants and resource transformations | G09 | Validate framework/app constant identity, ambiguities, non-final R fields and switch legality; retain original numeric meaning and resource navigation. | [resource_resolution](../tests/resource_resolution.rs), [native_resources](../tests/native_resources.rs) |
| **G11** (Blocked) | Code generation and recovery gate | G10 | Require legal typed regions and stable spans before source publication. Verify per-method recovery, cancellation, deterministic output, Java compilation and navigation after every presentation rewrite. | [native_java](../tests/native_java.rs), [native_pipeline](../tests/native_pipeline.rs), [native_branch_navigation](../tests/native_branch_navigation.rs) |
| **G12** (Blocked) | Optional deobfuscation and mapping parity | G11 | Implement and test configurable name inference, source-file names and mapping round-trips without altering original DEX identities. Optional behavior remains open until tested or explicitly excluded. | [native_annotations](../tests/native_annotations.rs) |

### Completed increment: G01-A

**Complete for the bounded scope; G01 remains open.** Tracked gaps: M01, M05,
M06. Eligible straight-line Java emission now consumes shared instruction
boundaries/opcodes/widths and ordinary invocation reads, signatures and result
ownership from `MethodFrontEnd`; full `MethodAnalysis` uses the same front end.
Non-invocation operand lowering, shared CFG and general SSA/type-driven emission
remain open. Constructors involving new-instance, filled/payload arrays, branches,
monitors and handlers keep their established paths.

[Validation and precise eligibility](validation/g01a-shared-front-end.md):
765 tests passed, zero failed; two selected JVM tests passed; Clippy/format passed.
Pinned corpus reconstruction and all diagnostic counts/samples are unchanged.
Search sample results and navigation coordinates match exactly. This does not
complete G01 or certify any full visitor.

### G01-B: shared non-invocation operands

**Complete for currently supported eligible straight-line operands.** G01-B-start is complete: all nine move encodings and eight numeric
constant encodings now consume shared decoded operands in the eligible straight-line
renderer. Tracked gaps: M01, M06. [Evidence](validation/g01b-move-constants.md):
769 default tests passed, zero failed; new JVM test passed 80 behavioral assertions;
strict Clippy/format passed. Entire pinned corpus report and search coordinates
are unchanged. Local cold-search median changed +1.0%; warm median was unchanged
within the small sample's variation. No full visitor or delivery unit closes.

**G01-B-reference-constants is complete:** const-string, const-string/jumbo and
const-class now use shared destinations and pool references. [Evidence](validation/g01b-reference-constants.md):
775 default tests passed, zero failed; the new JVM test passed six behavioral checks.
Independent Astra review and parent validation passed. Pinned corpus report and
search coordinates match; local cold-search median changed +2.1%. Existing inline
string emission remains; first-resolution/missing-class failure timing is not proven.

**G01-B-results-returns is complete:** shared result destinations and return operands
retain call ownership, type/width and constructor guards. [Evidence](validation/g01b-results-returns.md):
781 default tests passed, zero failed; new JVM test passed; independent Sol and
parent review passed. Corpus report and search offsets match. Local cold-search
median changed +3.2% (~7.4 ms over 300 classes); warm median changed +0.054 ms.

**G01-B-arithmetic is complete:** all 109 comparison, unary/conversion, normal
binary, 2addr and literal arithmetic encodings consume shared operands, including
boolean fast paths. [Evidence](validation/g01b-arithmetic.md): 785 default tests
passed, zero failed; JVM passed 11,728 boundary combinations; Astra and parent
review passed. Entire corpus report and search coordinates match. Local cold-search
median changed -1.7%; this is operand migration, not expanded fallback coverage.

**G01-B-fields is complete:** all 28 instance/static field encodings consume
shared receiver/value registers and pool identity. [Evidence](validation/g01b-fields.md):
792 default tests passed, zero failed; new JVM test passed; Astra and parent review
passed. Corpus report and search coordinates match. Local cold-search median
changed +2.6% (5.9 ms); warm median was effectively unchanged. No fallback expansion.

**G01-B-array-types is complete:** 18 cast/type-test, array-length, new-array
and array get/put encodings consume normalized shared operands and type references.
[Evidence](validation/g01b-array-types.md): 799 default tests passed, zero failed;
JVM and focused post-lint tests passed; Astra and parent review passed. Corpus
report and search coordinates match. Local cold-search median changed -1.5%;
warm median changed +0.029 ms. Existing semantic/fallback boundaries remain.

**G01-B-throw is complete:** shared throw operands retain null, exception,
constructor and terminal checks. [Evidence](validation/g01b-throw.md): 805 default
tests passed, zero failed; JVM passed 23 exception outcomes and five effect checks;
Astra and parent review passed. Corpus report and search coordinates match; local
cold-search median changed +0.36% (0.800 ms).

**Bounded G01-B is complete.** All successfully emitted non-call operand families
in the current eligible shared straight-line path consume decoded operands. Raw
NOP validity and eligibility scans remain; excluded legacy branches, handlers,
monitors, new-instance and filled/payload arrays are not covered by this closure.
Rejected modern opcodes are not newly supported. G01 remains open.

### G01-C-start: decoded forward conditional CFG

**Complete for one eligible forward conditional** (0x32..0x3d, optional
forward goto 0x28..0x2a), including shared-tail, bypass and terminal-arm shapes.
Tracked gaps: M01, M07, M06. [Evidence](validation/g01c-start.md).
Blocks and successors are built from shared decoded instructions; canonical
successors, join and decoded condition/goto operands govern production rendering.
Decoded join-tail liveness preserves merged values without raw operand analysis.
Poisoned raw branches and legacy targets cannot alter output; malformed canonical
edges/metadata reject without retrying raw rendering. This does not close G01 or
any full visitor. Constructors, nested/sequential conditions, loops, switches,
payloads, handlers, monitors and allocation-specific shapes retain legacy routing.

### G01-C-forward-composition: decoded forward regions

**Complete for eligible sequential/nested forward conditionals.** Tracked gaps:
M01, M07, M09, M06. [Evidence](validation/g01c-forward-composition.md).
Per-branch plans consume canonical successors, reverse-DAG postdominators and
terminal-aware region closure. Existing single-condition source layouts remain
stable. Decoded block live-in bitsets govern merges, including wide/reference
values and dead incompatible registers; budget exhaustion conservatively keeps
registers live. Poisoned raw branches/legacy targets preserve code and nonempty
navigation links; inconsistent plans/edges fail closed. Constructors, backedges,
switches/payloads, handlers/monitors and allocation-specific paths remain legacy.
This advances G01 without closing a whole visitor or delivery unit.

### Completed increment: G01-C-natural-loop

**Complete for the bounded pretest-loop scope.** Tracked gaps: M01, M07, M08,
M09, M06. [Evidence](validation/g01c-natural-loop.md). One forward guard, one
unconditional backward latch, a straight-line body and sole guard exit now use
canonical loop metadata, dominance and decoded fixed-point liveness in production.
Raw header/latch/body poisoning preserves source, links and carried state;
malformed loop metadata and edges reject reconstruction without raw retry.

Twelve public fixtures, nine negatives and five private tests cover the selected
path. Independent JVM validation checks 672 outcomes, including 384 effect/failure
scenarios. The header-defined null exit regression now compiles and executes.
Full suite: 826 passed, zero failed, 87 ignored; three selected optional JVM tests
also passed. Astra reviewed production/private tests/oracle; strict Clippy,
formatting and release build passed. Corpus report and search coordinates are
unchanged. Broader loop/exception integration remains open; no full visitor or
unit is closed by this increment.

### Completed increment: G01-C-loop-body-composition

**Complete for the bounded forward-body scope.** Tracked gaps: M01, M07, M08,
M09, M06. [Evidence](validation/g01c-loop-body-composition.md). Sequential/nested
body conditionals now consume canonical plans and revalidated joins through the
latch. A planning copy cuts the latch backedge; the full cyclic graph retains
dominance, membership and fixed-point liveness authority.

Four private tests prove selected-path ownership and rejection under corrupted
raw/canonical data. Eleven public fixtures and eight negatives passed; the new
JVM oracle checks 3,006 outcomes including 2,016 effect/failure scenarios. Existing
full regression: 830 passed, zero failed, 87 ignored; the new target passed
separately. The combined focused run passed thirteen tests including four JVM
tests (overlapping counts, not additive). Independent review, strict Clippy,
formatting and release build passed. Corpus report and exact search/navigation
results remain unchanged. No whole visitor or delivery unit closes.

### Completed increment: G01-C-loop-break

**Complete for the bounded common-exit scope.** Tracked gaps: M01, M07, M08,
M09, M06. [Evidence](validation/g01c-loop-break.md). One conditional body break
uses revalidated canonical exit metadata and exit-slot synchronization. Planning
removes only its taken exit edge; full cyclic CFG liveness remains authoritative.
Selected loops reuse proven restored-literal invariants at exits; legacy behavior
is unchanged. Eight public fixtures and eight negatives, raw-data poisoning,
malformed metadata rejection and JVM effect/exception checks cover the boundary.

### Completed increment: G01-C-loop-continue

**Complete for the bounded conditional-backedge scope.** Tracked gaps: M01,
M07, M08, M09, M06. [Evidence](validation/g01c-loop-continue.md). One conditional
continue returns to the same dominating header alongside the unconditional latch
and optional common-exit break. Revalidated metadata drives decoded conditions,
actual fallthrough and snapshot header-slot synchronization. Planning cuts the
taken edge; full cyclic liveness retains both backedges. Four private tests and
ten public fixtures cover raw poison, invalid metadata, edge-dependent liveness,
restored literals, skipped effects and break/continue priority.

### Completed increment: G01-C-loop-edge-composition

**Complete for the bounded same-loop edge scope.** Tracked gaps M01, M07, M08,
M09, M06. [Evidence](validation/g01c-loop-edge-composition.md). An ordered
canonical collection replaces single optional break/continue caches. Every edge
participates in root validation, dominance, liveness and restored-literal proofs;
planning preserves actual fallthrough and snapshot synchronization. The existing
1,024-total-control budget is enforced at selection and canonical derivation.
Five private tests and eight finite public fixtures cover poison/metadata faults,
per-edge live/invariant proofs, competing edges, identity and exceptions.

### Completed increment: G01-C-loop-outer-composition

**Complete for bounded outer-loop composition.** Tracked gaps M01, M07, M08,
M09, M06. [Evidence](validation/g01c-loop-outer-composition.md). Acyclic forward
regions can precede, follow or bypass one canonical loop. Atomic outer planning
retains full cyclic dominance/liveness and body plans; external body/latch entries
and malformed joins reject without raw retry. Two private tests, ten public
fixtures and nine malformed cases cover ownership, skipped/taken paths, terminal
bypasses, wide/reference merges and effects. JVM oracle: 311,040 outcomes.
Full suite: 855 passed; pinned APK gains two reconstructed methods.

### Completed increment: G01-C-disjoint-loop-composition

**Complete for bounded disjoint-loop composition.** Tracked gaps M01, M07, M08,
M09, M06. [Evidence](validation/g01c-disjoint-loop-composition.md). Up to 32
nonoverlapping canonical pretest loops now share decoded planning with per-loop
edge ownership, full cyclic liveness and bounded analysis. Sequential, alternative
and bypassed loops preserve cross-loop values. Three private tests, fourteen
public fixtures and twelve malformed cases cover selection, raw poisoning,
adjacent headers, ownership rejection and the 32/33 boundary. JVM oracle:
131,328 outcomes. Full suite: 860 passed. Pinned APK gains one reconstruction.

### Completed increment: G01-C-nested-loop-composition

**Complete for bounded nested pretest loops.** Tracked gaps M01, M07, M08,
M09, M06. [Evidence](validation/g01c-nested-loop-composition.md). Up to four
parent-owned levels share atomic child planning and original cyclic proofs,
within 32 loops and 1,024 controls. Owner-local breaks/continues and carried
values are tested. Cross-owner exits and unsupported shapes remain outside scope.
The JVM oracle passed 198,801 outcomes; corpus acceptance is unchanged with one
new reconstruction and one previously invalid Java method now rejected.

### Completed increment: G01-C-posttest-loop

**Complete for one straight-line posttest loop.** Tracked gaps M01, M07, M08,
M09, M06. [Evidence](validation/g01c-posttest-loop.md). Canonical dominance and
membership govern the conditional latch; the body executes before the first
condition check. Distinct exit values and condition-before-copy ordering preserve
body-only liveouts and aliases. The independent JVM oracle passed 723 outcomes,
including 384 effect/failure scenarios. The pinned APK gains eight reconstructions.
Additional controls, exits and nested compositions retain established handling.

### Next increment: G01-C-posttest-body-composition

Add bounded forward diamonds inside one canonical posttest loop. Cut only its
latch backedge for branch planning while retaining full cyclic dominance and
liveness. Preserve the sole exit; defer extra break/continue edges and outer or
nested compositions. Validate branch-dependent exit/carried values, mandatory
first iteration, effects/exceptions and malformed ownership. Run the same full
regression, lint/build, corpus and search gates. G01 remains open.

### Rule for closing a gap

1. Record the upstream algorithm/source, supported cases and explicit remaining cases for the affected IDs.
2. Integrate the implementation into normal Java rendering; an unused analysis pass cannot qualify.
3. Pass structural/negative fixtures and compile/run semantic fixtures covering returns, exceptions, effects and identity where relevant.
4. Run the full regression suite, strict Clippy, formatting and pinned-corpus comparison; investigate regressions and render/search performance changes.
5. Link evidence, commit and remaining limitations here. Close a visitor only when all its listed behavior and gate are met. A delivery increment can finish while the visitor stays open.

## Ordered upstream pass inventory

Each row links its exact upstream implementation. Its remaining behavior plus its delivery-unit acceptance gate is the closure criterion. No row is closed merely because a named file or a matching example exists.

### Pre-decompile

| ID / upstream pass | Purpose | State / delivery unit | RDX evidence and integration | Remaining behavior / acceptance target |
| --- | --- | --- | --- | --- |
| P01 [SignatureProcessor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/SignatureProcessor.java) | Parse generic signatures | Missing; G08; open | No equivalent dedicated pass identified in the source audit. | Parse and propagate class, field and method generic signatures; validate malformed and erased signatures. |
| P02 [OverrideMethodVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/OverrideMethodVisitor.java) | Resolve override families | Custom; G08; open | native_hierarchy and native_java/mod.rs inherited exception lookup; no general override pass | Resolve bridges, covariant returns and generic override families; preserve checked-exception contracts. |
| P03 [AddAndroidConstants](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/prepare/AddAndroidConstants.java) | Provide Android constants | Missing; G10; open | src/resource_table.rs and src/native_resource_attributes.rs provide related resources, not a general constants pass | Resolve framework constants with type/context and collision checks; resource annotations alone are insufficient. |
| P04 [DeobfuscatorVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/deobf/DeobfuscatorVisitor.java) | Infer deobfuscated names | Missing; G12; open | No equivalent dedicated pass identified in the source audit. | Define deterministic configurable inferred names and mapping precedence across classes and members. |
| P05 [SourceFileRename](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/rename/SourceFileRename.java) | Use source-file names | Missing; G12; open | No equivalent dedicated pass identified in the source audit. | Recover eligible names from source metadata without collisions or broken identity links. |
| P06 [RenameVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/rename/RenameVisitor.java) | Make identifiers legal and unique | Custom; G08; open | src/native_java/names.rs::member/qualified; class/member emission uses injective aliases; tests/native_annotations.rs and names unit tests | Extend injective display aliases to global override-family and cross-class naming consistency. |
| P07 [SaveDeobfMapping](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/deobf/SaveDeobfMapping.java) | Persist name mappings | Missing; G12; open | No equivalent dedicated pass identified in the source audit. | Round-trip mappings with original DEX identity and stable output across sessions. |
| P08 [UsageInfoVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/usage/UsageInfoVisitor.java) | Build usage relationships | Custom; G07; open | native usage indexing; separate implementation, not upstream usage graph parity | Expose transformation-safe usage dependencies and invalidate them after rewrites; navigation indexing alone is insufficient. |
| P09 [CollectConstValues](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/prepare/CollectConstValues.java) | Collect global constants | Missing; G10; open | src/native_dex_metadata.rs::static_values and native_java/mod.rs::render_field render encoded values, not global collection | Collect and substitute constants with owner/type/ambiguity and initialization-order safeguards. |
| P10 [ProcessAnonymous](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ProcessAnonymous.java) | Identify anonymous classes | Missing; G07; open | No equivalent dedicated pass identified in the source audit. | Discover candidates and captures while rejecting classes with observable identity or incompatible uses. |
| P11 [ProcessMethodsForInline](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ProcessMethodsForInline.java) | Identify inline candidates | Missing; G07; open | No equivalent dedicated pass identified in the source audit. | Build method candidate/dependency analysis, including recursion and access constraints. |

### AUTO / RESTRUCTURE

| ID / upstream pass | Purpose | State / delivery unit | RDX evidence and integration | Remaining behavior / acceptance target |
| --- | --- | --- | --- | --- |
| M01 [CheckCode](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/CheckCode.java) | Validate input code | Partial; G01; open | native_ir / native_cfg validate operands and boundaries; not the full upstream checks | Make shared decoded-instruction checks govern emission; retain rejection diagnostics and offsets. |
| M02 [DebugInfoAttachVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/debuginfo/DebugInfoAttachVisitor.java)<br>Gate: `args.isDebugInfo()` | Attach debug metadata | Missing; G09; open | No equivalent dedicated pass identified in the source audit. | Decode DEX debug programs into validated line/local scopes, including malformed programs. |
| M03 [AttachTryCatchVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/AttachTryCatchVisitor.java) | Attach exception regions | Partial; G05; open | native_cfg / native_ssa retain handlers and exceptional state; no complete normalized exception regions | Normalize nested, overlapping and shared handlers using pre-write exceptional states. |
| M04 [AttachCommentsVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/AttachCommentsVisitor.java)<br>Gate: `args.getCommentsLevel() != CommentsLevel.NONE` | Attach comments | Missing; G09; open | No equivalent dedicated pass identified in the source audit. | Define comment provenance and source-span attachment; preserve it through transformations. |
| M05 [AttachMethodDetails](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/AttachMethodDetails.java) | Bind method details | Partial; G01; open | native_method::MethodFrontEnd now drives eligible straight-line invoke bindings (G01-A); other shapes use bind_invocation; SSA call values remain selectively consumed | Use one signature binding model through analysis and emission, including wide and polymorphic calls. |
| M06 [ProcessInstructionsVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ProcessInstructionsVisitor.java) | Normalize instructions | Partial; G01; open | native_ir operand decoding; G01-B integrates move, constant, move-result, return, arithmetic, field, array/type and throw operands into eligible straight-line emission; no complete semantic instruction normalization | Make semantic instruction normalization canonical for emission, without losing effects or offsets. |
| M07 [BlockSplitter](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockSplitter.java) | Split basic blocks | Partial; G01; open | native_cfg decoded blocks/successors govern eligible forward conditionals and single-loop body composition and multiple conditional break/continue edges through G01-C-loop-edge-composition; broader loop/exception and synthetic block parity incomplete Atomic outer-region projection now also supports forward regions around/bypassing one loop. G01-C-disjoint-loop-composition supports up to 32 nonoverlapping loops and their outer regions.  Nested parent ownership and atomic child planning now support four levels with full cyclic proofs.  One canonical posttest region now preserves mandatory body execution and distinct exit values. | Share block boundaries and exceptional edges with emission; handle synthetic blocks explicitly. |
| M08 [BlockProcessor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockProcessor.java) | Process block graph | Partial; G01; open | Canonical single-loop metadata/dominance, forward body plans and revalidated ordered break/continue collections govern selected loop emission; broader block transformations and loop topology remain incomplete G01-C-loop-outer-composition rederives atomic outer plans while retaining original cyclic dominance. Disjoint region collection uses one dominator computation and bounded membership work, with owner-tagged loop edges and shared projections.  Nested parent ownership and atomic child planning now support four levels with full cyclic proofs.  One canonical posttest region now preserves mandatory body execution and distinct exit values. | Complete required graph transformations and loop metadata over the canonical CFG. |
| M09 [BlockFinisher](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/blocks/BlockFinisher.java) | Finish block graph | Partial; G01; open | Canonical CFG validation, revalidated body joins/break/continue edges and decoded fixed-point loop liveness govern selected Java rendering; upstream block finishing not fully reproduced Outer ownership/joins and external body/latch entries are now revalidated against the canonical CFG. Root rederivation validates all disjoint regions/edge owners/plans; complete cyclic liveness spans successive or alternative loops.  Nested parent ownership and atomic child planning now support four levels with full cyclic proofs.  One canonical posttest region now preserves mandatory body execution and distinct exit values. | Validate reciprocal edges and normalized block invariants before downstream passes. |
| M10 [SSATransform](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ssa/SSATransform.java) | Build SSA | Partial; G02; open | src/native_ssa.rs::SsaMethod::build; production constructor analysis consumes SSA, general Java expression emission does not | Make reaching values and pruned phis drive Java expressions; add safe phi simplification. |
| M11 [MoveInlineVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/MoveInlineVisitor.java) | Eliminate moves | Custom; G02; open | native_java/cleanup and readable transformations; no general SSA move elimination | Substitute SSA moves with dominance, exceptional-use and source-mapping checks. |
| M12 [ConstructorVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ConstructorVisitor.java) | Recover constructors | Partial; G03; open | native_constructors plus bounded native_java/allocation_lowering integration | Prove initialization identity, exactly-once initialization and effect order across aliases and branches. |
| M13 [InitCodeVariables](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/InitCodeVariables.java) | Create code variables | Custom; G02; open | native_java/method register values and local names; no shared SSA code-variable stage | Group SSA values into legal Java locals without combining incompatible lifetimes or types. |
| M14 [MarkFinallyVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/finaly/MarkFinallyVisitor.java)<br>Gate: `args.isExtractFinally()` | Extract finally blocks | Custom; G05; open | src/native_java/finally_regions.rs and synchronized.rs are wired; bounded duplicate cleanup, multiple normal monitor releases and selected inner catches | Generalize cleanup extraction and monitor ownership beyond exact bounded patterns. |
| M15 [ConstInlineVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ConstInlineVisitor.java) | Inline constants | Custom; G04; open | native_java/cleanup folds selected constants/class literals; no full SSA pass | Substitute typed SSA constants without changing evaluation order or exceptional behavior. |
| M16 [TypeInferenceVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/typeinference/TypeInferenceVisitor.java) | Infer types | Partial; G02; open | src/native_types.rs::InferredTypes::infer; opt-in native_method analysis and coverage audit, no general production Java-emitter consumer | Integrate inferred types into emission; close hierarchy joins, arrays, narrowing and wide coherence gaps. |
| M17 [DebugInfoApplyVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/debuginfo/DebugInfoApplyVisitor.java)<br>Gate: `args.isDebugInfo()` | Apply debug metadata | Missing; G09; open | No equivalent dedicated pass identified in the source audit. | Apply validated names/scopes to surviving values without stale aliases or collisions. |
| M18 [FixTypesVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/typeinference/FixTypesVisitor.java) | Repair types | Custom; G02; open | native_java casts and conversions handle selected patterns; no complete typed-IR repair | Insert required casts/conversions in typed IR; validate assignments and overloads. |
| M19 [FinishTypeInference](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/typeinference/FinishTypeInference.java) | Validate final types | Partial; G02; open | analysis diagnostics exist; no complete final typed-IR gate before Java emission | Require a final typed-IR legality gate; unresolved cases must remain explicit fallbacks. |
| M20 [AdjustForIfMergeVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/AdjustForIfMergeVisitor.java) | Prepare condition merging | Custom; G04; open | native_java/condition_cleanup handles selected guard/ternary shapes | Merge CFG conditions only when edge, short-circuit and effect-order proofs hold. |
| M21 [ProcessKotlinInternals](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/kotlin/ProcessKotlinInternals.java)<br>Gate: `args.getUseKotlinMethodsForVarNames() != JadxArgs.UseKotlinMethodsForVarNames.DISABLE` | Recover Kotlin internals | Missing; G09; open | No equivalent dedicated pass identified in the source audit. | Identify supported Kotlin patterns and metadata with explicit unsupported boundaries. |
| M22 [CodeRenameVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/rename/CodeRenameVisitor.java) | Rename code entities | Custom; G08; open | native_java/names and display_names; local naming rather than global rename parity | Keep declaration/use names consistent across scopes, captures and override families. |
| M23 [InlineMethods](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/InlineMethods.java)<br>Gate: `args.isInlineMethods()` | Inline methods | Missing; G07; open | No equivalent dedicated pass identified in the source audit. | Inline eligible bodies with receiver/argument evaluation, access, recursion and exception safety. |
| M24 [GenericTypesVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/GenericTypesVisitor.java) | Infer generic uses | Missing; G08; open | No equivalent dedicated pass identified in the source audit. | Infer generic call-site and expression types from parsed signatures and hierarchy constraints. |
| M25 [ShadowFieldVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ShadowFieldVisitor.java) | Resolve shadowed fields | Missing; G08; open | No equivalent dedicated pass identified in the source audit. | Qualify or rename fields across inheritance/captures without changing binding. |
| M26 [DeboxingVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/DeboxingVisitor.java) | Remove redundant boxing | Missing; G04; open | No equivalent dedicated pass identified in the source audit. | Prove boxing/unboxing removal preserves null exceptions, identity and overload selection. |
| M27 [AnonymousClassVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/AnonymousClassVisitor.java) | Recover anonymous expressions | Missing; G07; open | No equivalent dedicated pass identified in the source audit. | Lower proven anonymous classes and captures into expressions while preserving initialization order. |
| M28 [ModVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ModVisitor.java) | Normalize instruction patterns | Custom; G03; open | native_java method/allocation special cases; no equivalent general IR pass | Extend typed normalization beyond selected allocation and method special cases. |
| M29 [CodeShrinkVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/shrink/CodeShrinkVisitor.java) | Shrink expressions before regions | Custom; G04; open | native_java/cleanup and readable; bounded expression/text cleanup | Perform effect-aware SSA substitution and dead-value removal, not only textual cleanup. |
| M30 [ReplaceNewArray](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ReplaceNewArray.java) | Recover array initializers | Custom; G04; open | native_java array emission handles selected initializers | Fold allocation/store chains with alias, escape, exception and store-order checks. |
| M31 [RegionMakerVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/RegionMakerVisitor.java) | Build structured regions | Custom; G06; open | native_java/method recursive Graph renderer; no shared region IR | Create common region IR over typed CFG; account for every executable edge. |
| M32 [IfRegionVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/IfRegionVisitor.java) | Simplify conditional regions | Custom; G06; open | native_java/method and condition_cleanup for supported branches | Transform shared IF regions with short-circuit, join and live-out preservation. |
| M33 [SwitchOverStringVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/SwitchOverStringVisitor.java)<br>Gate: `args.isRestoreSwitchOverString()` | Recover string switches | Missing; G06; open | No equivalent dedicated pass identified in the source audit. | Recognize hash/equals dispatch including hash collisions and null behavior. |
| M34 [ReturnVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/ReturnVisitor.java) | Normalize returns | Custom; G06; open | native_java/method terminal paths and cleanup | Normalize terminal regions without moving returns across cleanup or side effects. |
| M35 [CleanRegions](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/CleanRegions.java) | Clean region structure | Custom; G06; open | native_java/condition_cleanup; no shared region IR | Remove redundant region wrappers without losing edge ownership or source mappings. |
| M36 [MethodThrowsVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/MethodThrowsVisitor.java) | Infer throws declarations | Custom; G05; open | native_java/throwing and mod.rs checked-exception guards | Combine body/call/handler analysis with legal override-family checked exceptions. |
| M37 [CodeShrinkVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/shrink/CodeShrinkVisitor.java) | Shrink expressions after regions | Custom; G04; open | native_java/cleanup and readable; bounded expression/text cleanup | Run region-aware shrinking separately after region/throws changes and revalidate effects. |
| M38 [MethodInvokeVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/MethodInvokeVisitor.java) | Repair invocation expressions | Custom; G08; open | signature-aware calls and native_java/varargs_cleanup; incomplete overload/generic parity | Preserve overload choice, varargs, generics, receiver casts and exact evaluation order. |
| M39 [SimplifyVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/SimplifyVisitor.java) | Simplify expressions | Custom; G04; open | native_java/cleanup, condition_cleanup, readable and operations | Use typed expression rewrites with numeric overflow, NaN, null and effect safeguards. |
| M40 [CheckRegions](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/CheckRegions.java) | Verify regions | Custom; G06; open | native_java rejection guards; no independent complete region-edge verifier | Independently verify emitted region edges against CFG, including exceptions and abrupt exits. |
| M41 [EnumVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/EnumVisitor.java) | Recover enum declarations | Custom; G07; open | src/native_java/enums.rs::Plan::analyze wired by mod.rs; fieldless erased enums; tests/native_enums.rs includes JVM checks | Extend fieldless-enum recovery to state, constructor arguments and constant-specific classes. |
| M42 [FixSwitchOverEnum](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/FixSwitchOverEnum.java) | Recover enum switches | Missing; G06; open | No equivalent dedicated pass identified in the source audit. | Remove proven mapping-array dispatch and recover enum labels without changing initialization effects. |
| M43 [NonFinalResIdsVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/gradle/NonFinalResIdsVisitor.java) | Handle non-final resource IDs | Missing; G10; open | Resource annotations/navigation preserve numeric Java semantics; they do not implement non-final resource-field transformation | Handle resource field finality and legal switch use; do not replace literals with invented R fields. |
| M44 [ExtractFieldInit](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ExtractFieldInit.java) | Extract field initializers | Missing; G07; open | Encoded static values and rendering clinit blocks exist; extracting ctor/clinit assignments into field declarations does not | Move only proven equivalent ctor/clinit assignments, preserving ordering and constructor delegation. |
| M45 [FixAccessModifiers](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/fixaccessmodifiers/FixAccessModifiers.java) | Repair access modifiers | Missing; G08; open | No equivalent dedicated pass identified in the source audit. | Analyze cross-class/member access and apply minimal consistent source repairs. |
| M46 [ClassModifier](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ClassModifier.java) | Normalize classes | Custom; G07; open | native_java/mod.rs class header and modifiers | Handle synthetic/inner class structure and member cleanup with identity/use checks. |
| M47 [LoopRegionVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/LoopRegionVisitor.java) | Recover loop forms | Custom; G06; open | src/native_java/method.rs loop rendering now checks non-reentry and distinct header/exit liveness; tests/native_backward_loop_exit.rs and native_loop_header_liveout.rs | Recover general for/while/do/iterator forms, nested exits and header/exit live-outs. |
| M48 [SwitchBreakVisitor](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/SwitchBreakVisitor.java) | Normalize switch exits | Custom; G06; open | native_java/method handles selected switch exits | Handle fall-through, shared tails and labeled loop/switch exits using region ownership. |
| M49 [MarkMethodsForInline](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/MarkMethodsForInline.java)<br>Gate: `args.isInlineMethods()` | Mark inlineable methods | Missing; G07; open | No equivalent dedicated pass identified in the source audit. | Recompute safe inline eligibility after transformations; avoid stale candidates. |
| M50 [ProcessVariables](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/regions/variables/ProcessVariables.java) | Place variable declarations | Custom; G02; open | native_java/liveness and method register/local handling | Perform SSA-to-Java scope placement and phi destruction across loops and handlers. |
| M51 [ApplyVariableNames](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/ApplyVariableNames.java) | Apply final variable names | Custom; G09; open | src/native_java/display_names.rs::rename; no complete debug/Kotlin naming recovery | Combine debug/Kotlin/generated names consistently after scope and variable transformations. |
| M52 [PrepareForCodeGen](https://github.com/skylot/jadx/blob/28ff15e4ae69950aebea110a13e5ab895d234dfc/jadx-core/src/main/java/jadx/core/dex/visitors/PrepareForCodeGen.java) | Prepare final Java generation | Custom; G11; open | native_java/mod.rs and readable emission preparation; not upstream parity | Validate final typed-region legality, names, spans and Java emission before publishing source. |

## Supporting layers outside the 63 passes

These boundaries remain tracked; the visitor list is not an inventory of every JADX capability.

| ID | Layer / present RDX behavior | Remaining work / completion boundary |
| --- | --- | --- |
| S01 | APK/DEX loading: [native_engine](../src/native_engine.rs), [native_dex](../src/native_dex.rs), [metadata](../src/native_dex_metadata.rs) | Inventory accepted DEX versions, archive limits and unsupported formats; decode call-site metadata needed by invoke-custom. Validate malformed inputs and source identity. This is a prerequisite for claiming instruction coverage, independent of visitor counts. |
| S02 | Annotations, encoded values and hierarchy: [annotations](../src/native_java/annotations.rs), [hierarchy](../src/native_hierarchy.rs) | System signatures/debug metadata, incomplete dependencies and unknown hierarchy facts need explicit handling. Ordinary annotation rendering does not close generic signature processing (G08/G09). |
| S03 | Resources: [resource_table](../src/resource_table.rs), [native_resources](../src/native_resources.rs) | XML enums/flags and Java annotations/navigation work; framework constants and non-final resource transforms remain P03/M43/G10. |
| S04 | Java generation and navigation: [native_java](../src/native_java/mod.rs), [readable](../src/native_java/readable.rs) | Typed expression/region generation, stable symbol/span mappings and legality gate (G11). Compare with upstream ClassGen/MethodGen/InsnGen, beyond visitor preparation. |
| S05 | Processing lifecycle and recovery: [native_engine](../src/native_engine.rs) | Audit class dependencies, retries, caches, cancellation and isolated method failure against upstream ProcessClass/CodeGen. JADX restarts and SIMPLE/FALLBACK modes are not automatically RDX requirements; record deliberate differences and test recovery. |
| S06 | Alternate modes, diagnostics, plugins and other input backends | SIMPLE has 24 constructor-added pass entries; FALLBACK has four. Three optional DotGraph hooks are outside the 63 transformations. Detailed parity is not yet audited. RDX call graphs are not CFG/region diagnostics. Plugin-injected passes and non-DEX input parity require separate inventory before making a whole-JADX parity claim. |
| S07 | Semantic and corpus validation: [focused check](focused-semantic-check.md), [latest loop evidence](validation/jadx-loop-comparison.md) | Maintain a diverse, pinned APK/compiler fixture corpus, compile/run behavior checks and known-limitations ledger. Reconstruction rate is neither semantic accuracy nor all-Android-app coverage. |

## Measurement and progress reporting

The latest recorded Play Store corpus result is **260,225 / 268,289 concrete methods reconstructed (96.994286%), 8,064 fallbacks**; see [pinned evidence](validation/coverage-jadx-loops.json). It describes that APK and tested renderer only. No new corpus run was performed for this documentation audit.

Report two separate dimensions after each increment: **which gap behavior is now integrated and validated**, and **which pinned corpus changed by how many methods**. First-rejection reason counts overlap in underlying causes and must not be added as independent feature coverage. An improved percentage does not close a pass.

New failure reports are assigned to an existing ID or a newly documented supporting-layer gap. Work proceeds through the dependency queue; correctness regressions in already-working behavior are handled before the next increment. Do not switch to unrelated improvements or start a fresh coverage target without updating this backlog.

[Earlier implementation notes](jadx-port-plan.md) retain historical experiments and scoped ports. This file supersedes their old status tables and delivery ordering.
