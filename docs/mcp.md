# MCP server

Open **Tools → MCP Server…** in RDX. Open an APK/DEX, select **Start server**,
then **Copy MCP client configuration** and paste that JSON into your client's MCP
configuration. Restart the client connection after changing its configuration.
The server panel shows status, instance/project IDs, APK SHA-256, request count,
current request and errors. **Stop server** disables access without closing RDX.
**Cancel current request** cooperatively cancels active service requests, including
source reconstruction and literal indexing/search.

The native `rdx mcp` command is the stdio gateway. No Python, JVM or separate
server install is required. Each enabled GUI instance has an authenticated
loopback service; the gateway discovers it through a private per-user registry.
The instance backend loads independently of the GUI engine, using additional
memory while enabled. Opening/reloading a GUI project stops that service.

## Agent flow

1. Call `list_instances`, or `open_apk` with an absolute local APK `path` and a
   unique `request_key`. Agent-opened APKs launch new GUI windows and automatically
   enable their service. Repeating a request key on the same gateway connection
   returns the same launch ID; different gateway connections have separate keys.
2. Poll `get_instance_info` until `state` is `Running`. Use its `instance_id` and
   `project_id` on each project tool call. `Loading` and `Failed` are explicit.
3. Get classes, then supply the exact returned `class_id` to source/metadata tools.
   A short class name also resolves when exactly one class matches; responses
   include both `requested_class_id` and the resolved `class_id`. Ambiguous names
   require a fully qualified ID. Class-scoped tools also accept `class_name` as an
   alias for `class_id`: `get_class_source`, `get_class_disassembly`,
   `get_methods_of_class`, `get_fields_of_class`, `find_direct_subclasses`,
   `find_implementations`, `get_class_hierarchy`, `find_instantiations`, and `get_dex_strings`. Supplying both requires them to
   resolve to the same class; conflicts return `CONFLICTING_CLASS_SELECTORS`.
   This alias differs from the component-name and call-graph filters also named
   `class_name`. Restarting the server changes project identity;
   stale project requests are rejected.

A missing FQCN returns `SYMBOL_NOT_FOUND` with JSON-formatted `candidates`,
`candidate_count`, and `candidates_truncated`. Suggestions list loaded classes
with the same exact simple name, sorted by FQCN, capped at 20 IDs and 16 KiB.
For example, `com.baogong.app_login.activity.LoginActivity` can suggest
`com.baogong.app_login.LoginActivity`. Retry explicitly with the intended ID;
suggestions preserve exact FQCN and unique short-name selection rules.

## Currently available (30 tools)

| Area | Tools |
| --- | --- |
| Instances | `list_instances`, `get_health`, `get_instance_info`, `open_apk`, `forget_instance` |
| Classes | `get_all_classes`, `search_classes_by_keyword`, `find_direct_subclasses`, `find_implementations`, `get_class_hierarchy` |
| Code search | `search_code` |
| Source and metadata | `get_class_source`, `get_method_source`, `get_class_disassembly`, `get_methods_of_class`, `get_fields_of_class` |
| Resources | `get_all_resource_file_names`, `get_android_manifest`, `get_manifest_summary`, `get_manifest_components`, `get_resource_file`, `get_strings` |
| Call graphs | `get_call_graph` (exact `method_id`, `direction`: `callers` / `callees` / `both`, `depth`: 1–100, default 20 for graph / 1 for call sites) |
| DEX analysis | `find_field_references`, `get_lambda_branch`, `find_config_key_consumers`, `find_instantiations`, `find_integer_references` |
| DEX strings | `get_dex_strings` (strings from the DEX containing `class_id`), `find_string_references` |

`search_classes_by_keyword` searches literal class-name substrings only.
`search_code` searches generated Java and mixed DEX source using a case-sensitive
literal query; it does not search resources. Resource strings include configurations; binary resources are not exposed.
Class identifiers currently follow the native engine's class-name identity.
Disassembly is `rdx-dex`, not assemblable Smali. Java source can contain explicit
DEX fallbacks. Method IDs retain full DEX signatures.

List responses contain `next_offset`; text responses use Unicode character
`offset` and `next_offset`. Continue until `next_offset` is null. Text paging
only limits tool responses: it does not truncate the GUI viewer. Lists default
to 100 items (maximum 500); text defaults to 32,000 characters (maximum 128,000),
except `get_method_source`, which returns the complete selected method by default.
The same filters and project identity must be retained across pages.
Responses exceeding the 8 MiB transport frame limit return `RESPONSE_TOO_LARGE`
while keeping the connection available. Retry with a smaller page or narrower
filter.

