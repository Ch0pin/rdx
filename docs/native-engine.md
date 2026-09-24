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
  Captured reference widening requires proven ancestry and preserves the declared
  overload. DEX check-casts in allocation arguments retain their runtime-check
  event, original order and type link.
  Ignored StringBuilder.append(String/char) results preserve receiver aliases and
  ordered mutations; arbitrary fluent methods are not assumed to return this.
  Unused/reordered effects, crossed lifetimes, live wide values and exception
  regions decline this lowering.
- Forward conditional control flow and early returns, including integer,
  boolean and reference equality tests. Typed merge locals preserve register
  values across branches. Backward address edges that cannot form a cycle may
  reuse shared tails; duplicated effects remain in mutually exclusive arms.
- Simple single-entry while/do loops with parallel loop-carried register copies;
  multiple backedges to the same header emit conditional/unconditional continues,
  copying carried values before each continue. Interior entries and overlapping
  loops remain rejected. This follows the loop/continue distinction checked against
  pinned JADX `LoopRegionMaker`, without porting its full region builder.
  guarded conditional-backedge loops can execute a one-time exit tail before a
  shared continuation. Exit-live values use separate locals from backedge-live
  values; invalid tail entries/backedges and incompatible live exit types decline.
  Forward packed/sparse switches with grouped targets, typed joins and a single
  shared continuation. Nested/overlapping loops, extra loop exits, switch/loop
  combinations and incompatible merges remain DEX. Supported long/double joins and
  loop-carried values retain register-pair ownership; wide exception snapshots
  remain unsupported. Wide computations before a try are accepted when no wide
  register state reaches its entry; wide operations in/after the try remain rejected.
  A bare void return may terminate the normal try arm even when placed just after
  the DEX protected interval; effectful continuations are not moved into the try.
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
  visible register state. Interleaved handlers are accepted only when normal flow
  bypasses them and no unprotected effects move inside the try; unsupported
  exception control flow remains DEX.
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
same searchable text or analysis coverage as JADX-generated Java. ARSC resource names and values are indexed by the application boundary;
full Android XML enums/flags and external framework/split resources remain unresolved.

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

Flat allocation windows now have a JADX-compatible readable fallback: when the
strict expression tree cannot preserve argument order, emit typed temporary
values in their original order and then the Java constructor expression. This
explicitly relocates the root allocation, including possible initialization,
linkage and allocation-failure timing. Every other recorded effect must retain
its original order and every capture must remain used. The fallback excludes
nested allocations, exception regions, control-flow crossings and forward/cyclic
capture dependencies. It generates no synthetic helper methods or conditional
sequencing expressions; raw links also cover the temporary declarations.

A bounded exception to the control-flow/nesting restriction handles an immediate
integer guarded-copy diamond after `new-instance`. Both inputs must already be
materialized integers; external branch entries, allocation-register uses and
side-effecting branch bodies are rejected. The selected value is emitted before
allocation. Nested constructors in this proven region may use ordered temporary
statements, with identical allocation identities and an exact comparison of all
nonallocation events (including constructor calls). As above, allocation-related
failure and initialization timing may move. Ordinary nested allocation windows
retain their strict rejection rules. Integer `add-int/2addr` over materialized
integer inputs is supported with Java's matching wrapping arithmetic.

### Readability cleanup

Generated Class locals can inline repeated class-literal uses when first use is
an adjacent direct invocation with only stable earlier arguments and a verified
static owner or stable local receiver. Reassignments, preceding effects and field
receivers reject the rewrite. Every copied literal carries its original class
navigation link. Standalone cast initializers omit their redundant outer pair of
parentheses while cast receiver grouping remains intact. Both passes are bounded
and inspect generated source only, without scanning DEX-wide type inventories.

