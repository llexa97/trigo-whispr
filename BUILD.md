# Build Instructions

This guide covers how to set up the development environment and build Trigo Whispr from source across different platforms.

## Prerequisites

### All Platforms

- [Rust](https://rustup.rs/) (latest stable)
- [Bun](https://bun.sh/) package manager
- [Tauri Prerequisites](https://tauri.app/start/prerequisites/)

### Platform-Specific Requirements

#### macOS

- Xcode Command Line Tools
- Install with: `xcode-select --install`

#### Windows

- Microsoft C++ Build Tools: Visual Studio 2019/2022 with C++ development
  tools, or Visual Studio Build Tools 2019/2022
- [CMake](https://cmake.org/download/) (must be on `PATH`):

  ```powershell
  winget install Kitware.CMake
  ```

- [Vulkan SDK](https://vulkan.lunarg.com/sdk/home) from LunarG — required to
  build the Vulkan GPU backend (`vulkan-shaders-gen` needs the SDK's headers
  and `glslc`):

  ```powershell
  winget install KhronosGroup.VulkanSDK
  ```

  Open a new terminal afterward so `VULKAN_SDK` is set.

> [!NOTE]
> Windows' 260-character path limit used to break the native Vulkan build in
> most checkouts. Since `transcribe-cpp` 0.1.3 the build works around it
> automatically (it compiles through a short NTFS junction — no admin rights
> or setup needed), so a normal checkout just builds. If you still hit
> path-limit errors, see
> [Windows build fails with path-limit errors](#windows-build-fails-with-path-limit-errors-msb3491--ftk1011--msb6003)
> in Troubleshooting.

#### Linux

- Build essentials
- ALSA development libraries
- Install with:

  ```bash
  # Ubuntu/Debian
  sudo apt update
  sudo apt install build-essential clang libclang-dev libevdev-dev libasound2-dev pkg-config libssl-dev libvulkan-dev vulkan-tools glslc spirv-headers glslang-tools libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libgtk-layer-shell0 libgtk-layer-shell-dev patchelf cmake

  # Fedora/RHEL
  sudo dnf groupinstall "Development Tools"
  sudo dnf install alsa-lib-devel pkgconf openssl-devel vulkan-devel glslc \
    clang clang-devel libevdev-devel \
    spirv-headers-devel spirv-tools-devel glslang \
    gtk3-devel webkit2gtk4.1-devel libappindicator-gtk3-devel librsvg2-devel \
    gtk-layer-shell gtk-layer-shell-devel \
    cmake

  # Arch Linux
  sudo pacman -S base-devel clang libevdev shaderc spirv-headers glslang alsa-lib pkgconf openssl vulkan-devel \
    gtk3 webkit2gtk-4.1 libappindicator-gtk3 librsvg gtk-layer-shell \
    cmake
  ```

## Setup Instructions

### 1. Clone the Repository

```bash
git clone git@github.com:llexa97/trigo-whispr.git
cd Trigo Whispr
```

### 2. Install Dependencies

```bash
bun install
```

### 3. Start Dev Server

```bash
bun tauri dev
```

### 4. Build for Production

```bash
bun run tauri build
```

This compiles a release binary and generates platform-specific bundles (deb, rpm, AppImage on Linux; dmg on macOS; msi on Windows).

### Test meeting capture on macOS

System audio capture (the other participants of a call) uses a Core Audio
process tap and requires macOS 14.6 or later. macOS only sends real audio to
the tap when the app has a **stable code-signing identity**: `bun run tauri dev`
and the plain `bun run tauri build` output are ad-hoc signed, so the meeting
records fine but the system track is digital silence (the meeting pill warns
that the system audio is silent). The microphone track is not affected.

To test with real system audio, build a release bundle and self-sign it:

```bash
bun run tauri build
bash scripts/macos-selfsign.sh
open "src-tauri/target/release/bundle/macos/Trigo Whispr.app"
```

The script creates a free self-signed `Trigo Whispr Dev` identity in your login
keychain the first time (macOS may ask for your password to trust it), signs
the bundle with the hardened runtime and `src-tauri/Entitlements.plist`, then
runs `codesign --verify --deep --strict`. Pass another bundle path as the first
argument, or set `SIGN_IDENTITY` to use an existing identity (e.g. a Developer
ID). Re-run it after every rebuild: it reuses the same identity (and asks again
to trust it if you declined), and signs by its SHA-1 hash. If several
identities share the name, it stops and lists their hashes so you can delete
the extra ones (`security delete-identity -Z <hash>`).

On the first meeting recording, macOS asks to allow system audio recording
with the `NSAudioCaptureUsageDescription` text from `src-tauri/Info.plist`.
Allow it; you can change it later in System Settings → Privacy & Security →
Screen & System Audio Recording → System Audio Recording Only. To be asked
again (for example after switching signing identity):

```bash
tccutil reset AudioCapture com.trigo.trigo-whispr
```

For a quick check, press `Cmd+Shift+D`, open Debug settings, and run the
10-second meeting capture test while playing audio and speaking. The report
links to a temporary directory with separate `mic.wav` and `system.wav` files
(mono, 16 kHz), padded to the same duration. The system mode should be
`core_audio_tap`. This captures the default output, including the app's own
sounds. Default-output changes are detected and the stream reopened.

Use the Mac speakers or another output-only device for this test. The current
CPAL backend opens a microphone stream on duplex devices (some USB interfaces
and Bluetooth headsets), so these are rejected for system capture to avoid
mistaking microphone audio for system audio. A failed system capture leaves the
microphone available and reports `system_audio_unavailable`; the detailed reason
is logged.

## Linux Install (from source)

The raw binary (`src-tauri/target/release/trigo-whispr`) cannot run standalone — it needs Tauri resource files (tray icons, sounds, VAD model) to be co-located at the expected path.

**Install from the deb bundle** (works on any Linux distro):

```bash
cd /tmp
ar x /path/to/Trigo-Whispr/src-tauri/target/release/bundle/deb/Trigo-Whispr_*_amd64.deb data.tar.gz
tar xzf data.tar.gz
sudo cp usr/bin/trigo-whispr /usr/bin/
sudo cp -a usr/lib/. /usr/lib/
sudo cp -r usr/share/icons/hicolor/* /usr/share/icons/hicolor/
sudo cp usr/share/applications/Trigo-Whispr.desktop /usr/share/applications/
```

The runtime libraries live in the app-private `/usr/lib/Trigo-Whispr/` (on the binary's rpath), so no `ldconfig` step is needed.

After subsequent rebuilds, copy the binary and any refreshed runtime libraries:

```bash
sudo cp src-tauri/target/release/trigo-whispr /usr/bin/
sudo mkdir -p /usr/lib/Trigo-Whispr
sudo cp -a src-tauri/transcribe-libs/. /usr/lib/Trigo-Whispr/
```

Resources only need re-copying if they change upstream (new icons, sounds, models, etc.).

## Troubleshooting

### macOS Accessibility remains enabled after a local rebuild

Local builds use the ad-hoc `signingIdentity: "-"`. A rebuild can have a new macOS code
identity while the old **System Settings > Privacy & Security > Accessibility** entry
remains visibly enabled, leaving Trigo Whispr on `Waiting...`.

After installing the final bundle at `/Applications/Trigo-Whispr.app`, quit Trigo Whispr, clear only its
stale Accessibility record, then reopen it:

```bash
osascript -e 'tell application id "com.trigo.trigo-whispr" to quit' || true
tccutil reset Accessibility com.trigo.trigo-whispr
open /Applications/Trigo-Whispr.app
```

Grant Accessibility again when prompted. This does not reset Microphone or other TCC
services, and official releases normally do not need it.

For optional diagnosis, compare the designated requirements of the previous and rebuilt
bundles:

```bash
codesign -dr - /path/to/previous/Trigo-Whispr.app 2>&1
codesign -dr - /Applications/Trigo-Whispr.app 2>&1
```

An ad-hoc requirement contains a `cdhash`; a changed requirement confirms the rebuild is
not covered by the old grant. The reset procedure does not require this check.

See [issue #1618](https://github.com/cjpais/Handy/issues/1618) for the related onboarding
and stale-permission report.

### AppImage build fails on Arch / rolling-release distros

`linuxdeploy` bundles its own `strip` binary which is too old to process system libraries built with newer toolchains on rolling-release distros (Arch, CachyOS, Manjaro, EndeavourOS).

The error from Tauri:

```
Bundling Trigo-Whispr_*_amd64.AppImage
failed to bundle project `failed to run linuxdeploy`
```

Tauri swallows the real linuxdeploy error. To see it, run linuxdeploy manually:

```bash
cd src-tauri/target/release/bundle/appimage
~/.cache/tauri/linuxdeploy-x86_64.AppImage --appimage-extract-and-run \
  --appdir Trigo-Whispr.AppDir --plugin gtk --output appimage
```

**Workaround:** The binary, deb, and rpm bundles all build fine — only the AppImage step fails. To skip it:

```bash
bun run tauri build -- --bundles deb
```

Then install using the deb extraction method above.

### Windows build fails with path-limit errors (`MSB3491` / `FTK1011` / `MSB6003`)

On Windows the native build can fail partway through `transcribe-cpp-sys` with
any of these (all the same root cause):

```
error MSB3491: Could not write lines to file "...VCTargetsPath.tlog\VCTargetsPath.lastbuildstate".
Path: ... exceeds the OS max path limit. The fully qualified file name must be less than 260 characters.
```

```
FileTracker : error FTK1011: could not create the new file tracking log file:
...\vulkan-shaders-gen-build\...\cmTC_xxxxx.tlog\link.write.1.tlog.
The system cannot find the path specified.
```

```
error MSB6003: The specified task executable "CL.exe" could not be run.
System.IO.DirectoryNotFoundException: Could not find a part of the path ...
```

This is **not** a code or toolchain problem — it's Windows' legacy 260-character
path limit (`MAX_PATH`), overflowed by the Vulkan shader generator's nested
CMake build tree on top of Cargo's already-deep
`target\release\build\<crate>-<hash>\out\build\...` directory.

Since `transcribe-cpp` 0.1.3 this is mitigated automatically: the native build
compiles through a short NTFS junction under `%LOCALAPPDATA%\tcs` (created
without admin rights), so a normal checkout builds with no setup. Enabling
Windows long paths does **not** reliably help here — MSBuild's native
`FileTracker` (`tracker.exe`) ignores the long-paths flag — which is why the
junction, not the registry flag, is the fix.

If you still see the errors above, junction creation was likely blocked
(filesystem or corporate policy) — the failing build's log then contains a
`transcribe-cpp-sys: could not create short build junction ...` warning — or
your checkout is deep enough to overflow even the shortened layout. Work
around either case with a short Cargo target directory:

```powershell
# Per-shell:
$env:CARGO_TARGET_DIR = "C:\h"

# Or persist it for all future terminals (note: redirects ALL your
# Rust projects' build output, not just Trigo Whispr):
[Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', 'C:\h', 'User')
```

Artifacts then land in `C:\h\release\...` instead of the repo's
`src-tauri\target\`. Open a **new terminal** if you persisted the variable —
it is only picked up by freshly started processes. Then `bun run tauri dev`
and `bun run tauri build` work normally.

### Windows `tauri build` fails at bundling with `program not found`

If the build compiles all the way to `Built application at: ...\trigo-whispr.exe` and
then fails with:

```
Signing C:\...\trigo-whispr.exe with a custom signing command
failed to bundle project `program not found`
```

that's the code-signing step: `tauri.conf.json` configures a custom
`signCommand` (`trusted-signing-cli`, Azure Trusted Signing) that only exists
in the release CI environment. Local development doesn't need it:

```powershell
# Development (no bundling/signing at all):
bun run tauri dev

# Or compile a release binary without the installer/signing step:
bun run tauri build --no-bundle
```
