# Native-only migration validation

## Latest: shared tails, interleaved catches and allocation expressions

The reported `marketnotice.f` class now reconstructs all 11 concrete methods,
including the three reported `d`, `f`, and `h7` methods. Backward address jumps
are classified using reachability before loop summarization; acyclic shared tails
retain their branch values and effects. Interleaved handlers remain separate from
normal flow, retain the original catch type, and cannot pull unprotected effects
into a protected region. The JSON checked-exception proof is limited to the exact
Android `JSONObject(String)` signature.

Allocation expressions preserve typed boolean/null arguments. Optimized no-argument
constructor retargeting requires a declared equivalent body and proven superclass
ancestry; arbitrary owner mismatches remain unsupported. Source navigation retains
the original DEX method identity. APK regressions check all three reported methods,
while independent fixtures check mutually exclusive effects, genuine cycles, and
exception boundaries.

Captured reference arguments now widen only through proven type relationships;
ignored `StringBuilder.append(char)` results preserve the actual overload and
receiver aliases. An allocation argument's DEX `check-cast` is an ordered throwing
event, enabling the reported Play Store `ClassicApplication.e` method. Independent
fixtures reject unused casts, reordered captures and unproven conversions.

The reported Play Store `SubscriptionAskToPauseActivity.onClick` and coroutine
allocation sample remain DEX: their string-resolution order cannot be represented
by the current constructor-expression tree without moving effects. These are
explicit remaining failures, not accepted Java methods.

Forward navigation now complements Back in the toolbar and Navigate menu, with
Alt+Right / Alt+Left. History retains source positions and source hashes, commits
only successful async moves, discards cancelled completions, and resets on reload.
New reference navigation clears forward history. UI tests cover cached and evicted
classes/assets, failure cleanup and manifest round-trips.

465 regular tests passed, 12 opt-in checks ignored. Reported Zoom and Play Store
APK regressions passed separately; formatting, strict Clippy and release builds
passed locally. CI compatibility fixes replace constant-size `chunks_exact` calls
and type an egui stroke width explicitly for Rust 1.98.

| Input | Java methods | DEX fallbacks | Acceptance | Gain this batch |
| --- | ---: | ---: | ---: | ---: |
| Zoom | 655,386 / 706,072 | 50,686 | 92.82% | 3,421 |
| Expedia | 1,085,560 / 1,175,337 | 89,777 | 92.36% | 5,296 |
| Vending | 227,733 / 268,289 | 40,556 | 84.88% | 10,232 |

Gains use the initial synchronized commit as baseline. Detailed reports, input
hashes and installed binary hashes: `target/validation/continuation-validation.json`.

These changes extend bounded native lowering. They do not finish the general
SSA-to-region port or establish semantic equivalence for every accepted method.

## Previous: ancestor-owner superclass-call category

All ten reported MeetingCommentActivity callbacks now emit Java; the class scan
improves from 11/21 to 21/21 concrete methods accepted. One shared superclass-chain
check replaces the direct-parent-only requirement. `this` is still required;
interface edges do not authorize ordinary `super` dispatch. Unknown, ambiguous or
unsupported receiver relationships retain fallback. Original DEX method links
are preserved. See pinned JADX/AOSP references in `third_party/jadx/README.md`.

441 regular tests pass (9 opt-in ignored). The additional reported-APK regression
passes for all ten callbacks, including call order and exact source-hash-bound
method references. Strict Clippy, formatting and release build pass. Tests cover
normal/range invocation, inherited owners, invalid receiver, interfaces, missing
parents, conflicts and cycles.

| Input | Java methods | DEX fallbacks | Acceptance | Gain |
| --- | ---: | ---: | ---: | ---: |
| Zoom | 642,104 / 706,072 | 63,968 | 90.94% | 2,529 |
| Expedia | 1,071,286 / 1,175,337 | 104,051 | 91.15% | 1,012 |

Counts measure renderer acceptance, not semantic equivalence or 100% coverage.
Remaining super-call fallbacks include receiver/ancestry cases outside this
supported pattern. Reports, input hashes and installed binary hashes are in
`target/validation/super-validation.json`; detailed logs use the `super-` prefix.

## Previous: guarded loop with an exit tail

