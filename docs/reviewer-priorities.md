# Reviewer request priorities

Reviewed: 2026-11-15. Baseline: main commit `6d6804d4434218fcd51d169c2af36d964d69baee`.

This records the implementation audit and agreed prioritization of the RDX reviewer notes dated 2026-11-01 through 2026-11-10. The audit inspected source, MCP schemas and existing tests; it did not rerun every APK reproduction. Medusa-specific proposals are outside this backlog. No implementation is implied by an entry being listed here.

## Priority definitions

| Priority | Meaning |
| --- | --- |
| P0 | Emergency requiring immediate action. None established by this audit. |
| P1 | Incorrect conclusions or a blocked primary workflow. |
| P2 | High-value improvement after P1 correctness fixes. |
| P3 | Useful, bounded enhancement. |
| P4 | Investigate or defer until evidence supports the investment. |
| P5 | Decline the proposal as written; a narrower evidence-producing version may be worthwhile. |

## Current implementation foundations

| Capability | Verified scope at the baseline |
| --- | --- |
| Annotation usages and Retrofit recovery | Class, field, method and parameter annotations with explicit values; supported renamed Retrofit roles; service rows and supported local creation/base-URL links. |
| Source search | Resumable cursors, page coverage diagnostics, regex/case options, shared cached renders and parallel scanning. The 20-second source-scan limit is a page budget. |
| Source access and references | Class/name method selection, source pagination, complete method export, indexed type references, identifier/reference search, supported smali normalization and existing selector aliases. |
| Asset diagnostics | Compact skipped counts, stable diagnostics ID, and opt-in skipped-file details. |
| Call-graph semantics | Labelled callback-creation and possible interface/virtual dispatch edges with bounded coverage; these do not establish framework delivery. |
| Networking | Named API usage, supported Retrofit services and structural hook maps; client identity and interceptor execution order remain unanalysed. |
| Navigation | Manifest/resource XML and supported Hotwire destination evidence. Compose type-safe registration analysis is absent. |
| Logical WebView inventory | Supported load/eval sites, local receiver/client/factory evidence, caller context and containing Activity/Fragment ancestry. Static groups are distinct from runtime objects. |
| Large APKs | Explicit bounded Standard/Large GUI profiles; headless open_apk still uses the default profile. |

## P1 implementation batch

### P1-01 — Validate call-graph roots

Status: implemented and locally validated on feature/mcp-p1-correctness (2026-11-15). Cross-platform verification is tracked by the commit checks.

Problem: a complete but incorrect method signature can pass normalization without declaration validation. `root_body="not_loaded"` supplies some information, but the query can still succeed with zero matches and the batch can report zero failed queries.

Scope: validate selectors in the shared call-graph path so single and batch calls agree. For a missing loaded declaration, return a per-query `SYMBOL_NOT_FOUND` error with bounded same-name candidates. Batch `failed_queries` must count the error. Preserve valid external/framework targets, inherited aliases, abstract declarations and methods without bodies; absence of a loaded body alone is not an invalid-root proof.

Batch `complete` may retain its documented meaning that all inputs were processed, including errors. It must not represent unresolved queries as successful zero-hit queries. Resolution, traversal coverage and pagination remain separate.

Acceptance checks:

1. A wrong parameter-package or return descriptor on a loaded owner produces a bounded candidate-bearing error.
2. A batch containing valid and invalid roots reports successful data and explicit errors independently, with accurate failure counts.
3. A valid declared method with no callers returns a genuine zero result.
4. Referenced external/framework methods and supported inherited aliases remain queryable.
5. Existing pagination, byte budgets, cancellation and batch-completion semantics remain intact.

Baseline evidence: https://github.com/Ch0pin/rdx/blob/6d6804d4434218fcd51d169c2af36d964d69baee/src/method_lookup.rs#L187 and https://github.com/Ch0pin/rdx/blob/6d6804d4434218fcd51d169c2af36d964d69baee/src/mcp_navigation.rs#L505.

### P1-02 — Expose bounded load profiles to headless MCP

