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
   `find_implementations`, and `get_dex_strings`. Supplying both requires them to
   resolve to the same class; conflicts return `CONFLICTING_CLASS_SELECTORS`.
   This alias differs from the component-name and call-graph filters also named
   `class_name`. Restarting the server changes project identity;
   stale project requests are rejected.

## Currently available (25 tools)

| Area | Tools |
| --- | --- |
| Instances | `list_instances`, `get_health`, `get_instance_info`, `open_apk` |
| Classes | `get_all_classes`, `search_classes_by_keyword`, `find_direct_subclasses`, `find_implementations` |
| Code search | `search_code` |
| Source and metadata | `get_class_source`, `get_method_source`, `get_class_disassembly`, `get_methods_of_class`, `get_fields_of_class` |
| Resources | `get_all_resource_file_names`, `get_android_manifest`, `get_manifest_summary`, `get_manifest_components`, `get_resource_file`, `get_strings` |
| Call graphs | `get_call_graph` (exact `method_id`, `direction`: `callers` / `callees` / `both`, `depth`: 1–100, default 20) |
| DEX analysis | `find_field_references`, `get_lambda_branch` |
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
Use `direction: "callers"` to trace into a method, `"callees"` (default) to
follow outgoing calls, or `"both"`. Edges always point from caller to callee.

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

The compact projection returns one record per DEX call site, including
`caller_method_id`, `caller_signature`, `callee_method_id`, and
`call_site_offset` in DEX code units. Multiple calls in the same caller remain
separate. `matching_total` counts all matching records; follow `next_offset`
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
`format` (`java` or `rdx-dex`), `fallback_reason`, `source_hash`, `offset`,
`total_chars`, and `next_offset`, alongside the usual APK/project identity.
Enclosing imports and generated helper declarations are available through
`get_class_source`. A DEX fallback retains the complete selected method's
instructions, switch payloads, and exception table.

Omit `limit` to retrieve the whole method. Optional `offset` and `limit` use
Unicode scalar characters relative to this method's source; explicit pages have
a maximum of 128,000 characters. Every page carries the hash of the complete
method. If a complete response exceeds the transport budget, request explicit
pages using the same method and project IDs. Oversized responses report an
error rather than silently truncating source.

## Bounded code search

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
