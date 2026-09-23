# RDX MCP v1 contract

Status: target contract. The server and GUI panel now implement an initial 15-tool subset.
See [current MCP usage and limitations](mcp.md) for the shipped surface; schemas
below describe the broader target, not a claim that every tool is implemented.

## Scope

Native Rust server, sharing RDX's native engine. Agents can connect to existing
GUI instances or open local APKs in new instances. Include read-only JADX MCP
capabilities, resource and DEX strings, hierarchy queries, outgoing calls and
decompilation coverage. Exclude renaming, debugger operations, arbitrary command
execution, and automatic changes to the user's current editor selection.

## Process and routing model

- `rdx mcp` is a stdio MCP gateway. Diagnostics go to stderr.
- Each GUI process hosts an independent local service and registers its endpoint
  with a per-user instance registry. Use loopback TCP for portability, ephemeral
  ports and a random per-process authentication secret. Registry access is limited
  to the current user. Secrets never appear in tool results or normal logs.
- The gateway discovers services, verifies their identities through an authenticated
  handshake and routes every project request by explicit `instance_id`. There is
  no global active-instance switch. An instance ID is never reused after exit.
- One instance has one loaded APK generation. Reloading or replacing the APK
  changes `project_id`; every project request must supply both IDs. A stale project
  fails before engine work. Results always identify the APK SHA-256 and project.
- `open_apk` launches a new RDX instance, including when the same APK is already
  open. Attaching to an existing instance means using its returned IDs. No copy of
  the existing GUI project's mutable engine is shared across processes.

Work runs outside the GUI thread. Use bounded worker queues and cancellation;
snapshot immutable project data, serialize mutable engine access where needed.
An expensive request must not block another instance or GUI interaction.

## Common schema definitions

The following types specify the tool argument and result schemas. Fields are
required unless marked `?`. Unknown argument fields are rejected.

```typescript
type Scope = { instance_id: string; project_id: string };
type Page = { cursor?: string; limit?: number }; // default 100, range 1..500
type ClassRef = { class_id: string }; // opaque ID includes DEX identity
type MethodRef = { method_id: string }; // includes owner and full DEX descriptor
type FieldRef = { field_id: string }; // includes owner, field name and type
type SymbolRef = { symbol_id: string };
type TextPage = { cursor?: string; max_chars?: number }; // default 32000, max 128000
type Identity = Scope & { apk_path: string; apk_sha256: string };
type Result<T> = { identity: Identity; data: T };
type Paged<T> = {
  items: T[]; next_cursor: string | null;
  total?: number; // omitted if not yet known; never estimated as exact
};
type Text = {
  text: string; format: string; source_hash: string;
  start_line: number; next_cursor: string | null;
  reconstruction: "java" | "mixed" | "disassembly" | "not_applicable";
  fallback_methods: { method_id: string; reason: string }[];
};
```

Class, method and field listings return IDs plus display names and raw descriptors.
Methods with the same name are never silently resolved to the first overload.
Duplicate class names in separate DEX files remain independently addressable.
Cursors bind to the instance, project generation, query and source revision;
they cannot be reused with changed filters. Text paging is transport-only: it
does not introduce any GUI source cutoff. All text remains retrievable.

## Tool schemas

Unless stated otherwise, results are wrapped in `Result<T>`. Discovery and opening
return instance records directly because they establish the project scope.

| Tool | Arguments | Data returned |
| --- | --- | --- |
| `list_instances` | `Page` | `Paged<Instance>` |
| `get_instance_info` | `{ instance_id }` | `Instance`, capabilities, index status |
| `open_apk` | `{ path: string, request_key: string }` | `Instance` in loading or ready state |
| `get_all_classes` | `Scope & Page & { package_prefix?: string, role?: "all" \| "application" }` | Classes and exact IDs |
| `get_class_source` | `Scope & ClassRef & TextPage` | `Text` |
| `get_fields_of_class` | `Scope & ClassRef & Page` | Fields and types |
| `get_methods_of_class` | `Scope & ClassRef & Page` | Methods, descriptors and modifiers |
| `get_method_source` | `Scope & MethodRef & TextPage` | `Text`; exact-signature replacement for JADX's name-only getter |
| `get_main_activity_class` | `Scope & Page` | All MAIN/LAUNCHER candidates, alias and target identities |
| `get_main_application_classes_names` | `Scope & Page` | Manifest Application class and discovered inner classes |
| `get_main_application_classes_code` | `Scope & Page & { max_chars?: number }` | Source references and bounded source chunks; per-class continuations |
| `search_classes_by_keyword` | `Scope & Page & { query: string, mode?: "literal" \| "regex", areas?: ("names" \| "code")[], package_prefix?: string, excluded_packages?: string[] }` | Classes, snippets and exact locations |
| `search_method_by_name` | `Scope & Page & { query: string, class_id?: string }` | Matching method IDs and full signatures |
| `get_xrefs_to_class` | `Scope & ClassRef & Page` | Reference sites |
| `get_xrefs_to_method` | `Scope & MethodRef & Page` | Incoming call sites |
| `get_xrefs_to_field` | `Scope & FieldRef & Page` | Reference sites with read/write kind |
| `get_method_callees` | `Scope & MethodRef & Page` | Outgoing call sites and declared targets |
| `find_direct_subclasses` | `Scope & ClassRef & Page` | Direct subclasses only |
| `find_implementations` | `Scope & SymbolRef & Page` | Class or method implementation matches with resolution kind |
| `get_android_manifest` | `Scope & TextPage` | Decoded XML `Text` |
| `get_manifest_component` | `Scope & Page & { kind?: "activity" \| "activity-alias" \| "service" \| "receiver" \| "provider", exported?: boolean }` | Components, explicit/effective exported state and derivation |
| `get_all_resource_file_names` | `Scope & Page & { prefix?: string }` | Resource entries and opaque IDs |
| `get_resource_file` | `Scope & TextPage & { resource_path: string }` | Decoded text or binary metadata; binary content via resource URI |
| `get_strings` | `Scope & Page & { query?: string, configuration?: string }` | Resource strings, names, IDs, values and configurations |
| `get_dex_strings` | `Scope & Page & { query?: string, dex_id?: string }` | DEX string constants with DEX/string-index identity |
| `resolve_resource` | `Scope & { resource_id: string, configuration?: string }` | Names, values, variants, aliases and navigation targets |
| `get_class_disassembly` | `Scope & ClassRef & TextPage` | `Text` with format `rdx-dex`; not labelled assemblable Smali |
| `fetch_current_class` | `Scope & TextPage` | Selected class ID and `Text`, or null if no class selected |
| `get_selected_text` | `Scope` | Selected text, document identity/hash and range, or null |
| `get_decompilation_coverage` | `Scope & Page & { package_prefix?: string }` | Counts, scan status and paginated fallback method/reason records |

