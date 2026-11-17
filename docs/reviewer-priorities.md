# Reviewer request priorities

Reviewed: 2026-11-16. Current baseline: main commit `5cf161b4ffb9fc824ad9faed48d7f8b84014322f`. Original implementation audit baseline: `6d6804d4434218fcd51d169c2af36d964d69baee`.

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

## Current P0/P1 decision — 2026-11-16

Reviewed all 18 RDX note blocks in `/Users/ch0pin/Desktop/Git-Projects/Agents/Codex/tooling-improvements.txt`, including the mutation-to-hook-map entry at line 14 under the Medusa heading. Medusa/Frida-only entries were excluded. Input SHA-256: `32ad339a695d691d53b0a88256e4de2bd812f4b1d1b384e8e03c1a7006a1fd98`. The source notes were preserved unchanged.

| Priority/status | Decision | Evidence and action |
| --- | --- | --- |
| P0 | None established | No remaining RDX report demonstrates an emergency such as data loss or a widespread unusable release. |
| P1 implementation queue | No new confirmed item | Previous P1-01 and P1-02 are merged. The remaining notes primarily request new analysis coverage or workflow improvements. |
| Closed investigation — TRIAGE-01 | Zappos raw WebViewActivity type reference is present | Exact reported APK reproduced on 2026-11-16: 57 references, complete coverage, including ProductScreenNavigationKt at DEX offset 0x0089. No current P1 defect reproduced. Compose interpretation remains ANALYSIS-02 (P2). |
| Closed P1 — P1-01 | Missing call-graph roots | Merged in `b5c40975a5be954f35efe76734dd97a42055bd2f`; single and batch queries explicitly report invalid loaded-method signatures. |
| Closed P1 — P1-02 | Headless large-APK loading | Merged in the same commit; `open_apk(headless=true, load_profile="large")` is supported, with explicit limits and request identity checks. |

### TRIAGE-01 — closed after exact-build reproduction

Source report: `/Users/ch0pin/Desktop/Git-Projects/Agents/Codex/tooling-improvements.txt:110`. The supplied `/Users/ch0pin/Desktop/BugBounty/mango-db/com.zappos.android.apk` matches the original APK SHA-256 `2c2b51eb7a08762c8a197e54280f17c772a982ac2a50838d398d5bb300442344`; decoded manifest confirms version 15.10.1 / 259360970.

A temporary private MCP process used the installed current build (binary SHA-256 `1a4d98ff827453b086f3bb05292ad0071b7ab489815635e69ab581a3e8d77939`). It was shut down after validation; no persistent duplicate project was left running.

1. `find_type_references(class_name="com.zappos.android.activities.WebViewActivity", limit=100, offset=0)` returned all **57 references in one page**, with `coverage_complete=true`, `index_complete=true`, `exhausted=true`, `next_offset=null`, and zero skipped classes. The cold query took 0.685 seconds.
2. The result includes `com.zappos.android.compose.product.ProductScreenNavigationKt.productDetails$lambda$5(Lgd/a;Lgd/p;Lcom/zappos/android/product/analytics/ProductDetailsTracker;LI3/I;Lgd/l;Lgd/l;Lcom/zappos/android/navigation/NavBarItem;Lud/c;LI3/c0;)LRc/S;` in `classes3.dex`, code-unit offset **0x0089 (137)**.
3. Class disassembly shows `0089: const-class v11, com.zappos.android.activities.WebViewActivity`. Independent inspection of the original ZIP member at file-byte offset 4,923,578 confirmed instruction words `0x0b1c, 0x190e`; resolving the DEX type/string tables yields `Lcom/zappos/android/activities/WebViewActivity;`.
4. **Disposition:** the missing raw reference is not reproduced on current RDX. No production fix is required for this case. The historical invocation/version is unavailable, so its original cause is not established. Recognizing the existing reference as a type-safe Compose navigation registration remains a separate P2 feature.

