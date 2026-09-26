# G01-B-array-types validation

Date: 2026-09-26. Gaps: M01, M06. Complete for the bounded scope; G01-B and G01 remain open.

## Production change

Eligible straight-line rendering consumes shared operands for 18 encodings:
check-cast, instance-of, array-length, new-array (0x1f..0x21, 0x23), and array
get/put (0x44..0x51). Type references come from shared Type16 pool identities.

The shared decoder adapter checks counts/kinds, full wide-register bounds,
instruction width/effect metadata, pool-reference presence/kind/range and
check-cast source/destination identity. A fixed-size normalized operand structure
feeds existing expression generation directly; shared operands are not re-encoded
as synthetic raw instructions. The legacy reader uses the same structure.
Constructor input guards consume the selected operand structure too.

Existing materialized reads/casts, negative-size/null/bounds/store exception
behavior, narrow stores and reference-array Object[] store checks remain in place.
Shared validation failures do not retry raw decoding. Const-class and legacy
narrow-conversion users of the operations interface retain their behavior.

## Verification

- Four private tests cover all 18 encodings: raw operand/reference poisoning with
  source/link parity; changed valid type identity; malformed shared invariants
  after typed successful baselines; and constructor guards with poisoned raw
  instructions, rejecting each decoded input individually when it carries `this`.
- Public matrix: 48 fixtures (nine component types, cast/type tests, multidimensional
  allocation, aliases, snapshots and subtype stores), plus 61 negatives for invalid
  pools, mismatched components, scalar length and wide-register bounds. Type link
  labels and character spans are checked explicitly.
- Selected JVM test checks identity, quiet-NaN raw bits and signed zero, narrowing,
  casts, negative allocation sizes, null/bounds/store exception precedence, and
  read-before-write snapshots. A declared String[] store taking Object checks that
  an incompatible value raises ArrayStoreException rather than ClassCastException.
- Sol implemented production and public tests; Astra reviewed independently;
  parent inspected integration and runs the full validation gates. Review added
  exact navigation checks, subtype store proof and poisoned constructor-guard tests.

## Completed gates

Full default suite: **799 passed, zero failed, 83 ignored**. The selected optional
JVM test also passed. Strict all-target Clippy, formatting, whitespace checks and
the release build passed. Clippy found one redundant usize conversion in a new
private test; after removing it, all four affected private tests passed again.
The full suite had already compiled the semantically identical test version;
production code was unchanged by this cleanup.

The entire pinned Play Store corpus report is byte-for-byte identical:
260,225 / 268,289 concrete methods reconstructed (96.994286%), 8,064 fallbacks.
APK SHA-256: `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.

Three alternating paired search runs over 300 classes returned identical 53
matches and source coordinates, without errors, skips or limits. Cold median:
229.867 ms before → 226.371 ms after (-1.5%). Warm median: 1.232 ms → 1.261 ms (+0.029 ms).
This small local sample is not a universal performance claim.
[Machine-readable evidence and hashes](g01b-array-types.json).

Next increment: **G01-B-throw**, shared throw operand (0x27), preserving exception
type/null handling, constructor guards and terminal instruction validation.

## Boundaries

This is operand integration, not expanded opcode semantics or general semantic
accuracy proof. Array access/length still require known array types; an untyped
null literal remains conservative fallback. Filled/payload arrays, throw/handler
operands, shared CFG and general SSA/type-driven emission remain separate work.
Type-resolution and allocation-failure behavior are not newly certified. No full
visitor or delivery unit closes. No release, GUI deployment, commit or push.
