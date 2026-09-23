# Install and run RDX

Download an archive from [GitHub Releases](https://github.com/Ch0pin/rdx/releases).
Choose the build for your operating system and CPU, then extract it completely.
Prebuilt releases do **not** require Rust, Java, Python, an Android SDK, ADB, or a
connected device. APK analysis runs locally.

| Download | Runtime requirements | Launch |
| --- | --- | --- |
| `macos-arm64.zip` | Apple Silicon Mac, macOS 13 or newer | Move `RDX.app` to Applications and open it |
| `macos-x86_64.zip` | Intel Mac, macOS 13 or newer | Move `RDX.app` to Applications and open it |
| `windows-x86_64.zip` | 64-bit Windows 10 or newer, graphics driver with OpenGL support | Open `rdx.exe` in the extracted folder |
| `linux-x86_64.tar.gz` | x86-64 Linux, glibc 2.35 or newer (Ubuntu 22.04+), X11 or Wayland desktop and OpenGL support | Run `./rdx` in the extracted folder |

Keep the accompanying notices and documentation when redistributing an archive.
`rdx-source-stats` is an optional sample plugin, not required to launch RDX.

## First launch

**macOS:** this first release is ad-hoc signed, not Developer ID signed or
notarized. After attempting to open it, macOS may require **System Settings →
Privacy & Security → Open Anyway**. See [Apple's instructions](https://support.apple.com/en-gb/102445).

**Windows:** the executable is not Authenticode signed, so SmartScreen may show
an unknown-publisher warning. Check that the download came from this repository;
if your device policy permits it, use **More info → Run anyway**. The release
statically links the MSVC runtime; no separate Visual C++ redistributable is required.

**Linux:** a normal desktop installation usually provides the graphics libraries.
On a minimal Ubuntu system, install:

```sh
sudo apt install libgl1 libegl1 libx11-6 libxcursor1 libxi6 libxrandr2 \
  libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 xdg-desktop-portal
```

The file picker also needs the portal backend appropriate to your desktop
(for example `xdg-desktop-portal-gtk` on GTK desktops). Drag-and-drop or passing
an APK path on the command line can be used without the file picker.
A graphical desktop is needed for the GUI; CLI inspection does not need a window.

## Open a project

Use **File → Open APK / DEX**, drag a file into the window, or pass its path:

```sh
./rdx /path/to/app.apk
```

On Windows use `rdx.exe`; on macOS the CLI is inside
`RDX.app/Contents/MacOS/rdx`. Keep the application in its final location before
copying an MCP client configuration, because that configuration uses its executable path.

## MCP and storage

MCP is optional and included in the executable. Open a project, select
**Tools → MCP Server…**, start the server and copy its client configuration.
You need an MCP-compatible client to use it; no extra server runtime is needed.
The service listens locally and authenticates requests. It uses additional memory
while enabled because its project engine is independent of the GUI engine.

RDX needs writable user-settings and temporary directories. Large APKs require
more memory and temporary disk space; the search index can grow up to 1 GiB.
Individual cache budgets are not a total application memory limit.

## Verify a download

Each release includes `SHA256SUMS`. Compare your archive's SHA-256 with its entry:

```sh
# macOS
shasum -a 256 rdx-v0.1.0-macos-arm64.zip
# Linux
sha256sum rdx-v0.1.0-linux-x86_64.tar.gz
```

```powershell
# Windows PowerShell
Get-FileHash .\rdx-v0.1.0-windows-x86_64.zip -Algorithm SHA256
```

This is alpha software: unsupported methods remain labelled DEX disassembly.
For usage and current scope, see the [user guide](https://github.com/Ch0pin/rdx/blob/main/docs/user-guide.md)
and [MCP tools](https://github.com/Ch0pin/rdx/blob/main/docs/mcp.md).