`PBXVideoRecordActivity.onBtnSwitchClick()` now emits Java. The bounded loop
renderer handles a forward guard, conditional backedge and one-time exit tail
joining a shared continuation. Separate exit and iteration locals preserve a DEX
register whose type changes only after leaving the loop. Unsupported entries,
backedges or live exit types still reject reconstruction. This extends the
existing renderer; the general SSA-to-region port remains unfinished.

432 regular tests pass (8 opt-in ignored), plus the actual APK regression for all
three reported PBX methods. Strict Clippy, formatting and the release build pass.
The regression checks both breaks, continue placement, one camera update, the
shared updateButtons call outside the loop, and its exact navigation target.

| Input | Java methods | DEX fallbacks | Acceptance | Gain |
| --- | ---: | ---: | ---: | ---: |
| Zoom | 639,575 / 706,072 | 66,497 | 90.58% | 95 |
| Expedia | 1,070,274 / 1,175,337 | 105,063 | 91.06% | 33 |

These counts measure renderer acceptance, not semantic equivalence. Evidence,
input hashes and installed binary hashes: `target/validation/pbx-loop-validation.json`.
Other reports, logs and the final class source use the `pbx-loop-` prefix.

## Previous: PBX constructor argument regressions

Both reported `PBXVideoRecordActivity.getRecordPath()` and `initItems()` now render
as Java. Structured input casts preserve bound-local validation and type links;
ignored `StringBuilder.append(String)` results retain their mutations through
shared capture aliases. Exact effect-order checks remain mandatory. Other
fluent-looking methods are not assumed to return their receiver.

Validation: 424 regular tests pass, strict all-target Clippy and formatting pass,
and the release build succeeds. The additional opt-in `native_pbx_accuracy`
regression passes on the reported Zoom APK, checking both Java bodies, expected
call counts, exact constructor links and field usage resolution. Five new regular
tests cover casts, bound operands, alias retention and rejection of arbitrary
builder-return assumptions.

| Input | Java methods | DEX fallbacks | Java acceptance | Gain in this fix |
| --- | ---: | ---: | ---: | ---: |
| Zoom | 639,480 / 706,072 | 66,592 | 90.57% | 1,496 |
| Expedia | 1,070,241 / 1,175,337 | 105,096 | 91.06% | 2,877 |

These are renderer acceptance counts, not semantic-equivalence proof. The other
reported-class fallback (`onBtnSwitchClick`, loop interior edge) remains open.
Evidence and hashes: `target/validation/pbx-validation.json`; reports and logs
use the `pbx-` prefix, including the before/after class source and APK regression.

## Previous: array inference and nested allocation emission

All 419 regular tests pass (7 opt-in ignored), with strict all-target Clippy,
formatting and release builds passing. Runtime remains entirely Rust.

| Input | Java methods | DEX fallbacks | Java acceptance | Additional Java methods |
| --- | ---: | ---: | ---: | ---: |
| Zoom | 637,984 / 706,072 | 68,088 | 90.36% | 104 |
| Expedia | 1,067,364 / 1,175,337 | 107,973 | 90.81% | 276 |

Acceptance is not semantic-equivalence or whole-class correctness coverage.
The nested-allocation path now consumes SSA constructor identity, preserves
shared aliases and exact overloaded constructor links, and requires an exact
ordered effect trace. Unsupported lifetime/order combinations retain fallback.
The general renderer and region construction are still not fully ported.

Dynamic array listeners reduce unresolved active SSA words by 70,700 across the
two APKs: Zoom has 5,024,559 resolved / 374,950 unresolved; Expedia has 10,937,286
resolved / 1,046,671 unresolved. Both full audits report zero analysis rejections,
type conflicts, wide-pair issues or unresolved constructor origins. Backwards
array inference, general reference joins, generics and conversions remain open.

Evidence: `target/validation/array-nested-{renderer,audit}-{zoom,expedia}.json`;
test/lint/build logs and binary/input hashes use the `array-nested-` prefix.

## Previous: SSA types, constructor identities and exceptional edges

The full regular suite passes **402 tests**, with 7 opt-in tests ignored. Strict
all-target Clippy, formatting and release builds pass. The final analysis audits
cover every concrete method in both input APKs:

| Input | Methods | Resolved active SSA words | Unresolved active SSA words | Constructor calls tracked |
| --- | ---: | ---: | ---: | ---: |
| Zoom | 706,072 | 4,984,613 | 414,896 | 274,872 |
| Expedia | 1,175,337 | 10,906,532 | 1,077,425 | 727,283 |