The SessionDetailsActivity cleanup preserves unused invocation effects without
emitting unused result locals, inlines adjacent single-use values into negated
guards and direct `this` field/local assignments, and removes Object argument
casts only for proven unique static targets. Unknown or overloaded targets keep
the descriptor-preserving casts. Terminal negative guards with two returning
paths can place the short failure path first; safe reference/null selections use
conditional assignments. Numeric-promotion ambiguity retains the branch form.

Documented `PackageInstaller$SessionInfo` nesting displays as
`PackageInstaller.SessionInfo`; unproven dollar-containing type names remain
unchanged. All transformations preserve binary navigation identifiers and remap
source positions. Work bounds and cached pattern compilation limit search cost.


The renderer records its generated locals and performs up to eight bounded
cleanup passes. A single-use local can move into an immediately adjacent receiver
expression, or a boolean local into an immediately adjacent `if` condition.
Exact token counts ignore quoted strings and comments. The pass does not cross
statements, block boundaries, loop conditions, or short-circuit operands, and
keeps locals used more than once or reassigned. Constructor allocations are not
candidates. Moved expression references retain Unicode-scalar navigation spans.

Null comparisons omit the Object bridge; reference-to-reference comparisons
retain it when Java types might be unrelated. A check-cast omits the bridge only
when the input is already Object or the target type. Boolean zero comparisons
render as direct or negated conditions, while integer zero comparisons remain
numeric.

The class-view presentation pass additionally inlines adjacent first arguments
when the linked DEX signature confirms the same Java type, and adjacent cast
operands while retaining the runtime check. It does not move arguments past
earlier effects. Primary call chains omit unnecessary receiver parentheses.
Generated local and parameter names use declared types or outermost call names,
with collision checks; these are inferred display names, not recovered original
source names. Raw method symbols, definitions and navigation targets stay intact.

Directly nested guards with no else or trailing outer statements fold to ordered
short-circuit `&&`. Both precomputed boolean calls remain before the guard.
Static call owners participate in import shortening. Adjacent single-use
Object-array arguments expand only for an exact, confirmed static Object-varargs
target with no local or inherited overload ambiguity, and only for suitable
stable reference locals. Other arrays stay explicit. As with readable allocation
staging, varargs syntax can relocate array allocation/failure timing.

These presentation changes run per successfully reconstructed method, before
class import processing. Each pass updates Unicode navigation spans and source
hashes and has explicit work/input bounds.

Inherited method jumps resolve the exact parameter and return descriptor through
superclasses and parent interfaces. A unique most-specific interface declaration
is required; unrelated ambiguous declarations are not guessed. This lookup does
not rewrite the original call-site symbol used for usages.

Optimized classes with no declared constructors can use an implicit Java default
constructor when a concrete nonnested class has a unique loaded direct parent
with an accessible declared no-argument constructor and no declared throws.
That parent's constructor effects remain in the generated `new` call. Boolean
register merges consumed as byte, short or char use explicit numeric 0/1 casts,
matching DEX narrow stores without treating arbitrary integers as booleans.


A bounded nested-finally recognizer handles duplicated single-call cleanup with
stable register inputs and one enclosing typed catch. It proves exception dispatch
and normal exits against the DEX table before replacing the normal cleanup copy.
Multiple cleanup operations, changing cleanup inputs, switches, loops and mixed
monitor/typed-catch shapes remain outside this subset. Framework metadata now
includes ActivityNotFoundException and the exact IBinder.transact RemoteException
contract; arbitrary unresolved exception classes are still rejected.

### Native resource resolution

`resource_table.rs` reads resources.arsc using the structures documented by
[AOSP ResourceTypes.h](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/libs/androidfw/include/androidfw/ResourceTypes.h).
The index is built once per APK and reset on project replacement. Dense, sparse,
16-bit offsets, compact values, aliases and complex bags are decoded. Configuration
bytes are retained with a locale hint; default variants sort first. No device
configuration is assumed. A missing table leaves numbers intact; parsing errors
are visible in GUI diagnostics while DEX navigation remains available.

