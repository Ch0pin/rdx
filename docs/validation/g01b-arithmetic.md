# G01-B-arithmetic validation

Date: 2026-09-26. Gaps: M01, M06. Complete for the bounded scope; G01-B and G01 remain open.

## Production change

Eligible straight-line Java rendering consumes shared decoded registers and signed
literals for all 109 arithmetic encodings: comparisons 0x2d..0x31, unary/conversions
0x7b..0x8f, normal binary 0x90..0xaf, 2addr 0xb0..0xcf and literals 0xd0..0xe2.
This includes boolean bitwise fast paths and byte/char/short narrowing. Existing
numeric expression generation, read order, materialization and legacy routes stay
in place. Shared validation errors propagate without retrying raw operands.

The helper validates operand count/kind, full wide-register bounds, instruction
width/effect metadata, signed literal width and 2addr destination identity. It uses
fixed-size storage, with no added per-instruction heap allocation.

## Verification

- Private consumption tests poison raw arithmetic operands for all 109 encodings
  and 12 boolean cases after shared decoding. Negative fixtures first demonstrate
  a valid typed baseline, then reject malformed counts, kinds, bounds, metadata,
  2addr identity, literal overflow and wide-shift operand widths.
- Public renderer matrix: 128 fixtures spanning all 109 encodings, with independent
  expression checks for operators, operand order, narrowing and signed literals.
- JVM comparison: 11,728 input combinations against independent Java expressions,
  including overflow, shifts, division/remainder exceptions, narrowing, NaN bias,
  infinities and signed zero.
- Implementation and public tests delegated to Sol; independent review to Astra;
  parent inspected integration and runs the full validation gates. Review identified
  and corrected redundant heap allocation and negative-test baseline weaknesses.

## Completed gates

Full default suite: **785 passed, zero failed, 81 ignored**. The selected optional
JVM test also passed. Strict all-target Clippy, formatting, whitespace checks and
the release build passed.

The complete pinned Play Store corpus report is byte-for-byte identical:
260,225 / 268,289 concrete methods reconstructed (96.994286%), 8,064 fallbacks.
APK SHA-256: `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.

Three alternating paired search runs over 300 classes returned identical 53
matches and source coordinates, without errors, skips or limits. Cold median:
228.949 ms before → 225.042 ms after (-1.7%). Warm median: 1.227 ms → 1.190 ms.
This small local timing sample does not establish a universal speed improvement.
[Machine-readable evidence and hashes](g01b-arithmetic.json).

Next increment: **G01-B-fields**, shared instance/static field operands and pool
references (0x52..0x6d), preserving receiver/value order, width/type checks and
existing materialization behavior.

## Boundaries

This is a shared-operand migration, not new opcode support or a general semantic
accuracy certificate. Branch/handler/allocation eligibility is unchanged. Field,
array/type and throw operands, shared CFG and general SSA/type-driven emission
remain open. Arithmetic NaN payload identity is not proved: the JVM oracle
canonicalizes NaNs. No full visitor or delivery unit closes.

No release, GUI deployment, commit or push was performed.