Both reports have zero analysis rejections, zero reported type conflicts, zero
wide-pair issues and zero unresolved constructor origins. **This is not 100% Java
coverage or a semantic-equivalence result.** Unresolved bounds remain explicit,
including array component producers, ambiguous literal uses, reference joins,
missing hierarchy and conversions. The old Java renderer is still active.
Fresh final renderer scans remain unchanged: Zoom 637,880/706,072 methods
(90.34%, 68,192 fallback); Expedia 1,067,088/1,175,337 (90.79%, 108,249 fallback).
The release binaries were atomically installed into `target/RDX.app` and their
SHA-256 identities verified against the audited build.

The independent constructor review retained actual invoke owners and marked
16,828 Zoom calls for eventual owner retargeting, including optimized allocation
and ancestor-chaining cases. No instructions were moved or rewritten. SSA now
removes impossible exceptional edges from non-throwing instructions; genuine
throwing instructions preserve pre-write state. This removed the false conflicts
and incoherent joins observed in the first corpus run. Further regressions cover
char/-1 widening, runtime reference conversions and verified platform interfaces.

Final reports: `target/validation/jadx-types-constructors-final-{zoom,expedia}.json`.
Earlier unsuffixed reports are intermediate diagnostic runs, not final results.
Tests/build/lint logs use `types-constructors-*`; identities and renderer results
are pinned in `types-constructors-validation.json`. The renderer scans are
`types-constructors-renderer-{zoom,expedia}.json`.

## Previous: SSA and signature constraints

The Rust SSA pass and signature-to-value binding pass accept every concrete
method in both complete APKs: 706,072 Zoom and 1,175,337 Expedia methods, with
zero rejections. Zoom produces 6,683,446 word definitions and 197,667 phi nodes;
Expedia produces 14,272,075 definitions and 583,868 phi nodes. The two audits link
6,748,312 calls/array producers to SSA values. No calls were unreachable in these
inputs. Input and release-binary hashes and full counts are recorded in
`target/validation/jadx-ssa-validation.json`.

Ten SSA fixtures exercise pruned joins, loop-entry inputs, exceptional pre-write
state, parallel normal/exception paths, wide overlap, undefined values and limits.
Two call-value fixtures verify repeated arguments, wide grouping, reused result
registers, dead calls and mismatched binding. Independent code review found no
blocking issue. The complete suite passes 370 tests (7 opt-in tests ignored);
strict all-target Clippy, formatting and release builds pass. Logs use `ssa-*`.

The refreshed installed-renderer baseline is unchanged: Zoom 637,880/706,072
methods accepted (68,192 fallback); Expedia 1,067,088/1,175,337 (108,249 fallback).
Reports are `target/validation/ssa-baseline-{zoom,expedia}.json`. This distinguishes
SSA acceptance from Java reconstruction. Type inference, constructor analysis,
regions and new-pipeline Java emission are still unfinished. Full Java coverage
has not been achieved, and the new SSA stage is not yet used by the GUI emitter.

## Latest: call binding, annotations, usage retention and field spacing

The signature-binding pass validates receiver/parameter grouping, contiguous wide
arguments, effective polymorphic prototypes and adjacent typed move-result uses.
Both full APK audits completed without binding rejections (1,881,409 methods;
6,748,312 call/array producers). This is new-pipeline analysis, not a switch of
the Java generator to SSA.

Annotation tests independently inventory raw DEX JavascriptInterface items and
compare them with rendered source and declaration navigation: 18 Zoom and 57
Expedia methods. This includes classes with DEX fallback bodies. Field declaration
spacing is checked alongside the source mappings. Shared annotation retention
uses budgeted allocations and avoids empty per-member arrays for class-only sets.

Find-usages regressions scan over 32 MiB of nonmatching source and still find a
later result, verify the actual retained-source cap, and preserve cancellation.
The engine returns no source document for empty occurrence lists.

Regular suite: 358 passed, 7 opt-in tests ignored. Formatting, strict all-target
Clippy and release builds pass. Evidence logs use the
`target/validation/calls-annotations-usages-*`, `js-*-validation.log` and
`jadx-calls-*.json` prefixes. Earlier sections below describe historical revisions.

