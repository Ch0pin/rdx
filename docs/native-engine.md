# Native Rust engine status

All application runtime paths use Rust. Java fallback has been removed. The
native implementation adapts algorithms from pinned JADX sources; it does not
embed or execute JADX.

## Working

- APK and raw little-endian DEX 035/037/038/039/040 loading, multidex inventories,
  checked table/index access, MUTF-8 and legal lone-surrogate escaped representation.
- Interned metadata strings, shared symbol tables, fields/methods/prototypes,
  instruction words and code-item metadata.
- Straight-line Java reconstruction: parameters, constants, moves, integer, long and floating-point
  arithmetic, fields, calls/results, returns, constructor delegation and supported
  allocation/constructor sequences. Typed locals preserve effect order and register
  overwrites; unsupported types/instructions reject reconstruction. Bounded
  allocation regions capture supported string constants, field reads and call arguments inside
  constructor expressions, preserving allocation-before-argument effects.
  Nested allocations use SSA constructor identities, shared captures and exact
  ordered effect checks, retaining aliases and overloaded constructor links.
  Structured caller-input casts retain operand validation and linked types.
  Ignored StringBuilder.append(String) results preserve receiver aliases and
  ordered mutations; arbitrary fluent methods are not assumed to return this.
  Unused/reordered effects, crossed lifetimes, live wide values and exception
  regions decline this lowering.
- Forward conditional control flow and early returns, including integer,
  boolean and reference equality tests. Shared continuations are emitted once;
  typed merge locals preserve register values across branches.
- Simple single-entry while/do loops with parallel loop-carried register copies;
  guarded conditional-backedge loops can execute a one-time exit tail before a
  shared continuation. Exit-live values use separate locals from backedge-live
  values; invalid tail entries/backedges and incompatible live exit types decline.
  Forward packed/sparse switches with grouped targets, typed joins and a single
  shared continuation. Nested/overlapping loops, extra loop exits, switch/loop
  combinations and incompatible merges remain DEX. Supported long/double joins and
  loop-carried values retain register-pair ownership; wide exception snapshots
  remain unsupported.
- Typed array allocation, filled arrays, length/load/store, class literals,
  checked casts, instance-of and integer narrowing. Reference array stores retain
  runtime array-store checks without added component downcasts.
- Invalid Java member names receive deterministic aliases while navigation retains
  original DEX identities. Boolean values used as integers are explicitly converted;
  XOR with the literal one preserves boolean negation. Immutable entry values keep
  their literal types across exception analysis.
- Bridge and varargs method declarations retain their DEX identities. Static
  initializers render as `static { ... }` when a single terminal return can be
  omitted without changing control flow. Other initializer returns remain DEX.
- Direct superclass and same-class constructor delegation accepts parameters,
  constants, arithmetic and calls on initialized values before delegation. Java 25
  constructor prologues preserve own-field writes before `super(...)`; reading
  uninitialized `this`, escaping it or writing inherited fields remains rejected.
  RDX does not need Java installed to generate this source.
- Ordinary class `invoke-super` calls accept proven ancestors beyond the direct
  parent and retain their original method links. The receiver must be `this`;
  interface/qualified-super calls and incomplete ancestry remain unsupported.
- Encoded static field values become Java declarations with exact constants and
  symbol mappings. Field initializers with unsupported values or final-field
  reassignment remain DEX. UTF-16 string literals retain lone surrogates.
- Single-region try/catch reconstruction preserves ordered effects and handler-
  visible register state; unsupported exception control flow remains DEX.
  DEX catch regions and declared Throws metadata are decoded and displayed.
  A shared, bounded project hierarchy proves custom Throwable ancestry and
  catch ordering. Unknown type hierarchies and narrow checked catches without
  protected-call exception analysis retain DEX output.
- Bounded register liveness removes dead branch/switch/exception merge variables
  without discarding effectful instructions. Straight-line methods skip this
  extra analysis; unsupported or over-budget analysis keeps all register values.
