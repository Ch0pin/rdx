# RDX user guide

A native Rust desktop application for APK, XAPK, APKS, and DEX inspection.

RDX is alpha software. Unsupported methods remain explicitly labelled DEX disassembly. See [native engine scope](native-engine.md) for reconstruction details.

While a project opens, a persistent indexing banner shows its filename, current loading stage, and elapsed time. Archive files can be browsed as soon as they are available; class navigation, code search, and analysis wait for the project index. The banner remains visible while navigating or auto-hiding the toolbar and clears when loading completes or fails. Stages are reported directly; no estimated percentage is displayed.

If a recognized project-loading limit is reached, the GUI offers **Retry with higher limits** or **Cancel**. The standard profile allows 1 GiB of retained parsed-DEX allocations, 250,000 classes and 256 MiB of combined DEX input. The large-APK profile allows 2 GiB, 600,000 classes and 512 MiB respectively. It also allows each resource table to be up to 128 MiB (standard: 64 MiB), and a 512 MiB decoded-resource index (standard: 256 MiB). The combined resource-table input limit remains 256 MiB. Total process RAM can exceed the parser allocation budget. Per-DEX input and temporary-parser limits remain unchanged.

Higher limits require an explicit retry and apply only to the current APK during this session, including reloads of the same path. Opening a different file restores standard limits. If the larger profile also fails, RDX reports the remaining limit; it never raises limits repeatedly or publishes a partial class inventory. Malformed-input and unsupported-format errors use the normal diagnostics. CLI loading keeps standard limits; agents can use the GUI's accepted, shared project through MCP.

## Run from source

Requirements: stable Rust and Cargo. Python is used for packaging and transport test fixtures. Initial builds download Rust dependencies.

```sh
cargo build --release --bins
cargo run --release
cargo run --release -- tests/fixtures/hello.apk
```

On Linux, install your distribution's C compiler, `pkg-config`, OpenGL, Wayland, and xkbcommon development packages. Windows builds require the Rust MSVC toolchain and Visual Studio C++ build tools; macOS builds require Xcode command-line tools.

To package the macOS app after building (Python 3.11 or newer):

```sh
cargo build --release --locked --manifest-path "$PWD/tools/androguard-helper/Cargo.toml"
python3 scripts/package_macos.py
open target/RDX.app
```

The macOS package includes the Androguard helper and its notices. Enable
**View → Settings → Androguard fallback** to inspect unverified alternate output
for methods the native engine could not reconstruct. Expand **Original DEX**
above a reconstructed method to see its diagnostics and original instructions.
Linux release archives include the same optional helper; Windows fallback is
currently unavailable.

## Headless commands

```sh
cargo run --release -- --engines
cargo run --release -- --list tests/fixtures/hello.apk
cargo run --release -- --engine native --decompile tests/fixtures/hello.dex sample.Hello
cargo run --release -- --native-coverage tests/fixtures/hello.apk
```

## Export source to files

Export an entire APK (or another supported input format) and exit, without a GUI
or MCP connection:

```sh
rdx --export-source /absolute/path/app.apk /absolute/path/new-output-directory
```

The output parent must exist. The final directory must be new so existing files
are preserved. Relative input and output paths are also accepted. Optional
`--package-prefix com.example` limits exported classes to that package boundary;
the default includes all classes, including obfuscated top-level packages.
`--engine native` can precede the command.

Progress and errors go to stderr. The final stdout line is a JSON summary with
status, counts, paths and APK SHA-256, suitable for scripts. Files contain Java
where reconstruction succeeds and explicitly labeled DEX where it falls back.
`manifest.jsonl` maps collision-safe numeric class filenames to exact class IDs
and includes each file's SHA-256 and reconstruction coverage.