`Instance` contains `instance_id`, nullable `project_id` and APK identity while
loading, display name, `state: loading|ready|failed`, capabilities and failure
details if applicable. Readiness is queried through `get_instance_info`.
`request_key` makes retried opens idempotent across gateway connections while the
instance exists. A reused key with a different path fails. Paths must resolve to
local regular APK files; directory traversal within archive resource reads is
not allowed. Opening an APK never replaces an existing project.

Search defaults to literal matching of names and code. Method name search is
literal substring matching. Exclusions are request-local, not changes to GUI
settings. `get_all_classes`' application role uses the same manifest Application
definition as the application helper tools; it does not mean every non-library
class in the APK. Missing manifest information returns an empty result with an
explanation, not a guessed application class.

Reference sites include enclosing method/class IDs, DEX instruction offsets and
source locations when available. Source locations include a source hash. Calls
describe statically declared targets; virtual dispatch is not claimed exhaustive.
Implementation queries report whether an implementation is declared or inherited.
Manifest exported filtering uses effective state when resolvable; unknown states
remain labelled unknown and are excluded from boolean-filtered results.

Coverage reports distinguish methods with code, abstract/native methods, Java
successes, mixed output, fallback and pending work. Coverage never implies
semantic accuracy. Expensive scans can return pending status and continuation;
partial results explicitly state incompleteness. Total counts use the same
definitions as the CLI rather than inventing a second denominator.

## Errors and limits

Use structured tool errors with `code`, `message`, `retryable`, and known scope.
Codes: `INSTANCE_NOT_FOUND`, `INSTANCE_UNAVAILABLE`, `PROJECT_CHANGED`,
`PROJECT_NOT_READY`, `INVALID_ARGUMENT`, `SYMBOL_NOT_FOUND`, `STALE_CURSOR`,
`UNSUPPORTED_FORMAT`, `RESOURCE_NOT_FOUND`, `BUSY`, `CANCELLED`, `INTERNAL_ERROR`.
Invalid protocol parameters remain protocol errors. Never route failed requests
to another available instance. Engine fallback is a successful disassembly result
with reasons, not a fabricated Java success or a generic server error.

Enforce page and text limits, bounded queues and query deadlines. Results larger
than a response budget return continuation metadata instead of silently losing
content. Cancellation affects only the requesting operation, not the GUI project.
Raw project text must be returned as data, never interpreted as server instructions.

## GUI control panel

Per instance: enable/disable MCP access, instance/project IDs, APK identity, status,
connected clients and running requests. Provide cancellation and copy-client-config
actions. Disabling access stops accepting requests and cancels existing requests;
it does not close the APK. Agent-created windows identify their origin. No remote
close-project or stop-GUI tool is included in v1.

## Implementation and acceptance sequence

1. Implement registry, authenticated local services and stdio gateway; validate
   two concurrent instances, reload generation checks, stale registrations,
   idempotent opens and isolated cancellation.
2. Add browsing, disassembly, manifest/resources and GUI context adapters over
   existing engine APIs. Validate overloads, duplicate DEX classes and text paging.
3. Add search, references, hierarchy, strings and coverage adapters. Check paginated
   results against native engine results and retain explicit fallback metadata.
4. Add GUI controls and cross-platform tests. Verify access disabling, responsive
   searching while agents work, resource boundaries and no project cross-routing.

The existing `src/transport.rs` is worker transport, not an MCP implementation.
Keep protocol and registry code in separate modules; reuse the engine APIs in
`src/engine.rs` rather than driving the GUI or adding a second decompiler.