The broader [v1 design](/Users/ch0pin/Desktop/Git-Projects/rdx/docs/mcp-v1.md) includes tools not implemented yet: method
search by method metadata, general X-Refs, main-application helpers,
resource resolution and GUI selection. Those tools
are deliberately absent from `tools/list`. Renaming and debugging are excluded.
The gateway runs up to eight `find_string_references` calls in the background,
so the same MCP connection can send health checks and
`notifications/cancelled` for a pending literal query. Cancellation targets that
request ID; gateway EOF cancels its outstanding literal queries. Other calls
remain sequential, preserving `open_apk` mutation ordering. Cross-connection
open-request deduplication remains future work.

Protocol reference: [MCP lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)
and [tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools).

### Method call graph

`get_call_graph` requires `instance_id`, `project_id`, and an exact DEX
`method_id` from `get_methods_of_class`, such as `sample.Target.doubleValue(I)I`.
It also accepts `sample.Target.doubleValue:(I)I`,
`Lsample/Target;->doubleValue(I)I`, and
`Lsample/Target;->doubleValue:(I)I`. The optional colon immediately before `(`
is normalized while preserving the complete parameter and return descriptors;
selection remains exact.
Use `direction: "callers"` to trace into a method, `"callees"` (default) to
follow outgoing calls, or `"both"`. Edges always point from caller to callee.

An incomplete selector such as `sample.Target.doubleValue` or
`sample.Target.doubleValue(I)` returns bounded same-name declaration candidates,
including their parameter and return descriptors. Candidates come from the
requested class and known superclass chain; constructors use only their own
class. Retry with an exact candidate. No overload is selected automatically.

The response includes numbered nodes, method IDs, distance from the root,
component classifications, available declarations, call-site counts, and
component-path highlights. `truncated` reports the 5,000-node/20,000-edge cap;
`depth_boundary_reached` reports that nodes reached the requested depth.
The graph is returned as one response, without pagination. Caller queries scan
the loaded DEX call sites and may take longer than outgoing queries.

Use `direct_only: true` for immediate neighbors (effective depth 1). An explicit
`depth` greater than 1 conflicts with this option. To retrieve complete caller
IDs and offsets without the node/edge payload, use:

```json
{
  "instance_id": "<instance>",
  "project_id": "<project>",
  "method_id": "sample.Target.doubleValue(I)I",
  "direction": "callers",
  "direct_only": true,
  "projection": "call_sites",
  "limit": 100
}
```

Compact `projection: "call_sites"` defaults to depth 1, so omitted depth returns
only direct neighbors. Graph projection defaults to depth 20. Set an explicit
depth to request transitive sites: each record includes its traversal `depth`,
and `direct_matching_total` counts only depth-1 matches. Method selectors accept
`pkg.Class.method(desc)ret` and `Lpkg/Class;->method(desc)ret`.

The compact projection returns one record per DEX call site, including
`caller_method_id`, `caller_signature`, `callee_method_id`, and
`call_site_offset` in DEX code units. Multiple calls in the same caller remain
separate. Evidence fields include `raw_callee_method_id`, `dex_entry`,
`method_index` (local to that DEX), `invoke_instruction` (readable raw invoke), `invoke_opcode`, `invoke_words` (numeric
16-bit instruction words), and `caller_code_offset_bytes`. The raw operand is
resolved using the caller's own DEX table. `matching_total` counts all matching records; follow `next_offset`
until null. `source_hash` identifies the complete ordered matching record set
and stays stable across pages. Compact enumeration is independent of the graph node/edge caps.
`offset` and `limit` apply only to `projection: "call_sites"` (maximum 500).

Optional filters combine with AND: `package_prefix` matches package boundaries,
`class_name` is an exact FQCN, `annotation` matches a method or class annotation
by descriptor or FQCN, and `component_type` is `activity`, `service`, `receiver`,
or `provider`, based on known DEX ancestry. Filters select neighboring method
declarations after depth-bounded traversal; intermediate methods remain
traversable. Filtered graphs retain the root and edges between retained nodes.
For direct caller queries, filters apply to callers. Compact records preserve
the callee operand declared at the DEX instruction even when hierarchy lookup
resolves it to an inherited implementation.

This is a static graph. It resolves inherited aliases through known superclass
metadata, but does not infer runtime virtual targets, reflection or Intent
routing. External method bodies cannot be expanded. The GUI graph continues
to follow outgoing calls; direction selection is currently an MCP feature.

`find_direct_subclasses` follows immediate superclass edges. Use `find_implementations` with `class_id` to list concrete direct and indirect implementations of an interface or class; it supports the same instance/project scope and offset/limit pagination.

### Class source coverage

`get_class_source` returns `coverage` for the entire class on every page.
`total_methods`, `counts`, and `methods` account for each exact method ID as
`decompiled`, `dex-fallback`, or `omitted`, with `representation` and `reason`.
`omitted_methods` identifies absent standalone declarations. Proven enum sugar
can appear as `source_sugar`; abstract/native declarations can appear as
`declaration_only`. `source_complete` means no methods are omitted;
`java_complete` also requires no DEX fallbacks. This measures declaration presence;
semantic equivalence requires separate verification.

