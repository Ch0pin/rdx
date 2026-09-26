# G01-B-fields validation

Date: 2026-09-26. Gaps: M01, M06. Complete for the bounded scope; G01-B and G01 remain open.

## Production change

Eligible straight-line Java rendering consumes shared decoded operands for all
28 instance/static field get/put encodings (0x52..0x6d). Receiver, value/destination
and field-pool identity come from the shared instruction. The helper validates
operand counts/kinds, full wide-register bounds, Field16 pool reference and
instruction effect/width metadata. It adds no per-instruction heap allocation.

Existing field type checks, receiver/value evaluation, constructor guards,
materialized reads, writes and navigation labels remain in place. Shared
validation failures propagate without retrying raw operands; other rendering
routes retain their existing behavior.

## Verification

- Three private tests cover all 28 encodings: raw operand/reference poisoning,
  changed valid shared field identity with source/navigation checks, and malformed
  operand/reference/metadata rejection after a successful typed baseline.
- Public matrix: 40 fixtures spanning all 28 encodings and nine Java types;
  48 negatives cover type mismatch, missing pool entries and wide-register bounds.
- JVM checks cover primitive values, quiet-NaN raw bits, signed zero, reference
  identity, null receiver exceptions, receiver/destination aliasing, instance/static
  read-before-write snapshots and once-only initialization on first static get.
- Sol implemented production and public tests; Astra reviewed independently;
  parent inspected integration and runs the full gates. Review corrected a no-op
  malformed-kind mutation and store tests that could miss omitted writes.

## Completed gates

Full default suite: **792 passed, zero failed, 82 ignored**. The selected optional
JVM test also passed. Strict all-target Clippy, formatting, whitespace checks and
the release build passed.

The entire pinned Play Store corpus report is byte-for-byte identical:
260,225 / 268,289 concrete methods reconstructed (96.994286%), 8,064 fallbacks.
APK SHA-256: `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.

Three alternating paired search runs over 300 classes returned identical 53
matches and source coordinates, without errors, skips or limits. Cold median:
223.657 ms before → 229.574 ms after (+2.6%, 5.9 ms). Warm median:
1.231 ms → 1.228 ms. The added helper uses fixed-size constant work; the small
local timing sample is not a universal performance guarantee.
[Machine-readable evidence and hashes](g01b-fields.json).

Next increment: **G01-B-array-types**, shared operands/type references for
check-cast, instance-of, array-length, new-array and array get/put (18 encodings).
Preserve exception/materialization order and existing type/fallback boundaries.

## Boundaries

This consolidates operand decoding; it does not expand supported field semantics
or certify general semantic accuracy. Eligibility and constructor behavior are
unchanged. The new JVM matrix does not cover constructor early-field execution,
volatile concurrency, resolution/access failures or class-initialization failure
precedence. Array/type and throw operands, shared CFG and general SSA/type-driven
emission remain open. No full visitor or delivery unit closes.

No release, GUI deployment, commit or push was performed.