This revision removes the Java worker, fallback routing, heap preferences, Java
CI setup and Java-dependent engine tests. Appearance/search preference migration
preserves saved UI settings while discarding obsolete heap settings. The bundled
source-statistics plugin is now a Rust executable. Development-only fixture and
transport-test helpers may use Python; application operation does not require it.

Native tests cover parsing/limits/malformed data/multidex, constant-return Java,
explicit disassembly and resolved operands, Unicode/source identity, overloaded
navigation, field links, usages, repeated cached search, cancellation, native XML,
plugin transport, and existing asset/UI behavior. Engine CLI tests run with an
empty PATH to establish that an external Java executable is not needed.

The search-completion fix explicitly repaints both root and search windows;
background completion no longer relies on whichever viewport happens to be active.
An automated regression checks those repaint targets and completed search state.

Run `cargo test --locked --all-targets`, `cargo clippy --locked --all-targets -- -D warnings`,
and `cargo build --locked --release --bins`. This is native runtime validation,
not validation of a complete JADX-equivalent decompiler.

The final migration suite passed 108 tests, with strict Clippy and release builds.
The local Zoom APK loaded 97,592 native classes and completed full native search
without skips/errors/limits. Native method/field navigation and usages are tested
against the synthetic overloaded-method fixture. Real Zoom binary manifest
decoding was also verified. Full-Java reconstruction parity is explicitly not
claimed. See search-performance.md for the native-only measured workload.

## Straight-line native Java reconstruction

The reconstruction revision passes 124 tests, strict all-target Clippy, formatting
and release builds, without adding Java or invoking a Java compiler/runtime.
The new tests check reference/declaration spans, overloaded jumps, exact usages,
constructor effect order, null receivers, field/call operands, invalid frames,
unsupported instructions and mixed-document range remapping. An independent
small Rust expression evaluator checks generated integer expressions for wrapping,
reverse subtraction and masked unsigned shifts at boundary values. This is not
full semantic equivalence testing for arbitrary Java programs.

On the local Zoom class `com.zipow.videobox.fragment.id`, four of 13 methods now
render as Java (fn, gn, onClickBack, dismiss); nine remain DEX. Captured output is
`target/validation/native-java-zoom-id.txt`. `sample.Target` reconstructs both
overloaded doubleValue bodies and its constructor, while preserving unported
class metadata in a mixed document. The updated binary is installed in
`target/RDX.app`; reopening is required to replace the running process.

## Native forward branches (alpha)

The forward-branch revision adds structured `if`/`else`, nested guards, early
returns, and typed register merges with a single shared continuation. Independent
Rust evaluators compare DEX and generated Java behavior for signed boundary
values, booleans, null/reference equality, and merged values. An engine-level
fixture verifies exact navigation and usages inside reconstructed branches.
Backward/malformed branches and unsupported joins retain explicit DEX fallback.
No Java compiler or runtime is used.

The local Zoom class `com.zipow.videobox.fragment.id` now renders seven of 13
methods as Java: fn, gn, onClickBack, dismiss, en, onClick, and onResume. Six
remain DEX. Output: `target/validation/native-branches-after.txt`.

The full native search probe covered 97,592 classes with no errors, skips, or
limits: 13.32 seconds cold and 2.15 seconds on repeat. The bounded cache caused
16,321 source fetches on repeat, so this is not a fully cached measurement.
These timings search mixed Java/DEX text and do not establish performance parity
with JADX's full Java output. Evidence:
`target/validation/native-branches-full-search.json`.

Final validation: 134 tests passed, formatting check, strict all-target Clippy,
and release builds passed. User-facing engine labels now say alpha.

## Expanded native control flow and typed operations

This revision passes 178 Rust tests, strict all-target Clippy, formatting, and
release builds. It adds single-entry while/do loops, forward packed/sparse
switches, arrays and filled arrays, class literals, casts, instance-of, integer
narrowing, bridge/varargs declarations, constructor delegation, conservative
static initializers and safe Object joins for differing references. Generated
invocation arguments preserve null and primitive-widening overload selection.

Independent Rust DEX/Java-subset evaluators validate branch, loop and switch
behavior. Fixtures additionally check effect ordering, parallel loop-carried
copies, shared tails, payload rejection and exact symbol spans/navigation.
No Java compiler/runtime participates in these checks.

The final local Zoom scan accepted 509,195 of
706,072 concrete method bodies
(72.12%). 196,877 remain DEX.
This is renderer acceptance coverage, not whole-class Java coverage or proof of
semantic equivalence for every accepted method. First-failure reasons are
recorded in `target/validation/native-completion-coverage-after.json`; a method
can have further blockers beyond the first.