When a method uses DEX fallback or is omitted, source text starts with an
`RDX Java reconstruction incomplete` comment. It reports the counts and up to
20 exact method IDs with concise reasons (16 KiB header budget); coverage keeps
the complete list and full reasons. The same header appears in the GUI source.
Navigation offsets and `source_hash` include the header. Fully reconstructed
classes retain their usual Java header.

For paged text, `returned_chars`, `page_end`, `has_more`, and
`full_text_in_response` describe the returned slice. `full_text_in_response: true`
means this response contains the complete document. Follow `next_offset` until
null and retain `source_hash`; a terminal page starting after zero is still a
slice. Coverage remains class-wide even when only a small source slice is returned.

### Method source

`get_method_source` requires `instance_id`, `project_id`, and an exact `method_id`
from `get_methods_of_class`, including parameter and return descriptors. For
example, `sample.Target.doubleValue(I)I` selects that overload alone.

The response renders only the requested method. It contains `method_id`, `text`,
`format` (`java` or `rdx-dex`), `requested_format` (`auto` or `dex`),
`reconstruction_attempted`, `fallback_reason`, `source_hash`, `offset`,
`total_chars`, and `next_offset`, alongside the usual APK/project identity.

The optional request parameter `format` defaults to `auto`: attempt Java, then
return explicit DEX fallback if reconstruction fails. Use `format: "dex"` to
read native RDX DEX disassembly of the selected method directly. This bypasses
Java reconstruction and reports `format: "rdx-dex"`, `requested_format: "dex"`,
`reconstruction_attempted: false`, and `fallback_reason: null`. An automatic DEX
fallback instead reports `requested_format: "auto"`,
`reconstruction_attempted: true`, and the reconstruction failure reason. RDX DEX
is a diagnostic disassembly format with original instruction offsets, rather
than assemblable Smali. Both modes retain exact method lookup, cancellation,
source hashes, and method-local pagination.
Enclosing imports and generated helper declarations are available through
`get_class_source`. A DEX fallback retains the complete selected method's
instructions, switch payloads, and exception table.
Its first source line explicitly flags Java reconstruction failure with the
exact method ID and a concise reason, before the raw DEX declaration.

Standalone Java keeps fully qualified types; SDK classfile membership and the
loaded hierarchy authorize canonical nested spelling such as `java.util.Map.Entry`.
Binary method/type IDs remain unchanged for navigation and lookup.

Omit `limit` to retrieve the whole method. Optional `offset` and `limit` use
Unicode scalar characters relative to this method's source; explicit pages have
a maximum of 128,000 characters. Every page carries the hash of the complete
method. If a complete response exceeds the transport budget, request explicit
pages using the same method and project IDs. Oversized responses report an
error rather than silently truncating source.

## Bounded code search

Set `pagination: "hits"` to page by occurrences across the APK. `limit` then
bounds matches (default 100, maximum 500), rather than classes. The server scans
until that limit, the end of the filtered class list, or a 20-second budget
checked between class renders. One expensive class may exceed that budget;
cancellation remains cooperative. Resume with **both** `next_offset` and
`next_source_offset`, using the same query, prefix and pagination mode.
`scan_boundary` explains each page boundary; `scanned_classes` and `total_classes`
show progress. `page_error_count` includes all failed renders even when the
bounded `errors` detail list is truncated. This mode reduces round trips;
uncached generated-source search still needs Java reconstruction. For DEX string
literals use `find_string_references`, which indexes literal operands directly.

The default `pagination: "classes"` preserves the previous contract:

`search_code` requires `instance_id`, `project_id` and `query`. Optionally set
`package_prefix` (package boundary match), `offset` (class index), `limit`
(classes to scan, default 25, maximum 500), and `source_offset` (Unicode character
offset inside the first class). Each response contains at most 100 matches.
Resume with both `next_offset` and `next_source_offset`, keeping the query and
package prefix unchanged. Continue until `exhausted` is true, and check every
page's `errors` before interpreting an empty result as absence. A failed class
is reported and does not silently count as successfully searched.

Hits include `class_id`, `source_hash`, Unicode `start`/`end`, one-based `line`,
and a bounded snippet. Source hashes identify generated text; the response
identity's APK SHA-256 identifies the input binary.
An exact decimal or hexadecimal Android resource-ID query also matches its
resolved displayed symbol. The hit range points to that symbol in the returned
source, preserving navigation and pagination after resource-name substitution.

## Instance filters and manifest summary

`list_instances` accepts `apk_path`, `apk_sha256` (exact), and
`package_name` (exact). Bare APK names match filename substrings; paths match
canonical filesystem identity. A legacy basename-only registration requires an
exact `apk_sha256` alongside an absolute-path filter. Filters combine with AND
and apply before pagination.
Loading instances may not have a package name yet. Filtering never replaces
explicit instance/project IDs on subsequent calls.

