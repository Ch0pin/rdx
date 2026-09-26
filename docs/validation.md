# Validation

RDX uses synthetic DEX fixtures, renderer regressions, malformed-input checks,
and focused behavioral tests. Tests exercise source reconstruction, navigation
metadata, resources, search, and the GUI and MCP interfaces.

## Run checks

```sh
cargo fmt --all -- --check
cargo test --locked -j 2 --no-fail-fast
cargo clippy --locked --all-targets --all-features -j 2 -- -D warnings
cargo build --release --locked --bin rdx
```

Some tests are opt-in and need additional local inputs or tools. An ignored test
has not passed merely because the default suite succeeds. Consult the relevant
test's prerequisites before running it explicitly.

## What the results establish

Renderer coverage counts accepted Java method bodies for a particular input.
It does not measure semantic accuracy or coverage across all Android apps.
Behavioral fixtures compare results, exceptions, and ordered effects within their
stated scope; they do not prove arbitrary Android runtime behavior or concurrency.

Review fallback reasons alongside successful reconstructions. Keep corpus inputs,
app-specific reports, and internal implementation inventories local. Public
reports should describe reproducible RDX behavior and disclose validation limits.

See [coverage commands](native-coverage.md),
[focused semantic checks](focused-semantic-check.md), and
[search measurements](search-performance.md).