Status: implemented and locally validated on feature/mcp-p1-correctness (2026-11-15). Cross-platform verification is tracked by the commit checks.

Problem: headless `open_apk` uses the default project loader and cannot request the already implemented Large profile. This blocks large APKs even when the GUI's explicit higher-limit retry can load them.

Scope: expose an explicit validated Standard/Large profile through headless `open_apk`, reusing the existing limits. Preserve Standard as the default, report the effective profile, and define request-key reuse/retry behavior when the requested profile differs. Keep the profile local to the requested project. Retain typed failures and full-project integrity when the selected limits are exceeded.

Acceptance checks:

1. Omitted options retain the current Standard limits and behavior.
2. Explicit Large loading reaches the existing bounded loader and reports its effective profile.
3. Invalid profiles and conflicting request-key reuse receive clear errors; no silent profile switch occurs.
4. Tests exercise a limit failure and explicit higher-profile success with small controlled fixtures or injected limits, rather than requiring multi-gigabyte CI fixtures.
5. Existing GUI shared-project loading, cancellation and complete-project guarantees remain intact.

Disk spilling and partial-project loading are P4 follow-ups if the existing bounded profiles prove insufficient; they are not part of this P1 batch.

Baseline evidence: https://github.com/Ch0pin/rdx/blob/6d6804d4434218fcd51d169c2af36d964d69baee/src/project_snapshot.rs#L26 and https://github.com/Ch0pin/rdx/blob/6d6804d4434218fcd51d169c2af36d964d69baee/src/load_limits.rs.

## Remaining core backlog

| ID | Priority | Request | Status and decision |
| --- | --- | --- | --- |
| CORE-01 | P2 | Compact filtered instance discovery | Missing. Suppress known-unrelated details by default, retain compact counts and distinguish unknown registration identities from proven nonmatches. |
| CORE-02 | P2 | Malformed identifier-query feedback | Missing targeted hints for mixed forms such as `android.webkit.WebView;->loadUrl`. Preserve existing valid smali normalization and explain owner-qualified name-only matching. |
| CORE-03 | P2 | Named-networking hook maps | Partial. Unify structural maps with named API records; add supported renamed builder/proceed roles and safe observer templates. Some standard-namespace stacks already match structural detection. |
| CORE-04 | P2 | Interface-to-concrete hook targets | Partial. Existing implementor/dispatch queries need a deduplicated target map separating declaring methods from possible receiver classes. Runtime receiver identity remains observed. |
| CORE-05 | P2 | Search within one huge method | Missing. Add method-scoped regex search with bounded line windows and explicit source/coverage status. Full method export already exists. |
| CORE-06 | P3 | Class-source member anchoring | Missing. Useful for surrounding class context; isolated method lookup already supports class/name selection. |
| CORE-07 | P3 | Explicit runtime class identity | Partial. Package manifests have descriptors; add concise runtime-name metadata to exports/source identity. No synthetic defpackage insertion was found in RDX; the reported attribution is unconfirmed. |
| CORE-08 | P3 | Character-limit aliases and parameter guidance | Existing aliases and accepted-parameter errors remain. Add max_chars only where limit measures characters; define class filtering separately from package prefixes. |
| CORE-09 | P3 | Short-name reference lookup latency | Unrevalidated APK-specific report. Measure cold and warm queries before changing indexes. |
| CORE-10 | P3 | Multi-query asset search | Missing. Scan each asset once, group hits by query and share coverage/diagnostics. |
| CORE-11 | P3 | Class-literal search guidance | Add targeted guidance toward find_type_references for X.class/const-class queries. Existing type-reference indexing needs no replacement. |
| CORE-12 | P4 | Background source-search jobs | Defer pending demonstrated need beyond existing resumable searches. |

## Bounded analysis-plugin backlog

