# RDX user guide

A native Rust desktop application for APK/DEX inspection. The GUI and CLI both use the RDX Native DEX engine. There is no Java worker, JVM dependency, or Java fallback.

RDX is alpha software. Unsupported methods remain explicitly labelled DEX disassembly. See [native engine scope](native-engine.md) for reconstruction details.

## Run from source

Requirements: stable Rust and Cargo. Python 3 is needed only for transport test fixtures. Initial builds download Rust dependencies.

```sh
cargo build --release --bins
cargo run --release
cargo run --release -- tests/fixtures/hello.apk
```

On Linux, install your distribution's C compiler, `pkg-config`, OpenGL, Wayland, and xkbcommon development packages. Windows builds require the Rust MSVC toolchain and Visual Studio C++ build tools; macOS builds require Xcode command-line tools.

To package the macOS app after building (Python 3.11 or newer):

```sh
python3 scripts/package_macos.py
open target/RDX.app
```

## Headless commands

```sh
cargo run --release -- --engines
cargo run --release -- --list tests/fixtures/hello.apk
cargo run --release -- --engine native --decompile tests/fixtures/hello.dex sample.Hello
cargo run --release -- --native-coverage tests/fixtures/hello.apk
```

The default engine is native. `--engine jadx` and `--engine auto` are no longer accepted. Java heap settings and Java runtime environment variables no longer affect RDX. Old saved appearance/search settings remain readable; obsolete heap preferences are ignored.

## Interface

The desktop has project navigation on the left, source and asset tabs in the center, and status/error information below. **File → Open APK / DEX** opens a project; **File → Reload** refreshes it. Cmd+O / Ctrl+O and drag-and-drop are supported. The project tree groups classes, manifest, assets, resources, libraries, DEX bytecode, signatures/metadata, and other files.

**View → Settings** contains interface theme (System, Light, Dark), code theme (Atom One Light, Quiet Light, Solarized light; Ocean, Eighties, Solarized dark, One Dark, Dracula), and font size (10–28 px). Code themes are grouped into Light and Dark; all palettes are bundled for offline use. Changes save automatically across sessions. Settings live at `~/Library/Application Support/rdx/settings.json` on macOS, `%APPDATA%/rdx/settings.json` on Windows, or `$XDG_CONFIG_HOME/rdx/settings.json` (default `~/.config/rdx/settings.json`) on Linux.

**View → Settings → Code font** offers default monospace, JetBrains Mono,
Fira Code and Source Code Pro. Fonts are bundled with Unicode fallbacks; the
choice saves automatically. Click a word in the source viewer to highlight its other exact occurrences.
Find results and navigation highlights remain separate.

Theme and font attribution is in [third_party/themes](../third_party/themes/README.md)
and [third_party/fonts](../third_party/fonts/README.md). Redistributed application
bundles must include these license notices alongside the embedded assets.

The **▾ Open views** menu at the right edge of the tab strip lists every open view by full name, including offscreen tabs. Right-click a tab to copy its name, pin/unpin it, bookmark it, or close views. Pins protect tabs from automatic eviction and **Close Others / Close All**; explicit **Close** still works. Pins and bookmarks last for the current session. The 8-view / 64 MiB admission budget remains enforced; unpin or close a view when pinned tabs prevent opening another.

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

The code viewer displays the complete loaded text without a byte or line cutoff. **Copy source** includes the full loaded document. Up to eight tabs are retained against an estimated 64 MiB tab budget; this is not a total application memory ceiling.

Right-click a project-tree file/class, an open tab, or a viewer and choose **Export…**, then choose a directory. Archive entries export original bytes. Class exports contain the current native representation, including labeled disassembly where Java reconstruction is unavailable. Existing files receive numbered alternatives. Individual exports are limited to 1 GiB.

## Assets

Readable text includes UTF-8 and BOM-marked UTF-16: JSON, XML, HTML, JavaScript, CSS, configuration files, Markdown, and other text. HTML, JavaScript, and SVG are displayed as source, never executed. PNG, JPEG, GIF (first frame), WebP, and BMP have image previews.

Exported manifest component names have a purple highlight and an **Exported
Android component** hover label. Detection uses explicit `android:exported="true"`
and known SDK-dependent defaults; unresolved resource values are not guessed.
Highlighting is available before class loading finishes and preserves class jumps.
It describes the exported flag, independently of enabled state or permissions.

Compiled Android XML, including the manifest, is detected by its binary header and decoded in native Rust directly from the archive. Previews work while class loading is running or if it fails. The native `resources.arsc` index resolves numeric IDs in Java views: the original number stays visible with its resource name and default-value preview. Hover shows the package, configuration and variant count; double-click (or **Go to declaration**) opens all values. Layout/XML paths in that view open decoded files, whose compiled resource references link onward. Back/Forward works across these views. Dense, sparse, 16-bit-offset and compact table entries are supported. Locale/configuration variants are preserved; the viewer does not guess a device-specific selection. Names are shown exactly as stored in the APK, including obfuscated names. IDs absent from the APK, including unavailable framework/split resources, remain numeric. A malformed or unsupported table reports a diagnostic and leaves code navigation available. Other binary data is accessible through hexadecimal previews and original-byte export. Unsupported decoding reports a clear error while preserving the raw preview.

Asset decompression is limited to 8 MiB per preview. Larger entries show their first 4 KiB as hexadecimal. Text retains at most 2 MiB with a truncation notice. Images are limited to 4096 × 4096 pixels and a 64 MiB decoder allocation budget. Archive browsing does not extract files.

## Search

**Search → Find in file…** (`Cmd+F` on macOS, `Ctrl+F` elsewhere) searches the
current class or text asset, including text outside the currently visible scroll area.
Use Enter / Shift+Enter or Next / Previous to move between highlighted matches;
Escape closes the find bar. Match case is optional and queries are literal text.
Project search remains available with `Cmd/Ctrl+Shift+F`.

**View → Word wrap** wraps the viewer to the pane width and saves automatically
across sessions. Line numbers remain source-line numbers, and code links and
copied/exported text retain their original positions and content.

**Search → Class / Code / Methods / Fields** opens a separate search window with one selected scope. Code search examines the native representation, which may include DEX disassembly; it cannot yet reproduce all searches against JADX-generated Java. Definition searches use native class/member metadata. Resources, Comments, case sensitivity, regex, Auto search, and Keep open remain separate options.

**Excluded packages** contains editable defaults for Android, AndroidX, Kotlin, Java, and related namespaces. Remove an entry to include that package, add another pattern, restore defaults, or include all. Preferences save automatically. Nonstandard packages are searched first.

The Node/Code divider is draggable. Auto search and Keep open appear in the bottom bar. Selecting a result brings the main window forward. Results are capped at 1,000 matches / 32 MiB retained matching source; skipped content and errors indicate partial coverage.

The Rust session index uses a private packed temporary file up to 1 GiB, a conservative 96 MiB metadata admission budget, and a 64 MiB RAM tier. Capacity limits are not reserved allocations. The index is removed on project reset and normal shutdown; crashes can leave files for operating-system cleanup. It is not reused across launches.

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
use shading and shadows for a 3D appearance. Full paths leading to Activities,
Services, BroadcastReceivers, and ContentProviders are highlighted, including
intermediate helper calls. Other calls stay visible with subdued edges.

The graph starts at **depth 20**, adjustable from 1 to 100. Change depth and press
**Rebuild**. Use **Zoom**, **Fit graph**, and drag-to-pan to explore larger graphs.
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