Exit status `0` means every selected class was written; it does not mean every
method reconstructed as Java. Status `1` means argument/setup/loading failure.
Status `2` means an export started but is incomplete (including integrity or
write errors). Inspect `export-status.json`, `completion.json` and manifest error
records for details. Interruptions such as Ctrl+C retain checkpoints; operating
system signal exit codes apply.

Resume interrupted work with the same APK, executable, directory and package
prefix:

```sh
rdx --export-source /absolute/path/app.apk /absolute/path/new-output-directory --resume
```

Resume verifies committed files before continuing and preserves existing source
files. A changed APK, renderer or class selection is rejected. Previously
recorded per-class failures remain listed; continuation does not automatically
retry them. Use a new directory for a fresh run. For all options:

```sh
rdx --export-source --help
```

The existing `rdx --decompile INPUT CLASS` command still prints one class to
stdout.

## Package formats

Open APK, XAPK, APKS, or DEX files through the GUI, drag and drop, or the CLI. AAB files are not supported. Java is not required. For XAPK and APKS, RDX keeps the base APK and split files under separate paths in the project tree. It requires one base APK and matching package/version identities across selected splits.

For bundletool APKS with `toc.pb`, RDX chooses the highest numbered persistent variant and validates its APK set. APKS exports without `toc.pb` are also supported when they contain one validated base and matching splits. RDX displays an analysis union, including configuration splits; it does not select a device profile or claim that all splits are installed together. XAPK selection validates the manifests inside its APKs; an outer manifest is optional. Preparing either format writes selected inner APKs to temporary disk files, bounded to 2 GiB per APK and 4 GiB total, and releases them with the project. Previews and exports read the selected members on demand.

The default engine is native. `--engine jadx` and `--engine auto` are no longer accepted. Java heap settings and Java runtime environment variables no longer affect RDX. Old saved appearance/search settings remain readable; obsolete heap preferences are ignored.

## Interface

The desktop has project navigation on the left, source and asset tabs in the center, and status/error information below. **File → Open…** opens a project; **File → Reload** refreshes it. Cmd+O / Ctrl+O and drag-and-drop are supported. The project tree groups classes, manifest, assets, resources, libraries, DEX bytecode, signatures/metadata, split APKs, and container files.

**View → Settings** contains interface theme (System, Light, Dark), code theme (Atom One Light, Quiet Light, Solarized light; Ocean, Eighties, Solarized dark, One Dark, Dracula), and font size (10–28 px). Code themes are grouped into Light and Dark; all palettes are bundled for offline use. Changes save automatically across sessions. Settings live at `~/Library/Application Support/rdx/settings.json` on macOS, `%APPDATA%/rdx/settings.json` on Windows, or `$XDG_CONFIG_HOME/rdx/settings.json` (default `~/.config/rdx/settings.json`) on Linux.

**View → Settings → Code font** offers default monospace, JetBrains Mono,
Fira Code and Source Code Pro. Fonts are bundled with Unicode fallbacks; the
choice saves automatically. Click a word in the source viewer to highlight its other exact occurrences.
Find results and navigation highlights remain separate.

**Region screenshot.** Hold `Ctrl` (or `Cmd` on macOS), press the right
mouse button anywhere in the RDX window and drag a rectangle; the live outline shows the
capture size in pixels. On macOS the left button works as well while the modifier is
held, so the gesture also suits one-button mice and trackpads. Releasing captures that region of the window at native
display resolution and opens a PNG save dialog. A region entirely inside the active
document body receives a caption naming its class or file and a matching suggested
name (for example `com.example.LoginActivity.png` or `res_layout_main.xml.png`).
Selections that include the sidebar, toolbar, tab strip, or another part of the
window keep the exact selected bounds and use `RDX-screenshot.png`. The same
default applies when no document is open. If a chosen save name lacks a PNG
extension, RDX asks for the corrected `.png` destination in a second save dialog
before writing. Drags smaller than a few points are ignored, and the right button
without the modifier still opens the code context menu.