`com.zipow.videobox.fragment.id` now has 12 of 13 reconstructed methods;
`onCreateView` remains DEX. Output:
`target/validation/native-completion-zoom-id.txt`. The engine remains alpha:
wide values/arithmetic, exception regions, complex allocation/initialization,
remaining type inference and full metadata/resource reconstruction are unfinished.

The final full native search scanned all 97,592 classes with no errors or skipped
classes. Release binaries are installed in `target/RDX.app`; reopen the app to
use them. See search-performance.md for the measured workload and limits.

## Encoded fields, exceptions, numeric operations and liveness (2026-09-20)

Latest checks: 248 regular Rust tests and both opt-in real-APK checks pass
(250 total). Formatting, strict all-target Clippy and release builds pass.
No Java compiler, JVM or external decompiler participates.

The added native implementation decodes encoded values, try/catch regions and
Throws annotations; reconstructs supported field initializers, single-region
handlers, terminal throws and straight-line wide/floating-point instructions;
and prunes dead register joins with bounded liveness. Behavior fixtures compare
small independent DEX and Java-subset evaluators, including completed writes,
failed calls, copied register aliases, constructor failure and capture reuse.
They do not prove equivalence for every accepted method or replace a complete
Java type checker. Unknown hierarchies, narrow checked-catch reachability, wide
joins and more complex control flow remain conservative DEX fallbacks.

The real input is pinned in `target/validation/native-accuracy-input.json`:
SHA-256 `16fb587a87fd3f8eae4a5de1d6a6f8415faadf343a796676c77fbd761decf051`.
`IntegrationActivity.acceptNewIncomingCall` now reconstructs with its actual
try/catch and constructor sequence, without dead Object merge variables. Both
reported String constants retain their encoded values. Exact field identities
and class-literal navigation are checked against the rendered source hash.

The APK scan reconstructs 548,948 of 706,072 concrete methods (77.75%);
157,124 remain DEX. This is acceptance coverage, not an accuracy percentage.
The previous scan reconstructed 509,195 methods (72.12%). The largest remaining
blockers are effects between allocation and constructor, wide joins, writes
before constructor initialization, missing exception hierarchy and complex CFGs.

Evidence: `target/validation/native-accuracy-tests.log`,
`native-accuracy-real-apk.log`, `native-accuracy-clippy.log`,
`native-accuracy-build.log` and `native-accuracy-coverage.json`.

Full native search visited all 97,592 classes with zero errors/skips and matching
cold/repeated results. It took 17.093 seconds cold and 2.607 seconds repeated,
excluding 1.758 seconds loading, with 941.625 MiB child peak RSS on macOS.
This is a larger reconstructed corpus than the prior measurement and is not a
JADX parity comparison. See `native-accuracy-search.json` and
`native-accuracy-search-memory.json` in the same validation directory.

The latest `com.zipow.videobox.fragment.id` output reconstructs all 13 methods,
including `onCreateView`; class-level unsupported metadata still uses the mixed
view. Evidence: `target/validation/native-accuracy-zoom-id.txt`.

## Native expansion validation (2026-09-20)

The next measured revision reconstructs **625,870 / 706,072 concrete methods
(88.64%)**, compared with 548,948 previously. **80,202 methods remain DEX**.
These counts measure renderer acceptance, not semantic accuracy or complete
class reconstruction. The input is the same SHA-256 pinned above.

This revision adds bounded project hierarchy queries, custom exception typing,
wide normal-flow joins and loops, closed branch joins, Java 25 constructor
prologues, member aliases retaining raw DEX identities, boolean/integer lowering
and transactional allocation argument captures. Repeated per-member reconstruction
comments have been removed; mixed Java/DEX documents remain labeled.

All three reported IntegrationActivity methods (`getIMActivity`,
`handleActionCCIMainPage`, `handleActionCCIIncomingCall`) pass the real-APK
reconstruction and declaration-span checks. Regression fixtures cover bypassing
branch paths, long joins, wide pairs, custom hierarchy relationships, constructor
initialization order, malformed allocation operands, effect ordering, aliases
and shifted navigation links after consecutive allocations.

