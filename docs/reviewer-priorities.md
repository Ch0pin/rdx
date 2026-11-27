# Reviewer request priorities

Reconciled: 2026-11-27. Current baseline: `735e44808865c024d5e73ad6d1819f3201855246` on main. The three approved November 25 batches were merged in `1f7d6ad840519368de80c50c16d0d67734f31b68`; follow-up Windows CI corrections are included. Previous reconciliation baseline: `5cf161b4ffb9fc824ad9faed48d7f8b84014322f` (2026-11-16). Original implementation audit baseline: `6d6804d4434218fcd51d169c2af36d964d69baee`.

This is the consolidated RDX backlog for the original November 1–10 notes and subsequent requests supplied through November 27, including the Snapchat query feedback. Statuses include all three verified implementation batches; deployed binaries should be checked by commit or executable hash. The reconciliation inspected source, schemas and recorded validation, and includes controlled Splitwise and exact-build Zappos reproductions below. The exact Wells Fargo constructor cases were reproduced and fixed; the Telegram Wallet case was not rerun. Medusa-specific proposals are outside this backlog. An open entry records agreed scope and priority; it does not authorize implementation.

## Priority definitions

| Priority | Meaning |
| --- | --- |
| P0 | Emergency requiring immediate action. None established by this audit. |
| P1 | Incorrect conclusions or a blocked primary workflow. |
| P2 | High-value improvement after P1 correctness fixes. |
| P3 | Useful, bounded enhancement. |
| P4 | Investigate or defer until evidence supports the investment. |
| P5 | Decline the proposal as written; a narrower evidence-producing version may be worthwhile. |

## Current P0/P1 decision — 2026-11-27

The November 16 audit reviewed all 18 RDX note blocks in `/Users/ch0pin/Desktop/Git-Projects/Agents/Codex/tooling-improvements.txt`, including the mutation-to-hook-map entry then at line 14 under the Medusa heading. Its input SHA-256 was `32ad339a695d691d53b0a88256e4de2bd812f4b1d1b384e8e03c1a7006a1fd98`. This reconciliation also covers the latest user-supplied KBC, Wells Fargo, Telegram Wallet and Splitwise notes. The external source-notes file was left unchanged.

| Priority/status | Decision | Evidence and action |
| --- | --- | --- |
| P0 | None established | No remaining RDX report demonstrates an emergency such as data loss or a widespread unusable release. |
| P1 implementation queue | No confirmed outstanding item | P1-01 and P1-02 are merged. The exact KBC loading failure is fixed. The reported Splitwise response mix-up is explained by inherited methods on the inspected build; see TRIAGE-02. The Snapchat batch/single discrepancy is closed as unconsumed input-method pagination; see TRIAGE-03. |
| Closed investigation — TRIAGE-01 | Zappos raw WebViewActivity type reference is present | Exact reported APK reproduced on 2026-11-16: 57 references, complete coverage, including ProductScreenNavigationKt at DEX offset 0x0089. No current P1 defect reproduced. Supported Compose interpretation is now available as an opt-in inventory. |
| Closed investigation — TRIAGE-02 | Splitwise receiver queries resolve to an inherited method | Local Splitwise 26.11.1 / 972: 12 concurrent queries match serial results. Both reported subclasses inherit onReceive from ContentSharedViaBroadcastReceiver. Method-source responses now separately identify requested and declaring classes. |
| Closed investigation — TRIAGE-03 | Snapchat batch and single call graphs agree | Exact APK and original saved response inspected on 2026-11-27. All six roots agree after pagination; the first response explicitly deferred the last two roots with next_method_offset=4. |
| Closed loading failure — KBC | Declared trailing DEX link data | Commit bcd897f loads the exact APK with 11 DEX files and 124,733 classes under Standard limits. General acceptance of undeclared trailing bytes is outside this fix. |
| Closed P1 — P1-01 | Missing call-graph roots | Merged in `b5c40975a5be954f35efe76734dd97a42055bd2f`; single and batch queries explicitly report invalid loaded-method signatures. |
| Closed P1 — P1-02 | Headless large-APK loading | Merged in the same commit; `open_apk(headless=true, load_profile="large")` is supported, with explicit limits and request identity checks. |