Discovery scans fixed batches of 16 registrations. Exhaust `next_offset` within
the current batch, then continue with `registration_offset` set to
`next_registration_offset` and `offset: 0`. Keep filters unchanged. `total`
counts matching instances in the current batch; `registered_instances` counts
registry entries. `discovery_complete` describes registration scanning;
`next_offset` describes result pagination. Connection failures appear in
`diagnostics`, including the affected instance and retry guidance.

New registrations include an owner PID. On Unix, discovery automatically removes
only records whose owner is confirmed absent by the OS; unknown, legacy and
reused PIDs are retained. Other platforms retain records conservatively.
For an unreachable stale record, call `forget_instance` with its exact
`instance_id`. Reachable instances are refused. This action removes only the
selected registration, leaving GUI processes and project files intact.

`open_apk.request_key` is required and described in the schema. Its uniqueness
scope is one gateway connection: reuse the same key for retries of the same
canonical path, and choose a new key to launch another window. A different path
with the same key is rejected.

`get_health` accepts the same filters and discovery pagination. It reports
`transport: "connected"`, gateway executable path, version, PID, and executable
SHA-256 captured at process startup, plus instance connection diagnostics.
`get_instance_info` includes the GUI service's build identity. Compare these
identities when diagnosing an older executable or a different running instance.
A failed class query leaves the project in `Running`. Its diagnostic appears
under `last_request_error`, with `tool` and `requested_class_id`; `error` is
reserved for a failed project load. The next successful project request clears
the request diagnostic. Discovery preserves it until then and continues to
report the available project.
For legacy GUI builds that report `Unknown class: ...` in `error` while the
project is `Running`, the gateway preserves it as `selection_diagnostic` with
`source: "legacy_gui"`, clears that stale project error, and supplies reload
instructions. Use the diagnostic and executable/build identities to select the
current GUI instance.

If the gateway advertises a tool but the selected GUI returns `Unknown tool`,
the gateway reports `INSTANCE_TOOL_UNAVAILABLE` with the tool, instance ID,
original GUI error, and `advertised_by_gateway`. Run `get_health` and compare the
gateway/GUI build identities, select the intended running GUI, and reconnect
or restart the relevant process when needed. Different build versions are one
possible cause of this capability mismatch.

A closed client transport requires reconnecting or restarting the MCP connector
before `get_health` can run. An unreachable registration calls for checking the
GUI MCP panel and retrying discovery; it alone does not establish process state.

`get_manifest_summary` defaults to the GUI's Markdown summary with Unicode
character pagination. `format: "records"`, or any filter, selects structured
component and permission records with matching-record offsets and a maximum
`limit` of 500. Filters combine with AND: `component` and `permission` are
literal substrings, `component_type` is exact (`activity`, `activity-alias`,
`service`, `receiver`, or `provider`), and `query` is a case-insensitive substring
of the serialized record. `host` and `scheme` match exact declarations; `path`
matches substrings of declared path attributes. Route predicates must match
within the same intent-filter. Records preserve separate data elements and
relative-filter groups, including their declared allow/block attributes.
`matching_filter_indexes` identifies matching filters. These are declaration
queries; runtime route delivery and callback reachability require separate proof.

Both tools resolve `@string`, `@integer`, and `@bool` references (including
package-qualified names and numeric IDs) before filtering, using the default
configuration. `resource_resolution.attributes` preserves raw references,
resolved values, explicit failure status, and every configuration encountered
through aliases. Alias traversal is bounded at 32 entries and detects cycles. Configuration
inventory admits at most 32 resource IDs and 4096 configurations/declarations;
`configuration_inventory_truncated` marks incomplete inventory. Split-origin
qualifiers retain their declared origin; conflicting values for one qualifier
return `ambiguous_configuration`. Hexadecimal integer resources resolve to signed
decimal values while declared raw representations remain available.
Missing defaults remain unresolved; locale variants never substitute for them.
Configuration overrides are inventory evidence; route filters select defaults.

For records, `projection: "compact"` retains identity, flags, effective permissions, provider authorities, concise
intent-filter constraints, matching-filter indexes and resource resolution.
Compact resource metadata retains relevant route/flag/permission references with
raw value, default status/value and configuration overrides; cosmetic references
and duplicate alias declarations stay available in full projection. The default `"full"` retains
all fields. `fields: "name,exported,intent_filters"` selects an explicit validated
comma-separated field list and overrides projection. Filtering and coverage
precede projection and matching-record pagination remains unchanged.
Compact records omit empty fields and targets equal to the component name.
Permission entries retain effective values, resolution status, source and concise
protection levels; full records retain declaration and catalog details. Resource
metadata belongs to the original declaration subtree; only application attributes
affecting reachability or permissions are inherited by component records.