The largest remaining categories are effects between allocation and constructor
(25,924), allocation/constructor mismatch (7,653), unsupported loop exit guards
(6,846), unsupported interior loop edges (5,336), and nested/multiple try regions
(4,972). Other gaps include monitor instructions, superclass receiver typing and
wide exception snapshots. They are explicit fallbacks, not completed features.

Evidence in `target/validation/`: `native-expansion-coverage.json` includes
bounded representative methods per failure category; `native-expansion-tests.log`,
`native-expansion-clippy.log`, `native-expansion-build.log`, and
`native-expansion-real-apk.log` record validation.

Validation completed: 291 regular tests and 2 opt-in real-APK checks passed;
all-target Clippy with warnings denied and release binary builds passed. Both
binaries were installed in `target/RDX.app` and SHA-256 matched to their release
outputs (`native-expansion-installed.json`).

Full native no-match search visited all 97,592 classes without errors or skips:
20.562 seconds cold and 2.700 seconds repeated, excluding 1.901 seconds loading.
Child peak RSS was 929.141 MiB on macOS. The cold scan is slower than the prior
17.093-second measurement while reconstructing more Java; this is not a speed
improvement or a controlled JADX comparison. Evidence: `native-expansion-search.json`
and `native-expansion-search-memory.json`.

## Boolean recovery and readable types (2026-09-20)

`IntegrationActivity.handleActionInputProxyNamePass` now preserves the boolean
result of `xor-int/lit8` with one as Java negation.
`handleActionPBXNewSMSFromSchema` now reconstructs its parse/catch/null-check
flow: entry constants proven unchanged throughout the try keep their original
types during handler analysis. Unknown writes retain conservative snapshots.

Java declarations use simple names and consolidated imports when unambiguous.
One class-wide naming plan prevents cross-method import collisions. Token-aware
rewriting preserves strings/comments and leaves raw DEX untouched; Unicode source
spans, raw target identities, import links and source hashes are maintained.
Nested binary names and shadowed/conflicting names remain qualified.

The real-APK regression checks both reported methods, existing methods, encoded
fields and navigation after shortening. Logs: `native-readability-tests.log`,
`native-readability-real-apk.log`, `native-readability-clippy.log`,
`native-readability-build.log` in `target/validation/`.

Final validation: 298 regular tests and the opt-in real-APK regression passed;
all-target Clippy with warnings denied and release builds passed. Installed
binaries match the release SHA-256 values (`native-readability-installed.json`).
Full no-match search visited all 97,592 classes without skips/errors: cold
26.982 seconds, repeated 3.768 seconds,
peak child RSS 837.094 MiB. Readable-type processing adds
cold-search overhead compared with the previous 20.562-second measurement;
these are smoke measurements, not a controlled comparative benchmark.
See `native-readability-search.json` and `native-readability-search-memory.json`.

## Expedia loading and archive XML preview (2026-09-20)

Pinned input SHA-256:
`63e44c00725c4f2322f2b366cbedb20bed385b9a4b2fb79692db083cf0d40b0d`.
The APK contains 25 DEX files and 228,363 classes. It failed the previous
768 MiB aggregate retained-data budget: measured capacity-based retained charge
is 955,524,525 bytes (911.26 MiB). The aggregate cap is now 1 GiB; temporary
parsing remains independently capped at 768 MiB per DEX. A bug that clamped
caller-supplied aggregate balances to the per-DEX cap has been fixed.

Compiled XML previews now decode directly from bounded archive data before
text-encoding detection, independently of DEX loading. The actual Expedia
manifest previews successfully without creating a decompilation engine.

300 regular tests and two opt-in Expedia checks passed, along with all-target
Clippy with warnings denied and release builds. CLI inventory loading completed
in 4.232 seconds with 1,251.719 MiB child peak RSS; the retained-data ceiling is
not an RSS limit. No full Expedia reconstruction-coverage claim is made.

Evidence: `target/validation/expedia-tests.log`, `expedia-real-apk.log`,
`expedia-load-measurement.json`, `expedia-resource-tests.log`,
`expedia-clippy.log`, `expedia-build.log`, and `expedia-installed.json`.

## Expedia default arguments and Java interface presentation (2026-09-20)

Both reported ProductFlavourFeatureConfig `$default` methods now reconstruct
their marker rejection paths. Allocation lowering captures ordinary/jumbo string
constants inside constructor arguments, retains aliases, and checks exact
allocation/string/constructor event order. Unsupported patterns still fall back.