### TRIAGE-01 — closed after exact-build reproduction

Source report: `/Users/ch0pin/Desktop/Git-Projects/Agents/Codex/tooling-improvements.txt:110`. The supplied `/Users/ch0pin/Desktop/BugBounty/mango-db/com.zappos.android.apk` matches the original APK SHA-256 `2c2b51eb7a08762c8a197e54280f17c772a982ac2a50838d398d5bb300442344`; decoded manifest confirms version 15.10.1 / 259360970.

A temporary private MCP process used the installed current build (binary SHA-256 `1a4d98ff827453b086f3bb05292ad0071b7ab489815635e69ab581a3e8d77939`). It was shut down after validation; no persistent duplicate project was left running.

1. `find_type_references(class_name="com.zappos.android.activities.WebViewActivity", limit=100, offset=0)` returned all **57 references in one page**, with `coverage_complete=true`, `index_complete=true`, `exhausted=true`, `next_offset=null`, and zero skipped classes. The cold query took 0.685 seconds.
2. The result includes `com.zappos.android.compose.product.ProductScreenNavigationKt.productDetails$lambda$5(Lgd/a;Lgd/p;Lcom/zappos/android/product/analytics/ProductDetailsTracker;LI3/I;Lgd/l;Lgd/l;Lcom/zappos/android/navigation/NavBarItem;Lud/c;LI3/c0;)LRc/S;` in `classes3.dex`, code-unit offset **0x0089 (137)**.
3. Class disassembly shows `0089: const-class v11, com.zappos.android.activities.WebViewActivity`. Independent inspection of the original ZIP member at file-byte offset 4,923,578 confirmed instruction words `0x0b1c, 0x190e`; resolving the DEX type/string tables yields `Lcom/zappos/android/activities/WebViewActivity;`.
4. **Disposition:** the missing raw reference is not reproduced on current RDX. No production fix is required for this case. The historical invocation/version is unavailable, so its original cause is not established. The later verified opt-in Compose inventory now recognizes this registration and its descriptor-derived ID.