`format: "exposure"` on an unfiltered summary returns the existing exported
component, deep-link, custom-permission and unresolved-flag sections with Unicode
character pagination. Deep-link literal examples combine direct data declarations
under Android matching semantics, preserving host/port pairing. Patterns without
a literal witness are explicitly labeled as templates. Scheme-only declarations can produce `scheme:example`, using an arbitrary
opaque scheme-specific part allowed by that filter. Examples use declared
hosts and make no domain-verification or runtime-delivery claim.
Exposure resource metadata includes relevant route, permission and flag references
once per original attribute, including their configuration overrides.

### Component lookup

`get_manifest_components` reads the decoded manifest directly, including binary
Android XML in the APK. It returns component records with the same project/APK
identity and matching-record pagination (`limit` defaults to 100, maximum 500).
`source_hash` covers the full filtered record set and remains stable across pages.

Use `class_name: "DeepLinkEntryActivity"` for a case-sensitive substring search.
For exact lookup, set `match_mode: "exact"` and supply an FQCN, `.RelativeName`,
or a simple name relative to the manifest package. Exact target lookup also
returns aliases targeting that activity. Each activity lists its `aliases`;
each alias retains its own permissions, flags and intent filters.

Optional `component_type`, `permission`, `query`, `host`, `scheme`, and `path`
filters combine with AND. Route filters use the same declaration semantics as
`get_manifest_summary` above. For example, `scheme: "https"` and
`host: "www.example.com"` find components declaring both in one intent-filter.

Records include `exported_state` with declared value, effective boolean (or null
when unresolved), and reason. `permission`, `read_permission`, and
`write_permission` describe declared/effective values, inheritance source, and
`resolved` status; read/write fields apply to providers. Alias permission
inheritance follows its target activity. An explicit empty permission clears
inheritance. `effective: null` with `resolved: true` means no permission is
required by that declaration; `resolved: false` means the value or target could
not be resolved. These fields describe manifest configuration.

### Permission protection levels and component coverage

Component permission states include `protection_level` with `status`, `source`,
`raw`, `base`, and `flags` when available. App-defined permissions use their
manifest definition, including the `normal` default when `protectionLevel` is
omitted. Platform permissions use the checked-in Android 16 / API 36 catalog,
tag `android-16.0.0_r1`; catalog metadata includes source URL and SHA-256.
Unknown definitions, resource references, duplicate definitions, conflicting
platform redeclarations, and unknown protection bits remain unresolved.
The catalog describes that pinned platform declaration set; actual device/OEM
policy, grants, signing identity, AppOps, and runtime authorization need separate
verification.

Both manifest tools include `coverage`: unfiltered `total`, `by_kind`, `exported`,
`non_exported`, and `unresolved_exported` counts, with
`scope: "all_manifest_component_declarations"` and `excludes_non_exported: false`.
Non-exported declarations participate in queries. Active `filters` describe any
narrowing; component lookup also reports `matching_components` and `page_complete`.
Exhaust `next_offset` with unchanged filters to retrieve the matching declaration
set. Exported counts describe configuration; callback execution requires runtime
proof.

## DEX field reads and writes

`find_field_references` takes the exact `field_id` returned by
`get_fields_of_class`, retaining its owner, name, and DEX type descriptor.
For example, supply the returned ID in this template:

```json
{
  "instance_id": "<instance_id>",
  "project_id": "<project_id>",
  "field_id": "<field_id_from_get_fields_of_class>",
  "access": "read",
  "offset": 0,
  "limit": 100
}
```

`access` accepts `all` (default), `read`, or `write`. The scan covers original
DEX `iget`/`iput`/`sget`/`sput` variants without Java reconstruction, resolving
provable inherited-owner aliases through loaded declarations and hierarchy.
The query owner is resolved too: `sample.Child.value:I` can resolve to
`sample.Parent.value:I`. Responses preserve `requested_field_id`, expose the
canonical `field_id` and `resolved_field_id`, and enumerate accesses through
both owners. Shadowing fields remain separate. Missing or ambiguous hierarchy
information leaves `resolved_field_id` null and `complete` false; a zero count
in that response is not proof that there are no readers. Equivalent resolved
selectors share the same result-set hash and pagination order.
Each record in `items` includes `method_id`, `symbolic_field_id`, `access`, opcode name
and value, `code_unit_offset` in 16-bit DEX code units, and
`byte_offset_in_method`. `read_count` and `write_count` count confirmed accesses
before the access filter; `counts_complete` covers both counts and
`unresolved_all_access_count` counts relevant unresolved accesses across both
kinds. `source_hash` identifies the full ordered matching reference set and stays
stable across pages. `total` counts filtered occurrences; follow
`next_offset` with unchanged arguments (`limit` maximum 500).