Theme and font attribution is in [third_party/themes](../third_party/themes/README.md)
and [third_party/fonts](../third_party/fonts/README.md). Redistributed application
bundles must include these license notices alongside the embedded assets.

The **▾ Open views** menu at the right edge of the tab strip lists every open view by full name, including offscreen tabs. Right-click a tab to copy its name, pin/unpin it, bookmark it, or close views. Pins protect tabs from **Close Others / Close All**; explicit **Close** still works. Pins and bookmarks last for the current session. There is no open-tab count limit.

**Find usages** reuses a reference index built in the background for the loaded
project. Searching a different symbol does not rebuild that index. The results
window distinguishes **Building reference index** from scanning **candidate
classes** to locate exact source references. Large indexes use temporary disk
storage; files are removed when the project is released or RDX closes normally.
The index is not reused across launches. Unsupported bytecode or unavailable
temporary storage can still require a conservative scan to preserve results.

Right-click a class name in the code viewer and choose **Find direct subclasses**.
Results use the Find Usages window layout; click a row to open its class declaration.
The lookup checks immediate DEX superclass relationships across the loaded project,
including references to external parent types. It excludes grandchildren and
interface implementations. Large result sets use the existing explicit partial-result
limits and cancellation controls.

Right-click an interface, class, or method name and choose **Find implementations**.
Type queries list concrete descendants, including implementations inherited through
superclasses and extended interfaces. Method queries list concrete overriding
method declarations and inherited superclass implementations, with exact signature
matching and proven covariant returns. Results jump directly to declarations.
Private/static/final methods and constructors cannot be queried; missing method
declarations produce an explicit message. The scope is the loaded APK, not runtime
reflection or classes loaded dynamically.

Use the toolbar arrows or **Navigate → Back / Forward** (**Alt+Left / Alt+Right**) to revisit reference jumps, including manifest references. A new reference jump clears forward history.

Source tabs show the alpha native engine label. Links are enabled only where native metadata provides a target; unavailable symbol operations remain disabled or report an explicit unsupported operation. Full JADX navigation, usage-analysis, and Java reconstruction parity are still porting work.

The code viewer displays the complete loaded text without a byte or line cutoff. **Copy source** includes the full loaded document. Source documents are shared with interactive cache hits. Above a 128 MiB source-heap target, inactive views use private anonymous file-backed text. Tabs, pins, bookmarks, selection and navigation remain available; the operating system pages text back on demand. The active source and view metadata can exceed this target. A failed disk write preserves the original text and records a diagnostic. Disposable rendered layouts have a separate 64 MiB target. These targets are not a total application memory ceiling.

To select beyond the visible area, hold the left mouse button and drag toward or
past the top or bottom edge of the viewer. It scrolls while extending the selection,
including with word wrap enabled; release the button to stop.

Right-click a project-tree file/class, an open tab, or a viewer and choose **Export…**, then choose a directory. Archive entries export original bytes. Class exports contain the current native representation, including labeled disassembly where Java reconstruction is unavailable. Existing files receive numbered alternatives. Individual exports are limited to 1 GiB.

## Assets

Readable text includes UTF-8 and BOM-marked UTF-16: JSON, XML, HTML, JavaScript, CSS, configuration files, Markdown, and other text. HTML, JavaScript, and SVG are displayed as source, never executed. PNG, JPEG, GIF (first frame), WebP, and BMP have image previews.

Exported manifest component names have a purple highlight and an **Exported
Android component** hover label. Detection uses explicit `android:exported="true"`
and known SDK-dependent defaults; unresolved resource values are not guessed.
Highlighting is available before class loading finishes and preserves class jumps.
It describes the exported flag, independently of enabled state or permissions.