| ID | Priority | Request | Status and acceptable scope |
| --- | --- | --- | --- |
| ANALYSIS-01 | P2 | Semantic guard summaries | Missing. Prove supported comparison wrappers including null behavior and boolean arguments, with exact helper IDs and evidence. Existing forwarder detection is not this analysis. |
| ANALYSIS-02 | P2 | Compose type-safe navigation inventory | Missing. Recover supported registrations, target classes, arguments, serializer-derived IDs and extras forwarding. Registration is not proof of external reachability. A missing raw const-class xref is a separate P1 bug only after reproduction. |
| ANALYSIS-03 | P3 | Empty methods / React Native prop setters | Missing. Identify proven empty/no-effect bodies and map supported ReactProp values. Ignoring parameters alone does not establish a no-op or security defect. |
| ANALYSIS-04 | P3 | Provider graph (R3) | Missing. Trace supported fields/factories/caching wrappers. Shared-instance claims require object-identity proof; two DoubleCheck providers need not share an instance. |
| ANALYSIS-05 | P3 | HTTP client composition (R4) | Partial registration evidence exists. Begin with supported local builder identity/order/service bindings, then extend through DI. Report host comparisons and credential writes with evidence rather than blanket host-gating verdicts. |
| ANALYSIS-06 | P3 | Backward provenance to Url arguments (R5) | Missing as a general analysis. Return constants, parameters, Intent data, fields, config and unknown origins. Attacker control requires ingress and validation evidence. |
| ANALYSIS-07 | P3 | LiveData / ActivityResult / Koin relationships | Missing general models. Add supported registration/delivery candidates with separate inferred-edge coverage; creation alone does not prove delivery. |
| ANALYSIS-08 | P3 | Logical WebView inventory extensions | Partial. Add supported cross-method wiring, bounded callers and focused observer generation. Keep runtime observation IDs separate from static group IDs. |
| ANALYSIS-09 | P4 | Annotation-stripped Retrofit fallback | Investigate original DEX annotations and actual service construction first. Missing JADX imports plus an available URL handler do not prove stripping or usage. |
| ANALYSIS-10 | P4 | Request-key-to-transport inventory | Consider a bounded inventory of occurrences, candidate writers, dispatch alternatives and native boundaries. It cannot promise all runtime/native serialization routes. |

## Scope corrections

The RN WebView documentation marks mediaCapturePermissionGrantType as iOS-only. An empty Android setter may be intentional: https://github.com/react-native-webview/react-native-webview/blob/master/docs/Reference.md#mediacapturepermissiongranttype.

Navigation route IDs must follow the bundled implementation. Current AndroidX combines the serializer descriptor name and element names: https://github.com/androidx/androidx/blob/androidx-main/navigation/navigation-common/src/commonMain/kotlin/androidx/navigation/serialization/RouteSerializer.kt.

Standard Retrofit reads annotations and rejects missing HTTP-method declarations. A structural fallback needs actual DEX/service-construction evidence: https://github.com/square/retrofit/blob/master/retrofit/src/main/java/retrofit2/RequestFactory.java.

P5 as written: guaranteed enumeration of every native/runtime transport, universal host-gating verdicts without complete evidence, or automatic attacker-control labels based only on source categories. The bounded versions above remain eligible.

Recommended order: P1-01 and P1-02 first; then CORE-01/CORE-02; then unified hook targets and proven guard summaries. No P2–P5 implementation is authorized by the current P1 batch.

## P1 validation record — 2026-11-15

Sol (gpt-6.1-sol, high reasoning) implemented both P1 items; supervising source review is complete. Local checks passed: 897 library tests, 188 GUI tests, 6 MCP stdio/transport tests, the final failed-launch retry regression, formatting and all-target Clippy with warnings denied.

The original OKX APK was also tested through the branch binary's public headless MCP interface in an isolated registry. Standard failed at the 1 GiB retained-allocation limit while parsing classes5.dex; explicit Large reached Running with 287,024 classes. Both attempts reported the selected profile correctly. The APK SHA-256 was 5bbbbbd5055140e6c735953fd05fb01dbe07ffb0843891c42f878a8c8cd47560.

Unresolved external/inherited-reference statuses are separate from DEX scan completeness. They retain observed reference counts without certifying the declaration. Local implementation and validation did not replace the installed app. No P2–P5 changes were made.