`scan_complete` describes traversal. `complete` additionally requires a unique
loaded query declaration and no unresolved relevant accesses for the selected
filter. Check `declaration_resolved`, `identity_scope`,
`unresolved_access_count`, and bounded `unresolved_examples`: missing, cyclic,
ambiguous, or overly deep hierarchy information can leave relevant operands
unresolved. A symbolic-only query retains exact operand matches with
`complete: false`. Zero confirmed reads establishes absence only within that
reported static scope; reflection, native code, and generated accesses require
runtime analysis. Each page performs a cancellable scan, sharing operand match
tables across classes in the same DEX during that scan.

## Merged lambda branch analysis

Use `get_lambda_branch` for a constructor call of an R8 merged lambda. First
obtain the exact caller ID and constructor `invoke-direct` offset from DEX
call-site records or disassembly. This template uses `0` as an offset placeholder;
replace it with the actual constructor instruction offset in 16-bit code units:

```json
{
  "instance_id": "<instance_id>",
  "project_id": "<project_id>",
  "caller_method_id": "<exact_caller_method_id>",
  "call_site_offset": 0
}
```

A `status: "resolved"` response connects caller constant/move dataflow to the
constructor argument, constructor parameter/move dataflow to a field store,
and dispatcher field/move dataflow to a validated switch. Results include
`constructor_method_id`, allocation offset, dispatcher `method_id`,
`discriminator`, `field_id`, constructor argument/store offsets, selected switch
target, and `provenance`. DEX `blocks` include successor closure, shared
continuations (`shared_with_other_case`), exception edges, and referenced helper
method IDs. Prelude instructions and their exception blocks remain visible.
Each returned DEX instruction includes readable native disassembly text alongside
its code-unit offset and raw words. Selected branch blocks, the executed prelude,
and the prelude exception closure are reported separately.

The proof assumes normal constructor completion, verified DEX final-field
semantics, and no reflective/native field writes. Unsupported or ambiguous
control/dataflow returns `status: "unresolved"` with a reason. A selected static
branch establishes the documented discriminator path; active route registration,
callback execution, attacker-controlled delivery, and runtime impact need
separate evidence.

## DEX literal owners

`find_string_references` requires `instance_id`, `project_id`, and `query`.
`match_mode` is `contains` (default) or `exact`, both case-sensitive.
`package_prefix` restricts results at package boundaries. `offset` counts matching
occurrences; `limit` defaults to 100 and has a maximum of 500. Continue using
`next_offset` with the same query, filters, and project identity.

The project-local index builds lazily in one background worker on the first
request, without Java decompilation. Concurrent literal queries share that build;
health, instance information and other operations remain available. The pending
query returns its ordinary complete result when indexing finishes. It indexes
`const-string`/`const-string/jumbo` instructions and
encoded string values in static fields and class, field, method, and parameter
annotations, including nested arrays and annotations. Results include exact
class/member IDs, occurrence kind, DEX string index, a class identifying that
DEX, and instruction code-unit offsets when applicable. The scope covers literal
owners; dynamically constructed strings and unreferenced pool entries require
other analysis.

`get_instance_info.literal_index` reports `state`, `completed_classes`,
`total_classes`, `progress`, `attempt`, `active_waiters`, `error`, `retryable`, and
`index_complete`. States are `idle`, `indexing`, `cancelling`, `ready`, `failed`,
`cancelled`, and `invalidated`.

Index construction has a 120-second deadline and a conservative 128 MiB
allocation budget. Cancelling a query or closing its connection removes its
waiter; the final departing waiter cancels the build. MCP cancellation
notifications and gateway shutdown also cancel pending literal queries. Failed
or cancelled builds publish no partial index, and the next query retries.
Successful indexes are cached for the running project service. Stopping the
server cancels indexing; a changed APK invalidates the project index and requires
restarting the service. Package filters apply to search results across the
complete project index. For an index-budget failure, use package-scoped
`search_code` as a bounded alternative.

The local service admits at most eight active operations and 32 connections.
`get_instance_info` has reserved operation admission. A saturated operation pool
returns retryable `INSTANCE_BUSY`; a partial request frame occupies only its own
connection. This keeps instance discovery and health checks responsive during a
long first query.

Pages also have a 1 MiB serialized-item byte budget, so a page can contain fewer
than `limit` occurrences. Follow `next_offset` even when a page is short.
Literal previews contain at most 4,096 Unicode characters; `value_chars` and
`value_truncated` describe the full value. Use `get_dex_strings` with the returned
`dex_class_id`, then locate the returned `string_index`, to retrieve full values.

## Dispatch guidance

Call graphs expose `dispatch_targets`: matching virtual method declarations in
known superclass/interface ancestors of the root. `dispatch_targets_truncated`
indicates a bounded ancestor traversal. These are navigation/query suggestions,
not additional call edges or proven runtime targets. Query their callers
separately. The GUI offers the same declarations in a collapsed section.