Local evidence: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/zappos-type-reference-20261116/report.json`, `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/zappos-type-reference-20261116/raw-dex-verification.json`, and `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/zappos-type-reference-20261116/probe.py`. The probe records pinned identities, complete page metadata and clean process exit.

November 16 implementation evidence: [type-reference query](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/mcp_discovery.rs#L62), [instruction type references including opcode 0x1c](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/identifier_search.rs#L679), [invalid-root regression](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/mcp.rs#L3231), and [headless load-profile regression](https://github.com/Ch0pin/rdx/blob/5cf161b4ffb9fc824ad9faed48d7f8b84014322f/src/mcp_indexing_tests.rs#L474).

### TRIAGE-02 — inherited Splitwise method, not a reproduced cache fault

Inspected `/Users/ch0pin/Desktop/BugBounty/mango-db/com.Splitwise.SplitwiseMobile.apk`, version 26.11.1 / 972, SHA-256 `0cf24232bf514095d0eddf604c26179fc54bc0f5839c66c42722f653d009f9e9`. The reviewer supplied no Splitwise APK hash, so this finding applies to the available local build.

`com.Splitwise.SplitwiseMobile.services.CodeSharedViaBroadcastReceiver` and `com.Splitwise.SplitwiseMobile.services.ReminderSentViaBroadcastReceiver` extend `com.Splitwise.SplitwiseMobile.services.ContentSharedViaBroadcastReceiver`. They override `logSharingApp`; neither declares `onReceive`. All three queries correctly resolve to `com.Splitwise.SplitwiseMobile.services.ContentSharedViaBroadcastReceiver.onReceive(Landroid/content/Context;Landroid/content/Intent;)V`.

One private headless project and three lightweight MCP clients exercised 12 concurrent calls in rotated class order. Every response matched its serial counterpart and echoed the declaring method_id. All four owned processes exited successfully and their temporary registry was removed. The verified API update now exposes requested and declaring classes and inherited resolution alongside the resolved method_id. A genuine mismatch for unrelated declarations would warrant a new P1 investigation.

The same probe found 219 class-level `dev.enro.annotations.NavigationDestination` usages, already including `values.key.kind="type"` and the key descriptor in `values.key.value`. This establishes annotation-value support, not complete coverage of generated Enro bindings; the preset remains ANALYSIS-12. It also verified an unmatched export fails with `No classes match package_prefix`, and exact package-plus-hash discovery selects the unique healthy project when the full registry fits one batch. Scope suggestions remain CORE-17. The subsequent resolve_instance query verifies exact package/hash uniqueness across registry pages within explicit budgets.

Evidence: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/reviewer-audit-20261125/report.json` and `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/reviewer-audit-20261125/probe.py`. Baseline implementation: [method selector](https://github.com/Ch0pin/rdx/blob/b303a207fbe19c225fa100cd35e9845aa2733539/src/mcp_review.rs), [annotation values](https://github.com/Ch0pin/rdx/blob/b303a207fbe19c225fa100cd35e9845aa2733539/src/annotation_values.rs), [export admission](https://github.com/Ch0pin/rdx/blob/b303a207fbe19c225fa100cd35e9845aa2733539/src/mcp_exports.rs), and [discovery selection](https://github.com/Ch0pin/rdx/blob/b303a207fbe19c225fa100cd35e9845aa2733539/src/mcp_feedback.rs).

### TRIAGE-03 — Snapchat call-graph batch parity

The reported Snapchat 14.33.1.0 / 329222 APK has SHA-256 `6a8a0d169fe62602d54ef5a7e79ac287e56bbc5efb6a7b29c50c6f5a9ab0a43a`. Reused the existing loaded instance `f4ac1ed7c5b0567a5103773736cd7b12e3cd4bd9a77efe2784ef17ce43e3bd5c`, project `5a90bb2acfee13a24c11a19905b6f3ebf052a8199e1248babfdb58372bc2dbb5`, with executable SHA-256 `91a15b9aec632358a7d2ed3096d54b8032316f1b6e8a04a0ada5a6e229951911`. No additional project was loaded and no app/device state was changed.

The original saved response at `/Users/ch0pin/.claude/projects/-Users-ch0pin-Desktop-Git-Projects-Agents-Codex/6e9b9921-54ce-405a-81e5-206a026cc48f/tool-results/mcp-rdx-get_call_graph_batch-1795730685953-n4p8i6.txt` contains four roots, `complete=false`, `next_method_offset=4`, and zero failed queries. It does not contain zero-result records for the other two roots. The handoff treated unprocessed roots as empty results.

Repeated the original six fully qualified selectors with callers/depth 2, followed the input-method continuation, and compared complete call-site arrays against single queries with identical options. Results were identical for all roots: u8g0.a=7, bnb.K=4, ev80.r=20, p9x0.a=6, ckl0.a=2, m9x0.invoke=0. Both batch pages reported successful root resolution and complete DEX traversal; every per-root next_offset was null. A zero direct-call result does not establish runtime unreachability through interface or callback dispatch.

Disposition: no P1 graph-correctness defect reproduced; no production change required. Improve compact presentation and explicit per-root processed/pending status under CORE-20. Evidence with exact selectors, options, response arrays and identity: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/snapchat-batch-parity-20261127/report.json` and `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/snapchat-batch-parity-20261127/responses.json`.

### Why the other requests stay below P1

| Notes | Priority retained | Reason |
| --- | --- | --- |
| Member anchoring, batching, runtime identity, framework relationships, DI/client/provenance and WebView extensions | P3 | Existing source export, paginated queries and direct references remain usable. Keep bounded scopes and unknown-result reporting below. |
| Source-search 20-second budget (R6) | Implemented | Existing synchronous cursor paging remains available. On-demand start_source_search now automatically consumes source pages with live progress, cancellation, bounded disk-backed results and cumulative coverage diagnostics. Storage limits return an exact first-unpublished-page continuation. |
| Stripped Retrofit, all native transports, complete mutation write-surface mapping | P3/P4 for bounded inventories; P5 for completeness guarantees | Missing Java hooks or decompiler annotations do not establish all runtime paths or a static engine defect. The mutation request is captured as ANALYSIS-11 below. |

The initial reconciliation changed only this document; all thirteen approved items across three batches have now been implemented and locally verified, and their entries have been removed. Local library, GUI, MCP/CLI regressions, linting and a release build passed. Cross-platform CI passed for main commit 735e448: https://github.com/Ch0pin/rdx/actions/runs/45663319203. Zappos, KBC and Splitwise evidence retains its explicit build and date boundaries. Other APK-specific reports remain unverified where stated.

## Current implementation foundations

| Capability | Verified scope including the working-tree batches |
| --- | --- |
| Annotation usages and Retrofit recovery | Class, field, method and parameter annotations with decoded values, including class-literal descriptors; supported renamed Retrofit roles; service rows and supported local creation/base-URL links. The Enro annotation-value request is already covered. |
| Source search | Synchronous resumable cursors plus on-demand background jobs with automatic continuation, live progress, disk-backed result pages, cancellation and cumulative coverage. The 20-second source-scan limit is a page budget. search_method_source adds line-oriented regex within one method, bounded context windows, exact resumable cursors, a shared-budget one-document cache, and explicit reconstruction/scan coverage. |
| Source access and references | Class/name method selection for source and call graphs, with overload candidates and preserved neighbor-filter semantics. Class-source max_chars and export class_id aliases are supported. Method responses expose requested/declaring classes and inherited resolution. Type references offer full or compact sites and optional owner/kind groups with occurrence counts and nonmethod owners. |
| Exports | Background/resumable package export accepts a scalar prefix or a bounded, deduplicated prefix union in one worker. Legacy scalar checkpoint semantics and manifest.jsonl class_id/descriptor/source_path mappings are preserved; unmatched scopes and changed resume identities fail explicitly. Future schema extensions should be compatible or explicitly versioned. |
| Manifest queries | exported_only=true is accepted by both manifest tools; false adds no restriction and preserves explicit exported_state. Conflicting true/false or true/unresolved filters are rejected. |
| Instance discovery | Identity-filtered discovery defaults to compact related diagnostics, with full details opt-in. resolve_instance checks exact package/hash uniqueness across registrations within bounded time/work; duplicates, uncertain identities, incomplete scans and registry changes prevent selection. Loading/Failed projects remain explicit diagnostics. |
| Identifier query guidance | Malformed mixed owner/smali syntax produces actionable examples. Exact owner-qualified method-name queries include overloads; valid smali descriptors and arbitrary source-mode text retain their semantics. |
| Asset diagnostics | Compact skipped counts, stable diagnostics ID, and opt-in skipped-file details. |
| Call-graph semantics | Labelled callback-creation and possible interface/virtual dispatch edges with bounded coverage. get_hook_targets separately pages possible loaded receiver classes and deduplicated declaring implementation methods, preserving unresolved/default-interface cases and runtime identity uncertainty. |
| Networking | Named API usage and supported Retrofit services remain available. Opt-in hook_maps unifies named and structural candidates with per-role proof/status and a bounded project cache. Named observer templates emit origin, allowed method/header names and credential-presence booleans; incomplete header enumeration is explicit. Prefixes select map declaration owners with guidance for excluded library maps. Client composition remains separate. |
| Semantic guard summaries | get_guard_summary proves supported small String/boolean wrappers from original DEX, preserving null behavior, operand order and boolean modes. Exact caller offsets expose bounded argument/resource provenance. The optional Semantic guard summary plugin presents the proof for humans; unsupported behavior remains unknown. Packaged resource values and helper-body semantics are distinct from runtime observations. |
| Navigation | Manifest/resource XML and supported Hotwire evidence remain the default. include_compose=true adds bounded DEX-backed typed registrations, route and target classes, serializer-derived IDs, ordered arguments and observed navigator Bundle copying. Kotlin lazy bindings are explicitly modeled; launch values and external reachability remain unresolved. Per-method, global-work, record and caller-association limits are explicit. |
| Constructor reconstruction | Ctorless, fieldless, static anonymous Kotlin function-reference adapters can recover an observed exact loaded superclass constructor after metadata, access, receiver/alias, control-flow and exception-boundary checks. Java expression lowering preserves allocation/argument order. Unsupported patterns retain DEX fallback. Both reported exact Wells Fargo methods now render native Java. |
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

## Additional completed requests

| ID / request | Status and evidence |
| --- | --- |
| CORE-12 — Background source-search jobs | Completed in [938e373](https://github.com/Ch0pin/rdx/commit/938e373c93267a44d878606a19dca2673f8e9f18). Explicit start/status/results/cancel/release, shared parsed project, bounded disk spooling and exact continuation. Coverage, cancellation, retention and headless cleanup are tested. Moved out of the open backlog; its former P4 priority is historical. |
| KBC DEX link-data loading | Completed in [bcd897f](https://github.com/Ch0pin/rdx/commit/bcd897f3d9a20a588b0ef76185ffcb910faf0677). Exact APK SHA-256 `4e94ec3a5477cf50faa0a4aa72a2f7f240e04dc624223a4d57a735a6aba69c62` loaded 11 DEX files and 124,733 classes with Standard limits. Declared link bounds are validated; ordinary data reads cannot enter the opaque link section. Evidence: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/kbc-link-data-20261123/headers.json` and `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/kbc-link-data-20261123/after.json`. |
| KBC relocated icon resources | Completed in the same commit. File-backed resources resolve renamed/root-level paths; ordinary strings retain text semantics. Actual KBC icon rendering and resource regressions passed. Evidence image: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/kbc-link-data-20261123/kbc-icon.png`. |
| Boolean exported-only manifest filter | Completed in [b303a20](https://github.com/Ch0pin/rdx/commit/b303a207fbe19c225fa100cd35e9845aa2733539). Both manifest tools advertise exported_only; boolean validation, conflicts, counts and paginated result parity are tested. Evidence: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/allocation-completion-20261012/careful-resume/exported-only-final-result.json`. |

Already-supported annotation values, method-ID echoing and empty-export errors are recorded in the foundations/TRIAGE-02 sections. They are not duplicate implementation tasks. The open entries below describe only remaining work.

The November 27 argument-schema requests are already supported or documented: get_method_source advertises format=auto|dex in its input schema; get_dex_strings is explicitly scoped to the DEX containing a selected class. For APK-wide literal references use find_string_references; search_code(mode=source, regex=true) searches reconstructed text and does not enumerate the raw DEX string pool. These are not duplicate implementation tasks. Parent-directory convenience remains CORE-22.

## Remaining core backlog

| ID | Priority | Request | Status and decision |
| --- | --- | --- | --- |
| CORE-06 | P3 | Class-source member anchoring | Missing. Useful for surrounding class context; isolated method lookup already supports class/name selection. |
| CORE-07 | P3 | Explicit runtime class identity | Partial. Package manifests have descriptors; add concise runtime-name metadata to exports/source identity. No synthetic defpackage insertion was found in RDX; the reported attribution is unconfirmed. |
| CORE-09 | P3 | Short-name reference lookup latency | Unrevalidated APK-specific report. Measure cold and warm queries before changing indexes. |
| CORE-10 | P3 | Multi-query asset search | Missing. Scan each asset once, group hits by query and share coverage/diagnostics. |
| CORE-11 | P3 | Class-literal search guidance | Add targeted guidance toward find_type_references for X.class/const-class queries. Existing type-reference indexing needs no replacement. |
| CORE-17 | P3 | Suggestions for an empty export scope | Current export already rejects zero matching classes. Add bounded package counts and manifest component-package suggestions to the error, labelled as candidate scopes. An applicationId is not necessarily a Java package prefix; never silently broaden an export. |
| CORE-19 | P2 | Recommended candidate pin and reachable-match count | Add recommended_pin and reachable_exact_match_count with explicit inspected-registration scope, readiness and supplied-identity strength. Preserve exact_match_unique and discovery_complete independently. A sole package-only or first-page candidate is not proof of global uniqueness or an installed APK match. Existing resolve_instance(package_name, apk_sha256) already handles strict selection. |
| CORE-20 | P2 | Compact batch call graphs and explicit per-root summaries | Add a compact caller/callee/depth projection and concise per-root counts, resolution, error and processed/pending status without implying that a withheld root has zero edges. Preserve DEX call-site multiplicity or explicitly label grouped/deduplicated edges. Existing matching_total, root_resolution_complete, per-root next_offset, next_method_offset and serialized-data budgets remain authoritative. Document the exact .data.results[].data.call_sites[] envelope and both continuation levels; correct the stale 2 MiB paragraph to the implemented default 65,536-byte data budget and distinguish MCP/client framing. |
| CORE-21 | P3 | Exact declared-method filter for implementations | Optional declares_method (name plus full descriptor) or equivalent compact match flag over loaded declarations. Keep declared-on-class semantics distinct from inherited implementation resolution and expose incomplete coverage. get_hook_targets already resolves exact virtual method selectors into concrete declaring targets and receiver pairs, avoiding per-class method-list fan-out for runtime hook mapping. |
| CORE-22 | P3 | Optional export parent creation | Add an explicit create_parents option for export_source, preserving new output-directory semantics, no-overwrite checks and clear errors for invalid parents. Current errors already identify the missing parent path. Do not reinterpret an out_path ending in source.txt as a single-file export without an explicit API contract. |

## Bounded analysis-plugin backlog

| ID | Priority | Request | Status and acceptable scope |
| --- | --- | --- | --- |
| ANALYSIS-03 | P3 | Empty methods / React Native prop setters | Missing. Identify proven empty/no-effect bodies and map supported ReactProp values. Ignoring parameters alone does not establish a no-op or security defect. |
| ANALYSIS-04 | P3 | Provider graph (R3) | Missing. Trace supported fields/factories/caching wrappers. Shared-instance claims require object-identity proof; two DoubleCheck providers need not share an instance. |
| ANALYSIS-05 | P3 | HTTP client composition (R4) | Partial registration evidence exists. Begin with supported local builder identity/order/service bindings, then extend through DI. Report host comparisons and credential writes with evidence rather than blanket host-gating verdicts. |
| ANALYSIS-06 | P3 | Backward provenance to Url arguments (R5) | Missing as a general analysis. Return constants, parameters, Intent data, fields, config and unknown origins. Attacker control requires ingress and validation evidence. |
| ANALYSIS-07 | P3 | LiveData / ActivityResult / Koin relationships | Missing general models. Add supported registration/delivery candidates with separate inferred-edge coverage; creation alone does not prove delivery. |
| ANALYSIS-08 | P3 | Logical WebView inventory extensions | Partial. Add supported cross-method wiring, bounded callers and focused observer generation. Keep runtime observation IDs separate from static group IDs. |
| ANALYSIS-09 | P4 | Annotation-stripped Retrofit fallback | Investigate original DEX annotations and actual service construction first. Missing JADX imports plus an available URL handler do not prove stripping or usage. |
| ANALYSIS-10 | P4 | Request-key-to-transport inventory | Consider a bounded inventory of occurrences, candidate writers, dispatch alternatives and native boundaries. It cannot promise all runtime/native serialization routes. |
| ANALYSIS-11 | P3 | Mutation-to-runtime-hook mapping | Recorded in the November 16 review from the source note then at line 14. Link supported DEX methods to proven mutation/friendly-name/doc-ID constants, known caller evidence and concrete hook selectors. Report unresolved bindings and bounded dispatch coverage. Hook silence does not certify the complete write surface. |
| ANALYSIS-12 | P3 | Enro NavigationKey/destination preset | Annotation usages already return annotated owners and decoded class-literal values. Add a bounded plugin/preset presenting that mapping and joining supported enro_generated_bindings evidence. Report missing annotations/bindings and coverage explicitly; destination registration alone does not establish an external entry path. |
| ANALYSIS-13 | P3 | Remote WebView shell identity snapshots | Plugin/assessment-workflow feature. Record externally observed appVersion, entry/bundle URLs, content hashes, capture time and relevant variant context separately from pinned APK identity. Preserve snapshots for diff/revalidation and report web-build drift. Automatic retrieval must be explicit and avoid persisting credentials; a static APK hash cannot certify a remotely deployed web bundle. |

## Scope corrections

The RN WebView documentation marks mediaCapturePermissionGrantType as iOS-only. An empty Android setter may be intentional: https://github.com/react-native-webview/react-native-webview/blob/master/docs/Reference.md#mediacapturepermissiongranttype.

Navigation route IDs must follow the bundled implementation. Current AndroidX combines the serializer descriptor name and element names: https://github.com/androidx/androidx/blob/androidx-main/navigation/navigation-common/src/commonMain/kotlin/androidx/navigation/serialization/RouteSerializer.kt.

Standard Retrofit reads annotations and rejects missing HTTP-method declarations. A structural fallback needs actual DEX/service-construction evidence: https://github.com/square/retrofit/blob/master/retrofit/src/main/java/retrofit2/RequestFactory.java.

P5 as written: guaranteed enumeration of every native/runtime transport, universal host-gating verdicts without complete evidence, or automatic attacker-control labels based only on source categories. The bounded versions above remain eligible.

The KBC reproduction contains declared link_data, not unexplained trailing bytes. General tolerant-tail loading, unparsed-tail diagnostics and strict/tolerant modes are deferred pending a distinct unsupported-layout reproduction. Preserve ordinary data bounds and reject malformed link sections. This case provides no evidence for weakening validation or marking partially parsed projects complete.

Constructor-argument staging must preserve observable order. Java allocates the object before evaluating constructor arguments; moving effectful preparation into statements before new can change failure/side-effect ordering. See https://docs.oracle.com/javase/specs/jls/se26/html/jls-15.html#jls-15.9.4 and the existing allocation-lowering tests. The verified function-reference fix retains these requirements and does not broaden recovery to arbitrary effectful instructions.

## Remaining implementation order

The thirteen approved items across three batches were removed after implementation, supervising inspection and independent public-MCP verification. No confirmed outstanding P0/P1 is established. The Snapchat P1 investigation is closed by exact parity evidence. The remaining inventory contains 2 P2 entries, 17 P3 entries and 2 P4 entries. Completed CORE-12 is excluded.

Prioritize CORE-19 and CORE-20 for the new usability work. Remaining P3/P4 entries retain their scope; Enro and remote-web features remain separately scoped plugin/workflow work. These remaining entries require separate implementation authorization.

Verification evidence for the removed batch: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/mcp-usability-20261125/root-review/candidate.json`. On the pinned Splitwise APK, alias exports matched byte-for-byte, graph filters retained their behavior, inherited owners were explicit, compact/grouped references agreed, 219 annotation class owners were retained, and a three-class prefix-union export preserved its manifest mapping and enforced resume identity. Equal-sized warm queries measured 1.031 seconds for full and 1.005 seconds for compact output; eight site records shrank from 5,448 to 3,511 JSON bytes. The corrected array-component matcher accounts for the additional site versus the previous seven-site result. Grouped pages explicitly stream the requested scope; ordinary compact pages reuse the existing index or its established bounded streaming fallback.

Second-batch evidence: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/mcp-second-batch-20261125/root-review/discovery-probe-report.json` and `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/mcp-second-batch-20261125/root-review/api-probe-report.json`. A private 21-registration fixture verified late-page exact selection, duplicate/timeout/registry-change refusal, and compact diagnostics (1,836 versus 10,838 bytes). On exact Zappos 15.10.1, the caller at DEX offset 0x26 proves rd.E.l uses case-sensitive equality with false as its boolean argument and packaged resource `.zappos.com`; the plugin presents the same result and null behavior. On local Splitwise, an interface-root query returned three concrete app interceptor installations, inherited receiver methods deduplicated correctly, and named OkHttp maps resolved with cache reuse. Observer redaction and failure behavior passed local JavaScript mocks; no runtime hooks were installed.

Second-batch local validation passed 938 library tests, 196 GUI tests, 3 CLI export tests, 1 shutdown test, 2 MCP stdio tests and 4 indexing integration tests, plus exact-APK tests, plugin Python checks, packaging checks, strict all-target Clippy, formatting and a release build. These are local results, not cross-platform CI claims.

Final P2 evidence: `/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/p2-final-20261125/root-review/public-api-report.json` verifies method search against the original DEX text (36 matching lines across 13 pages, cache reuse and changed-query cursor rejection) and exact Zappos navigation. Nine registrations include WebView route ID -787805064, target WebViewActivity, url/sizing arguments, and ProductScreenDestination graph ID 874828579. IDs use the bundled descriptor hash; modeled serializer bindings, observed Bundle copying, incomplete outer-graph relationships and external reachability remain explicit.

`/Users/ch0pin/Desktop/Git-Projects/rdx/target/validation/p2-final-20261125/core18/final-report.json` pins Wells Fargo APK SHA-256 `57dee92a1510f99747b1ff3767fcf1a03749a63eb0fd0f0ea4cf51a6c914d944`. Both g91.invokeSuspend and NavigationActivity.r0 changed from allocation-constructor mismatch fallback to native Java with no fallback reason. The cause was an erased Kotlin adapter constructor and a direct superclass initializer, rather than unsupported argument effects alone. Executed generated-Java fixtures verify class-initialization/getter/constructor order, thrown-object identity and alias identity; malformed/escaping/boundary-crossing variants are rejected. Local library, GUI, native reconstruction, MCP/CLI, plugin, packaging, strict lint and release checks passed. Cross-platform CI subsequently passed at main commit 735e448: https://github.com/Ch0pin/rdx/actions/runs/45663319203.

## P1 validation record — 2026-11-15

Sol (gpt-6.1-sol, high reasoning) implemented both P1 items; supervising source review is complete. Local checks passed: 897 library tests, 188 GUI tests, 6 MCP stdio/transport tests, the final failed-launch retry regression, formatting and all-target Clippy with warnings denied.

The original OKX APK was also tested through the branch binary's public headless MCP interface in an isolated registry. Standard failed at the 1 GiB retained-allocation limit while parsing classes5.dex; explicit Large reached Running with 287,024 classes. Both attempts reported the selected profile correctly. The APK SHA-256 was 5bbbbbd5055140e6c735953fd05fb01dbe07ffb0843891c42f878a8c8cd47560.

Unresolved external/inherited-reference statuses are separate from DEX scan completeness. They retain observed reference counts without certifying the declaration. Local implementation and validation did not replace the installed app. No P2–P5 changes were made in that November 15 P1 batch; later completed work is recorded above.