Local evidence: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/zappos-type-reference-20261116/report.json`, `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/zappos-type-reference-20261116/raw-dex-verification.json`, and `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/zappos-type-reference-20261116/probe.py`. The probe records pinned identities, complete page metadata and clean process exit.

Current implementation evidence (pinned to this audit): [type-reference query](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/mcp_discovery.rs#L62), [instruction type references including opcode 0x1c](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/identifier_search.rs#L679), [invalid-root regression](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/mcp.rs#L3231), and [headless load-profile regression](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/mcp_indexing_tests.rs#L474).

### Why the other requests stay below P1

| Notes | Priority retained | Reason |
| --- | --- | --- |
| Filtered discovery noise; mixed identifier-query hints | P2 | Actionable usability gaps, but the notes show a reachable target or successful alternative query. Free-text identifier matching is not a validated method-selector API. |
| Named networking/concrete implementation hook maps; guard semantics; Compose registrations | P2 | Valuable supported analyses. Current results explicitly distinguish structural maps, API evidence and supported navigation sources; missing interpretations are separate from a proven missing raw reference. |
| Huge-method search, member anchoring, aliases, batching, runtime identity, framework relationships, DI/client/provenance and WebView extensions | P2–P3 | Existing source export, paginated queries and direct references remain usable. Keep the bounded scopes and unknown-result reporting below. |
| Source-search 20-second budget (R6) | Implemented | Existing synchronous cursor paging remains available. On-demand start_source_search now automatically consumes source pages with live progress, cancellation, bounded disk-backed results and cumulative coverage diagnostics. Storage limits return an exact first-unpublished-page continuation. |
| Stripped Retrofit, all native transports, complete mutation write-surface mapping | P3/P4 for bounded inventories; P5 for completeness guarantees | Missing Java hooks or decompiler annotations do not establish all runtime paths or a static engine defect. The mutation request is captured as ANALYSIS-11 below. |

This review changes prioritization/documentation only. Existing tests were inspected; the prior green cross-platform run covers the baseline. TRIAGE-01 was rerun against the exact Zappos APK and independently checked against its raw DEX bytes. Other target APK reproductions were not rerun. The prioritization review itself changed no production code; CORE-12 was subsequently implemented and validated as recorded below.

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
| Large APKs | Explicit bounded Standard/Large profiles in GUI and headless MCP. Standard remains the default; effective profile and conflicting request-key reuse are explicit. |

## Completed P1 implementation batch

### P1-01 — Validate call-graph roots

Status: closed; merged into main in `b5c40975a5be954f35efe76734dd97a42055bd2f` (2026-11-15), with local and cross-platform validation passed. Included in the current baseline.

Original problem: a complete but incorrect method signature can pass normalization without declaration validation. `root_body="not_loaded"` supplies some information, but the query can still succeed with zero matches and the batch can report zero failed queries.

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

Status: closed; merged into main in `b5c40975a5be954f35efe76734dd97a42055bd2f` (2026-11-15), with local and cross-platform validation passed. Included in the current baseline.

Original problem: headless `open_apk` uses the default project loader and cannot request the already implemented Large profile. This blocks large APKs even when the GUI's explicit higher-limit retry can load them.

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
| CORE-12 | P4 | Background source-search jobs | Implemented: explicit start/status/results/cancel/release tools; one active job, four retained jobs, shared parsed project/source cache, bounded disk spool and byte-budgeted retrieval. Focused tests cover Unicode pagination, early-page errors, storage-limit replay, safe cleanup and responsive controls with eight source waiters. |

## Bounded analysis-plugin backlog

| ID | Priority | Request | Status and acceptable scope |
| --- | --- | --- | --- |
| ANALYSIS-01 | P2 | Semantic guard summaries | Missing. Prove supported comparison wrappers including null behavior and boolean arguments, with exact helper IDs and evidence. Existing forwarder detection is not this analysis. |
| ANALYSIS-02 | P2 | Compose type-safe navigation inventory | Missing. Recover supported registrations, target classes, arguments, serializer-derived IDs and extras forwarding. Registration is not proof of external reachability. TRIAGE-01 verified that the reported raw const-class xref is already present in the exact Zappos APK; navigation interpretation remains the feature gap. |
| ANALYSIS-03 | P3 | Empty methods / React Native prop setters | Missing. Identify proven empty/no-effect bodies and map supported ReactProp values. Ignoring parameters alone does not establish a no-op or security defect. |
| ANALYSIS-04 | P3 | Provider graph (R3) | Missing. Trace supported fields/factories/caching wrappers. Shared-instance claims require object-identity proof; two DoubleCheck providers need not share an instance. |
| ANALYSIS-05 | P3 | HTTP client composition (R4) | Partial registration evidence exists. Begin with supported local builder identity/order/service bindings, then extend through DI. Report host comparisons and credential writes with evidence rather than blanket host-gating verdicts. |
| ANALYSIS-06 | P3 | Backward provenance to Url arguments (R5) | Missing as a general analysis. Return constants, parameters, Intent data, fields, config and unknown origins. Attacker control requires ingress and validation evidence. |
| ANALYSIS-07 | P3 | LiveData / ActivityResult / Koin relationships | Missing general models. Add supported registration/delivery candidates with separate inferred-edge coverage; creation alone does not prove delivery. |
| ANALYSIS-08 | P3 | Logical WebView inventory extensions | Partial. Add supported cross-method wiring, bounded callers and focused observer generation. Keep runtime observation IDs separate from static group IDs. |
| ANALYSIS-09 | P4 | Annotation-stripped Retrofit fallback | Investigate original DEX annotations and actual service construction first. Missing JADX imports plus an available URL handler do not prove stripping or usage. |
| ANALYSIS-10 | P4 | Request-key-to-transport inventory | Consider a bounded inventory of occurrences, candidate writers, dispatch alternatives and native boundaries. It cannot promise all runtime/native serialization routes. |
| ANALYSIS-11 | P3 | Mutation-to-runtime-hook mapping | New inventory entry from the note at line 14. Link supported DEX methods to proven mutation/friendly-name/doc-ID constants, known caller evidence and concrete hook selectors. Report unresolved bindings and bounded dispatch coverage. Hook silence does not certify the complete write surface. |

## Scope corrections

The RN WebView documentation marks mediaCapturePermissionGrantType as iOS-only. An empty Android setter may be intentional: https://github.com/react-native-webview/react-native-webview/blob/master/docs/Reference.md#mediacapturepermissiongranttype.

Navigation route IDs must follow the bundled implementation. Current AndroidX combines the serializer descriptor name and element names: https://github.com/androidx/androidx/blob/androidx-main/navigation/navigation-common/src/commonMain/kotlin/androidx/navigation/serialization/RouteSerializer.kt.

Standard Retrofit reads annotations and rejects missing HTTP-method declarations. A structural fallback needs actual DEX/service-construction evidence: https://github.com/square/retrofit/blob/master/retrofit/src/main/java/retrofit2/RequestFactory.java.

P5 as written: guaranteed enumeration of every native/runtime transport, universal host-gating verdicts without complete evidence, or automatic attacker-control labels based only on source categories. The bounded versions above remain eligible.

Recommended order: no confirmed P0/P1 work remains in this note set. P1-01 and P1-02 are implemented, and TRIAGE-01 is closed after exact-build reproduction. Continue with CORE-01/CORE-02 (P2), then unified hook targets and proven guard summaries. This review authorizes no implementation of the remaining backlog.

## P1 validation record — 2026-11-15

Sol (gpt-6.1-sol, high reasoning) implemented both P1 items; supervising source review is complete. Local checks passed: 897 library tests, 188 GUI tests, 6 MCP stdio/transport tests, the final failed-launch retry regression, formatting and all-target Clippy with warnings denied.

The original OKX APK was also tested through the branch binary's public headless MCP interface in an isolated registry. Standard failed at the 1 GiB retained-allocation limit while parsing classes5.dex; explicit Large reached Running with 287,024 classes. Both attempts reported the selected profile correctly. The APK SHA-256 was 5bbbbbd5055140e6c735953fd05fb01dbe07ffb0843891c42f878a8c8cd47560.

Unresolved external/inherited-reference statuses are separate from DEX scan completeness. They retain observed reference counts without certifying the declaration. Local implementation and validation did not replace the installed app. No P2–P5 changes were made.