Component highlights describe ancestry. An accepted service start does not
prove an app callback executed. Constructor/provider callers do not prove a
route is registered; route-table data flow and runtime intent delivery are not
inferred by this static graph.

## Config-key consumers

`find_config_key_consumers` takes `instance_id`, `project_id`, an exact `key`,
`offset` and `limit` (default 20, maximum 100 candidate methods). It scans DEX
literal operands without reconstructing Java. Follow `next_offset` until null.
Pages are also bounded by encoded output size.

Each method includes candidate key/default retrieval calls, `default_raw`,
parsed `default_json`, and arrays identified by JSON Pointer paths such as
`/suffix`. Arrays include their packaged default value (including whether the
path exists and contains an array) and observed operations: `equals`,
`equalsIgnoreCase`, `endsWith`, `startsWith`, `regex_matches` or `regex_find`.
Case-folding/trim transformations, argument word position, exact target,
method ID, DEX offset and static helper call chain accompany each operation.

The query infers retrieval candidates from an exact key passed to a method
with two String parameters and a String return. It does not assert that an
obfuscated method is a remote-config API. `contract` labels this inference;
analysts can inspect `reader_method_id` to verify it. Defaults are literal APK
arguments, not current server values. `default_status` distinguishes parsed
JSON, non-JSON text and unresolved defaults.

Analysis follows normal DEX register flow, supported org.json operations,
and static/direct app helpers up to depth 2. Conflicting joins discard
provenance; unknown calls invalidate mutable JSON-container aliases. Dynamic
keys, field storage, collection adapters and virtual helper dispatch are
outside this coverage. `literal_only`, `analysis_error`, `unresolved_uses` and
`scan_errors` expose those boundaries. Empty operation lists do not prove an
array is unused. Results describe static possible uses, not runtime acceptance
or reachability. No best-effort decompiled text is used as proof.

## Exact superclass metadata

`get_class_hierarchy` returns the queried descriptor, definition availability,
and immediate `superclass` and `interfaces`, each with an exact `class_id` and
DEX `descriptor`. External definitions have unknown parent/interface metadata.
Class-scoped queries accept loaded FQCNs or `Lpackage/Class;` descriptors.
Hierarchy queries additionally accept unique short names across loaded classes
and referenced external parents. Ambiguous short names list candidate FQCNs;
use an exact descriptor to disambiguate across packages.

## Allocation references

`find_instantiations` takes `instance_id`, `project_id`, `class_id` or
`class_name`, `offset` and `limit` (default 100, maximum 500). Select a loaded
or DEX-referenced class using its FQCN, `Lpackage/Class;` descriptor, or a unique
short name. Referenced external classes can be queried without their definition.
Ambiguous short names list candidates.

Results are exact `new-instance` instructions, resolved against each caller's
own DEX type table. Each item contains `method_id`, `dex_entry`,
`allocated_descriptor`, `allocation_offset` in 16-bit code units,
`method_code_offset_bytes`, destination register, raw type index, instruction
words and readable instruction text. `call_site_offset` is an alias for the
allocation offset; it identifies the allocation rather than a constructor call.
The query scans original instructions without Java reconstruction. It skips
payload data, rejects malformed instruction/operand metadata and supports
cancellation independently of the interactive engine lock.

Follow `next_offset` until null; `total` counts all matching instructions across
loaded classes, and `source_hash` identifies the ordered reference set. Pages
also have a 1 MiB item budget. `scan_complete` describes the loaded DEX scan.
Unreachable instructions are included. These sites do not establish successful
construction or runtime reachability; arrays, reflection, native code and other
runtime allocations are outside the query's scope.

## Method lookup suggestions

`get_method_source` accepts canonical `pkg.Class.method(desc)ret` and smali-style
`Lpkg/Class;->method(desc)ret` IDs. An optional colon immediately before the
parameter descriptor is accepted in both forms: `pkg.Class.method:(I)V` and
`Lpkg/Class;->method:(I)V`. This separator is normalized to the canonical ID
without guessing an overload. The response reports the canonical `method_id`
and the original `requested_method_id`. A wrong/missing descriptor returns
`SYMBOL_NOT_FOUND` with JSON-formatted `candidates`, `candidate_count` and
`candidates_truncated` in the error message. Candidates are actual same-name
declarations in the requested class; exact parameter matches rank first when
only the return type differs. At most 20 IDs / 16 KiB of candidate text are
included. Use `get_methods_of_class` for the full list. No candidate is selected
automatically. Unknown names return an empty candidate list. This assistance is
for source-method lookup; call graphs retain their external-symbol semantics.


## Integer constant references