Representable mixed declarations now use Java class/interface syntax with
imports and braces; an interface extends its parent interfaces. Unsupported
members remain DEX. Unsupported header kinds preserve their raw declaration
without discarding successful member reconstruction. Requested mixed-output and
encoding/offset explanatory banners are removed. Interface member legality,
bridge/default placement, imports and Unicode navigation ranges have regressions.
The current class uses its simple type name while colliding external types remain
qualified.

The opt-in `native_expedia_accuracy` regression checks both methods, the Java
interface header, banner removal, overloaded identities and navigation using
the pinned Expedia APK. The previous Zoom regression also passes. Evidence in
`target/validation/`: `expedia-default-tests.log`, `expedia-default-clippy.log`,
`expedia-default-build.log`, `expedia-default-real-apk.log`,
`expedia-default-zoom-regression.log`, and `expedia-product-feature-config.java`.

## Manifest navigation, fields, and Kotlin static initialization (2026-09-20)

Confirmed UI defect: manifest clicks were silently discarded while search owned
the native engine. Manifest class navigation now uses the same bounded pending
request mechanism as code navigation, keeps the original manifest position for
Back, coalesces older jumps, and drains requests arriving after the final search
worker poll. Click routing uses the exact links already attached to the document.
No independent idle-path failure was reproduced.

The UI regression drives archive/project event arrival in both orders, then
actual manifest-navigation methods with the engine idle or owned by search,
including search completion before request handling, resulting class display and
Back to the manifest. The glyph-level double-click regression also covers XML
with usages disabled and Unicode before the target. The actual Expedia manifest
produces 212 component links and opens sampled component declarations.

Mixed-view fields now show Java declarations instead of `.field` records. Missing
encoded values are not fabricated; constructor/static-block assignments remain
in their original method output. Omitted encoded values have a precise note.
Structurally unsupported fields retain explicit unavailable metadata comments.

Ordered class-literal captures allow the reported PhoneLaunchActivity `<clinit>`
to render as a Java static block. The real-APK regression checks the block,
class-literal navigation, field declarations and exact field target identities.
312 regular tests passed; real Expedia manifest/component and static-initializer
checks are recorded separately. See `target/validation/manifest-fields-static-tests.log`,
`manifest-fields-static-expedia.log`, `manifest-navigation-ui.log`,
`manifest-navigation-expedia.log`, and matching Clippy/build logs.

### Readable staged allocation reconstruction

Following the user's explicit choice of JADX-compatible readable output, flat
allocation windows can fall back to ordered temporary declarations before the
Java `new` expression. The original cast, call, field-read, and string-resolution
sequence must still match exactly. Only the root allocation moves from its DEX
location to the constructor location. This changes potential class-initialization,
linkage, and allocation-failure timing; it is not an exact semantic-equivalence
claim. The strict expression path remains preferred. Nested allocations, try
regions, branch crossings, forward/cyclic dependencies, and unused captures
remain excluded from this fallback.

`tests/native_vending_accuracy.rs` validates Java output and original navigation
for `ClassicApplication.e`, `SubscriptionAskToPauseActivity.onClick`, and
`ScreenshotsActivityV2.x`. The latter retains `bdzh` then `wrt` checks as temporary
statements before each construction. `onClick` retains both `String.valueOf`
calls, then the prefix string, then `concat`, then construction. Baseline
Screenshots evidence (5/6 methods) remains in
`target/validation/screenshots-before.json` and `screenshots-before.txt`.
After staging, the class renders 6/6 methods as Java; evidence is in
`target/validation/screenshots-staged-after.json` and
`target/validation/screenshots-staged-after.txt`. The three-method Vending
regression passes with raw navigation links.

The nested `_COROUTINE.b.b` example remains outside the staged fallback: its
allocation nesting and reordered literal dependencies still require broader
sequence reconstruction.

Combined manifest, tab controls, and staged allocation validation: 486 regular
tests pass (12 opt-in APK tests ignored), strict all-target Clippy passes, and
the release binaries build. The Play Store corpus now renders 228,382 of
268,289 concrete methods, up 649 from 227,733; 39,907 retain fallback output.
These counts measure renderer acceptance, not semantic equivalence. Evidence:
`target/validation/staged-vending-after.json` and
`target/validation/manifest-tabs-staged-tests.log`.