Compiled Android XML, including the manifest, is detected by its binary header and decoded in native Rust directly from the archive. Previews work while class loading is running or if it fails. Java views show inline `package.R.type.name` references when an exact linked Android API establishes the argument as a compatible resource type. Unrelated numeric constants remain numeric even when their values match the resource table. Hover shows the original decimal and hexadecimal ID, package, configuration and variant count; double-click (or **Go to declaration**) opens all values. Layout/XML paths in that view open decoded files, whose compiled resource references link onward. Back/Forward works across these views. Dense, sparse, 16-bit-offset and compact table entries are supported. Locale/configuration variants are preserved; the viewer does not guess a device-specific selection. Names are shown exactly as stored in the APK, including obfuscated names. IDs absent from the APK, including unavailable framework/split resources, remain numeric. Decimal and hexadecimal ID queries also find resolved symbols in file search, project code search and MCP `search_code`, with results pointing to the displayed symbol. A malformed or unsupported table reports a diagnostic and leaves code navigation available. Other binary data is accessible through hexadecimal previews and original-byte export. Unsupported decoding reports a clear error while preserving the raw preview.

Asset decompression is limited to 8 MiB per preview. Larger entries show their first 4 KiB as hexadecimal. Text retains at most 2 MiB with a truncation notice. Images are limited to 4096 × 4096 pixels and a 64 MiB decoder allocation budget. Previewing a member does not extract it; XAPK/APKS preparation uses bounded temporary files for the selected inner APKs.

## Search

**Search → Find in file…** (`Cmd+F` on macOS, `Ctrl+F` elsewhere) searches the
current class or text asset, including text outside the currently visible scroll area.
Use Enter / Shift+Enter or Next / Previous to move between highlighted matches;
Escape closes the find bar. Match case is optional and queries are literal text.
Project search remains available with `Cmd/Ctrl+Shift+F`.

**View → Word wrap** wraps the viewer to the pane width and saves automatically
across sessions. Line numbers remain source-line numbers, and code links and
copied/exported text retain their original positions and content.

**Search → Class / Code / Methods / Fields** opens a separate search window with one selected scope. Code search examines the native representation, which may include DEX disassembly; it cannot yet reproduce all searches against JADX-generated Java. Definition searches use native class/member metadata. Resources, Comments, case sensitivity, regex, Auto search, and Keep open remain separate options. Closing the window clears the query and its results; the scope options are kept for the next search.

**Search → Scan → Workers** sets how many threads a Code search uses to render classes. Auto, the default, uses one fewer than the machine's cores, at most 12, and the menu shows that number. Lower it to keep the machine responsive during a long cold scan; the change applies to the next search and is saved with the other preferences. The status bar shows the count in use while a scan runs.

**Excluded packages** contains editable defaults for Android, AndroidX, Kotlin, Java, and related namespaces. Remove an entry to include that package, add another pattern, restore defaults, or include all. Preferences save automatically. Nonstandard packages are searched first.

Cold code searches use DEX string and symbol references to search likely matches first, then check the remaining source for complete coverage. Uncached classes render on up to twelve workers, leaving a CPU core available on multicore hosts. Finished classes are collected as they complete, so one slow class does not idle the other workers, while results are still reported in scan order. Workers also prepare search filters and compressed cache records. Results remain ordered and can be opened while scanning continues. Cancellation also stops pending rendering. Repeat searches reuse the session source index; adaptive trigram filters skip unrelated source while preserving Unicode matching and numeric resource aliases. A first exhaustive search of a large APK can still take substantially longer than the first results take to appear. Workers reserve estimated rendering memory before starting a class, against a 128 MiB in-flight target. The next required class can exceed the target to guarantee progress. This limits queued work without reducing the normal worker count; it is an estimate rather than an allocator cap.

