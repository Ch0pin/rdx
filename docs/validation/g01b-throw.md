# G01-B-throw validation

Date: 2026-09-26. Gaps: M01, M06. Complete. Bounded G01-B is complete; G01 remains open.

## Production change

Eligible straight-line Java rendering consumes the shared throw (0x27) source.
The adapter requires exactly one Reference read, no writes, width one, may-throw
metadata and no literal/pool/prototype/branch/payload metadata. It validates the
source register bounds without adding heap allocation. Invalid shared inputs fail
without retrying raw operands.

Existing allocation/prologue checks run in the same order. Null throws, precise
rethrow identities, known/declared/inferred exception checks and terminal emission
retain their existing behavior. The legacy path remains available for excluded
method shapes.

## Verification

- Three private tests cover poisoned raw operands for unchecked, declared checked,
  null and mapped precise-rethrow values; 12 malformed shared mutations after a
  successful typed baseline; and constructor/terminal guards.
- Public matrix: 17 methods plus one structural constructor positive, and 19
  rejections covering invalid types/registers, unreachable tails, checked
  declarations and constructor boundaries.
- Selected JVM test verifies 23 exception outcomes (identity and exact null-to-NPE)
  and five preceding-effect checks, including an earlier failing call taking
  precedence over the explicit throw. It compiles 17 generated methods.
- Sol implemented production and public tests; Astra reviewed independently;
  parent inspected integration and runs the full gates. Initializer evidence
  concerns rejecting checked throws metadata with a null body, not runtime checked
  exceptions from class initialization. Constructor evidence is structural only.

## Bounded G01-B audit

Independent review found that all successfully rendered non-invocation operand
families in the current shared straight-line path now consume shared operands.
Remaining raw reads are opcode-word access/nop validation and eligibility boundary
scanning, or explicitly legacy/excluded branches, handlers, monitors, new-instance
and filled/payload-array paths. Rejected modern opcodes are not newly supported.
Validation passed, so G01-B is closed for this bounded scope. G01 and whole JADX
visitors remain open.

## Completed gates

Full default suite: **805 passed, zero failed, 84 ignored**. The selected optional
JVM test also passed. Strict all-target Clippy, formatting, whitespace checks and
the release build passed.

The entire pinned Play Store corpus report is byte-for-byte identical:
260,225 / 268,289 concrete methods reconstructed (96.994286%), 8,064 fallbacks.
APK SHA-256: `c9910583fc93af2b750bde69d3db89a625359e19281adb5b3aa607b640d64ee5`.
Report SHA-256: `54ec0d59c0dd5148cd40bed1a9779babf5cd989455cda43a106aa05d882776f9`.

Three alternating paired search runs over 300 classes returned identical 53
matches and source coordinates, without errors, skips or limits. Cold median:
224.478 ms before → 225.278 ms after (+0.36%, 0.800 ms). Warm median:
1.252 ms → 1.198 ms. This small local sample is not a universal performance claim.
[Machine-readable evidence and hashes](g01b-throw.json).

Next increment: **G01-C-start**. Build shared CFG information from decoded
instructions and make production consume successors, joins and decoded conditions
for one forward conditional diamond. Exclude nested branches, backedges, switches,
handlers, monitors, allocation-specific paths and payloads initially. Existing
reconstruction semantics and fallback boundaries remain the baseline.

## Boundaries

No eligibility expansion, exception hierarchy expansion or new handler support.
Constructor prologue source checks do not establish new runtime coverage of Java 25
constructor behavior. Shared CFG, SSA/type-driven emission and general region
construction remain open. No release, GUI deployment, commit or push.