`find_integer_references` takes `instance_id`, `project_id`, `values`, optional
`bit_width` (32 by default, or 64), `offset`, and `limit` (default 100, maximum
500). A batch contains 1–64 signed decimal JSON integers or decimal/hex strings.
Equivalent normalized values are deduplicated. Decimal inputs must fit the signed
range for the selected width. Positive hex strings express raw bit patterns:
`"0xffffffff"` equals -1 at 32 bits and 4294967295 at 64 bits. Negative hex is
accepted within the signed range. Use strings for exact 64-bit values in clients
whose JSON number type cannot preserve them.

Example arguments (replace the instance and project identities):

```json
{"instance_id":"INSTANCE","project_id":"PROJECT","values":["0x02000000",268435456],"bit_width":32,"limit":100}
```

The query scans original DEX once for the whole batch per page, without Java
reconstruction. It matches `const/4`, `const/16`, `const`, and `const/high16` in
32-bit mode, or the four `const-wide` forms in 64-bit mode. Sign extension and
high-bit shifts are decoded before matching. Each result contains the owning
method ID, DEX entry, instruction offset in 16-bit code units, byte offset,
code-item offset, destination register, opcode, raw instruction words, and exact
`value_decimal`/`value_hex` strings. `values` reports full per-value totals,
including zero matches; `total` counts matching instructions, not methods.

Follow `next_offset` until null. Each page has a 1 MiB item budget and a stable
`source_hash` for the ordered reference set. The full scan computes exact totals
even when the page fills. Cancellation and immutable analysis snapshots keep the
interactive engine lock free. Malformed instruction/register data returns an
error with method and offset rather than a successful incomplete result.

These are raw constant loads, including unreachable instructions. DEX bit
patterns can represent integers, floats, doubles or null; no semantic flag use
is inferred. Arithmetic literals, computed constants, encoded field initializers,
and array/switch payloads are outside this query's scope.

## Indexed identifier/API search

Use `search_code` with `mode="identifiers"` for original DEX names and references:

```json
{"instance_id":"INSTANCE","project_id":"PROJECT","mode":"identifiers","query":"parseUri","match_mode":"exact","case_sensitive":false,"symbol_kind":"method","occurrence":"reference","limit":100}
```

The first request builds a project-local postings index directly from DEX;
subsequent queries and pages reuse it. No Java reconstruction is required.
The build runs on the request worker outside the interactive engine mutex.
Concurrent queries share a completed index; waiting and building requests can
be cancelled. Failed or cancelled builds publish no partial index and remain
retryable. Reopening a project creates a fresh cache. The index has a 256 MiB
conservative allocation budget; exceeding it returns an explicit error.

`query` matches a simple name or full original ID. `match_mode` is `contains`
(default) or `exact`; `case_sensitive` defaults to true. Full smali-style class,
method and field IDs are normalized to canonical IDs. `symbol_kind` accepts
`all` (default), `class`, `method`, or `field`; `occurrence` accepts `all`
(default), `declaration`, or `reference`. `package_prefix` filters the owning
class with package-segment boundaries. These filters are specific to identifier
mode; source mode rejects them rather than silently ignoring them.

`offset` and `next_offset` count matching identifier occurrences, with `limit`
default 100 / maximum 500 and a 1 MiB item budget. Omit `pagination`, or set it
to `hits`; `source_offset` is not used. `total` counts the entire filtered result
set. Follow `next_offset` until null; `exhausted` then indicates all matches have
been returned. `index_complete` describes index coverage, independently of page
completion. `index_hash` identifies the immutable index, not rendered source.
`index_cached` and build/wait/total timing fields expose first-use work.

Each hit identifies the symbol, owning class/method/field, DEX entry and context.
Instruction references also contain the exact opcode value, operand index,
raw instruction words, method code-item offset, and `code_unit_offset` (16-bit
DEX code units). Declaration and signature references have no instruction
offset. Only actual instruction operands produce instruction hits; an unused
DEX table entry is not a reference.

Coverage includes class/method/field declarations, superclass/interface edges,
field/return/parameter types, and direct type/field/method instruction operands.
Method operands identify declared targets, including unreachable instructions.
Generated aliases, local-variable names, comments, string literals, annotations,
encoded values, catch metadata, dynamic call sites/handles/prototypes and runtime
dispatch are outside this index. Dynamic instruction sites excluded from the
index are counted explicitly. Use `mode="source"` for generated Java/mixed DEX
text or `find_string_references` for string literals.

### Tool argument errors

Tool failures retain `isError: true` and return a JSON error object in both
`structuredContent` and the text content. Schema-validation failures use
`error.code = "INVALID_ARGUMENT"`; other tool failures use `"TOOL_ERROR"`.
For numeric bounds/type failures, `error.details` includes `tool`, `parameter`,
`received`, `expected_type`, `minimum`, `maximum`, and a correction `hint`.
For example, `get_call_graph` with `limit: 501` reports the valid range 1–500.
A rejected query leaves the gateway available for subsequent requests.