The Node/Code divider is draggable. Auto search and Keep open appear in the bottom bar. Selecting a result brings the main window forward. **Max results**, in the second options row after Package, is how many matches a scan reports before it stops; the default is 2,000 and the value is saved with the other preferences. At the limit the status reads "Stopped at 2,000 matches (limit)" and the matches found so far stay listed. Raise the value to continue from where the scan stopped, without rescanning what was already covered; the Search button always starts over, and lowering the value changes nothing until the next search. Skipped content and errors indicate partial coverage. Results hold match locations and previews rather than generated source. Memory grows with the configured result limit. Classes waiting behind a slow render retain cache markers or byte-budgeted source; their hit lists are created only when they reach reporting order. Opening a result reloads its source from the session index; the status bar reads "Opening result…" while that happens, or "…after the current search batch" during a running search. If the source has changed since the scan, the result opens on its line and the status says so.

GUI project search and MCP `search_code` share their session source index and native search pipeline. A render completed by either interface can serve the other's next query. MCP keeps case-sensitive literal matching by default; `regex: true` and `case_sensitive: false` expose the matching options, while `scope: "resources"` or `"all"` explicitly includes resource text and decoded XML. MCP uses at most four render workers and a 32 MiB in-flight estimate alongside the GUI's existing worker settings and 128 MiB estimate. Additional MCP source pages wait cancellably, leaving GUI navigation available. A first exhaustive source search still reconstructs uncached classes; warm queries reuse completed renders. Search starts on request and never pre-renders the whole project at startup. See [MCP source search](mcp.md#bounded-code-search) for continuation and resource-coverage fields.

Agents using MCP source search must follow every `next_cursor` as `resume_cursor` with unchanged pinned `instance_id`/`project_id` and matching filters, until `exhausted: true` and no continuation remains. Existing clients can follow both `next_offset` and `next_source_offset`. Honor the requested page size; the page-control `limit` can change between requests. Accumulate errors, truncation and other coverage diagnostics from every page before reporting complete coverage or absence within the requested scope. A zero-hit partial page remains incomplete. `page_status.complete` is page-local; a clean final page cannot erase earlier coverage failures. MCP instructions and page messages guide agents, while clients remain responsible for driving the bounded requests.

The Rust session index uses a private packed temporary file, a conservative metadata admission budget, and a 64 MiB RAM tier. The file and metadata budgets start at 1 GiB and 256 MiB for up to 150,000 entries and grow with the project (16 KiB of source and 2 KiB of metadata per class), so a complete first search of a large APK leaves every class indexed for the rest of the session. Source records use fast compression when it saves space. Search filters remain available when source storage fills, so subsequent searches can still skip unrelated classes. Possible matches without cached source are rendered again. Capacity limits are not reserved allocations. The index is removed on project reset and normal shutdown; crashes can leave files for operating-system cleanup. It is not reused across launches.

## Plugins

Load `plugins/source-stats/plugin.json` through **Tools → Plugins**, explicitly enable it, and run it on an open text tab. The example is a native Rust executable built with `cargo build --release --bins`; place `rdx-source-stats` beside the main application binary. Plugins run as local processes with your user privileges; there is no operating-system sandbox or existing JADX plugin compatibility. The decompilation engine itself runs in native Rust.

See [plugin protocol](plugins.md) and [native engine scope](native-engine.md).

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Native parser/reconstruction tests use checked-in APK/DEX fixtures and do not require Java. Interactive cross-platform GUI verification is separate from compilation. Remaining porting work includes full control-flow reconstruction, type inference, broad Java generation, complete native navigation/usages, and advanced Android resource formats.

The native parser adapts algorithms from pinned JADX sources. Preserve [upstream attribution and notices](../third_party/jadx/README.md). Historical Java-worker performance results do not describe the native engine.

Method references are available from the viewer's **X-Refs → Callers /
Callees** submenu. Results preserve overload signatures, dispatch kinds, and DEX
instruction offsets. Opening a result shows Java when the call has an unambiguous source reference;
otherwise it shows the exact DEX call site. Declaration
navigation opens the normal source view. Virtual/interface references use their
declared DEX targets, not inferred runtime receiver types. Reflection and
invoke-custom targets are not resolved.

### Component-focused call graph

Right-click a method declaration or call and choose **Method references →
Call graph…**. The graph opens in an independent native window that can be
resized or moved to another monitor. All collected methods and call edges are displayed. Circular nodes
use shading and shadows for a 3D appearance, and every edge carries an arrowhead at
the callee. Calls between methods in the same column bow out beside their nodes,
calls that point back to the left arc over the nodes they span, and a recursive
call loops above its own node, so every edge stays in view.

The **Callers / Callees / Both** selector chooses which calls the graph follows.
**Callers**, the default, walks the methods that invoke the selected method and
their callers in turn, so the selected method sits at the right edge and every
arrow flows into it. **Callees** walks the methods the selected method invokes, with
the selected method at the left edge. **Both** combines the two around the selected
method. Changing the selector rebuilds the graph at once.

Declared calls stop where the app hands a callback to the framework: a
`Runnable` lambda's `run()` has no caller in the DEX code, because a thread,
executor or handler invokes it through the interface. The **Lambda creators**
checkbox, available for Callers graphs, follows such callbacks back to the code
that creates them. A method declared by a synthetic, anonymous or `Lambda`-named
class, or by a class extending a Kotlin lambda or continuation base, gains an
edge from every method that allocates that class, and the walk continues from
those creators. These edges are drawn dashed and the status line counts them:
the allocation is exact, the callback relationship is inferred. Declared calls
keep their solid edges. The producer is the `lambda_creation` kind of the MCP
`semantic_edges` option, applied to the first 2,048 traversed methods; the
dispatch producers stay off in the window, and invoke-custom lambdas, reflection,
and framework or Intent delivery remain out of reach. Toggling the checkbox
rebuilds the graph at once.

Callers and Both graphs read the project's invoke index, the same index MCP
caller queries use. The first such graph after opening an APK builds the index
by scanning every class once; later rebuilds at any depth or direction reuse it
and finish in well under a second on large apps. Callees graphs scan only the
bodies they reach and never need the index.

Full paths between the selected method and Activities, Services,
BroadcastReceivers, and ContentProviders are highlighted, including intermediate
helper calls: in a callers graph the paths that start at a component and reach the
selected method, in a callees graph the paths that lead from it to a component.
Other calls stay visible with subdued edges.

Component nodes and the highlighted edges take their color from the manifest
exported state of the component class: **red** when the class, or a declared
subclass, is exported and therefore reachable from other apps, **blue** when every
declaration denies external access, **amber** when the manifest leaves the flag
unresolved or no manifest is available, and grey when no declaration names the class.
Helper methods on a highlighted path, including the selected method itself, carry
the same rim color, so a red rim on the selected method in a callers graph means an
exported component has a static call path to it. Hover a component node for the
manifest lines behind its color, including `activity-alias` declarations and the
declared subclasses of a base class; hover a helper method to see which component
state reaches it. The label inside a component node still names the component kind.
Exported state is a manifest property; it does not prove that the method runs for an
external intent.

The graph starts at **depth 20**, adjustable from 1 to 100. Change depth and press
**Rebuild**. Use **Zoom**, **Fit graph**, and drag-to-pan to explore larger graphs.
Drag a node to move it anywhere on the canvas; edges follow, and **Reset layout**
returns every node to its column. Rebuilding also resets the layout.
Click an available node to open the exact method; hover for its full signature.
Component classification follows superclass ancestry, including custom base
classes, rather than relying on class-name suffixes.

**Cancel** or closing the window stops collection. Graphs have a 5,000-method /
20,000-edge safety limit with a visible partial-graph notice when reached.
Virtual/interface calls use declared targets. Intent destinations, reflection,
runtime dispatch, and invoke-custom calls are not inferred. Missing class ancestry
can leave a component unclassified.

### MCP server

Use **Tools → MCP Server…** to enable local agent access and copy the client configuration.
See [MCP setup and available tools](mcp.md).
