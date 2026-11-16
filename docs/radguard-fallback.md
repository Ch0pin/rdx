# RadGuard fallback

Choose **View → Preferences… → Decompilation → Fallback decompiler → RadGuard**.
RDX first renders native source, then requests alternate text for methods that retained
DEX fallback. Native-complete methods retain their original source and navigation.
The same browsing policy applies when opening code-search results.

Each inserted method displays a red **Unverified RadGuard reconstruction** warning.
Expand **Original DEX** to inspect the exact original method, native failure reason,
and alternate diagnostics. RadGuard produces Java-like pseudocode; displaying a
method does not establish Java compilation or semantic equivalence.

The native helper is bundled beside RDX on macOS and Linux. It runs in a separate
process, with a 30 second timeout, 512 MiB sampled resident-memory ceiling, 8 MiB
output ceiling, and a 64 MiB per-project disk cache shared across fallback engines.
Both in-memory documents and disk results use separate backend identities. RDX
continues with native source when the helper fails or reaches a bound. Windows
reports unsupported worker memory enforcement.

Pinned upstream: https://github.com/MG1937/radguard/tree/6535e121a5923b54111619001de7f25872575895
Backend identity: `radguard-6535e121a5923b54111619001de7f25872575895-rdx1`.
The helper invokes the existing per-method writer through a visibility-only patch.
It uses exact owner/name/raw DEX descriptors to distinguish overloaded methods.
Attribution, source hashes, the patch, and dependency notices are under
`third_party/radguard` in the repository and packaged notices.

Settings persist `alternate_fallback` as `none`, `androguard`, or `radguard`.
Older `androguard_fallback` boolean values migrate to the matching None/Androguard
selection. Release users need no environment configuration; developers can set
`RDX_RADGUARD_HELPER` to an absolute matching helper executable during tests.