- Complete Java class output only when all structure/methods are supported.
  Otherwise supported methods replace their DEX blocks; unsupported members
  remain explicit DEX blocks. Representable class/interface declarations use
  Java syntax, with source mappings preserved and explanatory banners removed.
- Explicit DEX disassembly for other classes: instruction widths, registers,
  literals, branch offsets, payload raw words, and resolved strings/types/members.
- Generated Java uses simple type names with consolidated imports when unambiguous.
  Conflicting/shadowed names and unresolved nested binary names remain qualified.
  String literals, comments, raw DEX and original navigation identities are preserved.
- Native search, exact available symbol links, overloaded-method/field navigation,
  usages over rendered references, Android binary XML decoding, and asset previews.
- Build/runtime annotations on classes, fields, methods and parameters, including
  JavascriptInterface on methods whose bodies remain DEX. Common Java values and
  source links are preserved; system metadata stays internal and unsupported
  annotation values receive explicit comments. Adjacent fields have no blank separator.

## Limits

This is not a complete Java decompiler port. General control-flow structuring, SSA, broad type
inference, wide exception snapshots, nested/multiple exception-region reconstruction,
full annotation rendering and debug-program decoding remain incomplete. Encoded
values and exception-handler metadata are parsed; supported values and handlers
are reconstructed. Disassembly does not provide the
same searchable text or analysis coverage as JADX-generated Java. ARSC resource
names and full Android XML enums/flags are not resolved.

Aggregate uncompressed DEX input is limited to 256 MiB and classes to 250,000.
Retained native metadata allocation charges have a 1 GiB aggregate ceiling;
temporary parsing charges have an independent 768 MiB ceiling per DEX. These are
accounting budgets, not total RSS caps.
ZIP metadata/resource-name budgets are 32 MiB each; ZIP64 APKs are rejected.
Checksum/signature verification is not implemented. Oversized/unsupported inputs
fail explicitly and cannot trigger another runtime.

The local Zoom APK now opens natively, including
`com.zipow.videobox.fragment.id`, which now shows mixed reconstructed Java and DEX, retaining resolved
WebView method references. All thirteen of its methods now reconstruct to Java, including onCreateView. Its earlier parser-memory and lone-surrogate failures
were fixed by string interning and explicit surrogate representation.

## Remaining port sequence

The authoritative pass mapping is [the native JADX port checklist](jadx-port-plan.md).
`src/native_cfg.rs` now provides an independently audited basic-block construction
stage. `src/native_dominators.rs` adds predecessor lists, immediate dominators,
dominance queries and dominance frontiers using the pinned JADX algorithm.
These stages are not yet consumed by the GUI renderer; general block
transformations, SSA, type inference and region generation remain unported.
`src/native_ir.rs` adds bounded register/operand decoding for these analysis stages.
It retains explicit read/write widths, references, literals and throwing effects.
`src/native_calls.rs` resolves method references and effective signatures, groups
wide arguments and receivers, and binds adjacent typed move-result instructions.
Call-site metadata for invoke-custom remains unsupported; full SSA and virtual
dispatch resolution are not implemented.

1. Consolidate typed instruction IR and complete remaining block transformations.
2. Port SSA, constructor analysis and type inference with correctness fixtures.
3. Port regions (including exception/finally structure), Java emission and mappings.
4. Expand metadata, resources and further native Rust engines.

Preserve [upstream notices and source mappings](../third_party/jadx/README.md) as
each algorithm is ported. Build and test with Cargo; no Java SDK is required.

Forward-flow reconstruction validates every decoded instruction boundary before
following targets. Switch payloads are bounds/alignment/target checked and cannot
be executable; unreachable payload alignment padding is permitted. It rejects other
unreachable instruction regions and limits analysis
to 128 branches, nesting depth 32, 1,000,000 graph work steps and 4 MiB method output. These
are conservative fallback limits, not claims of full DEX control-flow coverage.