Java ID matches gain comments and links without changing the numeric expression.
Strings, comments and existing symbol/definition spans are excluded. Unicode
offsets and source hashes are remapped, including the cancellable search path.
Compiled XML resolution uses typed reference-attribute spans, so a literal string
that resembles an ID remains unchanged. Resource details link to table-listed
archive files and aliases; virtual documents reuse ordinary navigation history.
XML metadata and source remapping use linear passes rather than rescanning the
accumulated output for each reference.

Limits: 64 MiB table input, two million variants, 256 MiB retained table data,
and 16 MiB per resource detail. The existing XML decode limit still applies.
Unloaded framework/split resources, dynamic package remapping and styled-string
span markup are not reconstructed. Plain XML is shown unchanged.

Android-namespace integer attributes in compiled XML now use framework enum and
flag names (30 attributes), including protection levels, launch modes,
configuration changes, and soft-input modes. Unknown bits retain the complete
numeric value. Custom namespaces and string-valued attributes are unchanged.
The bundled declaration values and license are documented in
`third_party/android/README.md`.

The constructor-argument lowering pass also accepts typed `filled-new-array`
and range forms with an adjacent object result. It retains element order,
capture identity, and symbol links; invalid wide-element encodings and
uninitialized operands still fall back. The existing readable-allocation timing
policy applies. Known platform `List → Collection → Iterable` relationships
support constructor argument widening; conflicting APK definitions invalidate
that proof.

Exception reconstruction accepts shared ignored-handler return tails and
nonthrowing move/constant/branch/return continuations. Effectful instructions
outside the protected region are not pulled into its catch scope.

Up to 16 ordered, disjoint typed exception regions can be reconstructed in one
method. Each region retains its own catch scope and exception-state checks;
protected continuations crossing another region are rejected. Overlapping and
catch-all cleanup layouts still require the dedicated cleanup reconstructor.

Checked catches can use exact static-method Throws declarations from loaded
DEX classes, including cross-DEX calls. Owner, name, parameter and return types
must match, and the declared exception must have proven Throwable ancestry.
Missing or duplicate owners and duplicate signatures supply no proof. A shared
bare void-return handler can be emitted as an early return in catch, keeping
subsequent normal-path calls outside that catch scope.

Loop reconstruction accepts branches to an external bare void return without
turning them into loop breaks or running the normal continuation. Effectful
external exits remain unsupported. Invariant literal registers retain their
literal typing, so zero can still represent null inside a loop. Conditional
backward edges can use a shared earlier block only when reachability proves
that the edge is acyclic; cyclic edges retain the existing loop checks.

The viewer's Copy group includes **Copy as Frida snippet** for method symbols,
including constructors and methods shown in DEX fallback. It uses the exact
owner and parameter descriptors rather than displayed aliases. Generated code
selects the overload, logs positional arguments and non-void results, and calls
the original method once. Static initializers, fields and class-only symbols do
not enable this action. Generation only copies text; RDX does not run Frida.

X-Refs results use a separate `dex://` document to preserve exact instruction
locations even when the class already has Java output. These tabs now show an
explicit call-site-view label and **Open Java source** action; switching uses
normal navigation history, so Back returns to the previous DEX position.

Quiet float and double NaN constants are reconstructed with `Float.intBitsToFloat`
and `Double.longBitsToDouble`, retaining the original sign and payload instead of
substituting a canonical NaN. Signaling NaNs remain explicit fallbacks because
Java can quiet them during value transfer. Numeric fixtures cover both signs,
canonical and noncanonical payloads, and malformed widths. The optional
`generated_quiet_nan_returns_preserve_raw_bits_on_jvm` test compiles generated
methods and checks twelve raw-bit round trips on a host JVM.

Typed try/catch reconstruction accepts `long` and `double` operations when no
mutable wide value must be carried into a handler. Unchanged wide entry values
remain available in catch paths. Writes to either register half are checked;
handler-visible mutable wide values still fall back. JVM regression fixtures
exercise successful wide results and throwing calls, checking exact long values,
double signed zero, catch results, and call counts.
