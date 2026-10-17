# Trigo Whispr

[![Discord](https://img.shields.io/badge/Discord-%235865F2.svg?style=for-the-badge&logo=discord&logoColor=white)](https://discord.com/invite/WVBeWsNXK4)

**A free, open source, and extensible speech-to-text application that works completely offline.**

Trigo Whispr is a cross-platform desktop application that provides simple, privacy-focused speech transcription. Press a shortcut, speak, and have your words appear in any text field. This happens on your own computer without sending any information to the cloud.

## Why Trigo Whispr?

Trigo Whispr was created to fill the gap for a truly open source, extensible speech-to-text tool. As stated on [handy.computer](https://handy.computer):

- **Free**: Accessibility tooling belongs in everyone's hands, not behind a paywall
- **Open Source**: Together we can build further. Extend Trigo Whispr for yourself and contribute to something bigger
- **Private**: Your voice stays on your computer. Get transcriptions without sending audio to the cloud
- **Simple**: One tool, one job. Transcribe what you say and put it into a text box

## How It Works

1. **Press** a configurable keyboard shortcut: hold it to record and release to stop, or tap it to toggle recording on and off (Hold-only and Toggle-only modes are also available)
2. **Speak** your words while the shortcut is active
3. **Release** and Trigo Whispr processes your speech using Whisper
4. **Get** your transcribed text pasted directly into whatever app you're using

The process is entirely local:

- Silence is filtered using VAD (Voice Activity Detection) with Silero
- Transcription uses your choice of models:
  - **Whisper models** (Small/Medium/Turbo/Large) with GPU acceleration when available
  - **Parakeet V3** - CPU-optimized model with excellent performance and automatic language detection
- Works on Windows, macOS, and Linux

## Quick Start

### Installation

1. Download the latest release from the [releases page](https://github.com/llexa97/trigo-whispr/releases) or the [website](https://handy.computer)
   - **macOS**: Also available via [Homebrew cask](https://formulae.brew.sh/cask/trigo-whispr): `brew install --cask trigo-whispr`
   - **Windows**: Also available via [winget](https://github.com/microsoft/winget-pkgs): `winget install cjpais.Handy` \
     **Note:** The Homebrew cask and winget package are not maintained by the Trigo Whispr developers.
   - **Debian/Ubuntu**: Install the downloaded `.deb` with APT so required dependencies are installed automatically:
     ```bash
     sudo apt install ./Trigo-Whispr_*.deb
     ```
     Do not use `dpkg -i` unless the dependencies are already installed. If you already used it, run `sudo apt --fix-broken install`.
2. Install the application
3. Launch Trigo Whispr and grant necessary system permissions (microphone, accessibility)
4. Configure your preferred keyboard shortcuts in Settings
5. Start transcribing!

### Development Setup

For detailed build instructions including platform-specific requirements, see [BUILD.md](BUILD.md).

## Sponsors

<div align="center">
  We're grateful for the support of our sponsors who help make Trigo Whispr possible:
  <br><br>
  <a href="https://wordcab.com">
    <img src="sponsor-images/wordcab.png" alt="Wordcab" width="120" height="120">
  </a>
  &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://github.com/epicenter-so/epicenter">
    <img src="sponsor-images/epicenter.png" alt="Epicenter" width="120" height="120">
  </a>
  &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://boltai.com?utm_source=trigo-whispr">
    <img src="sponsor-images/boltai.jpg" alt="Bolt AI" width="120" height="120">
  </a>
  &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://cantydigital.com.au/">
    <img src="sponsor-images/cantydigital.png" alt="Canty Digital" width="120" height="120">
  </a>
</div>

## Integrations

<a href="https://www.raycast.com/mattiacolombomc/handy" title="Install Handy Raycast Extension"><img src="https://www.raycast.com/mattiacolombomc/handy/install_button@2x.png?v=1.1" height="64" style="height: 64px;" alt="Install trigo-whispr Raycast Extension" /></a>

Control Trigo Whispr from [Raycast](https://www.raycast.com) — start/stop recording, browse transcript history, manage dictionary, switch models and languages.

[Source](https://github.com/mattiacolombomc/raycast-handy) · by [@mattiacolombomc](https://github.com/mattiacolombomc)

### Debug Mode

Trigo Whispr includes an advanced debug mode for development and troubleshooting. Access it by pressing:

- **macOS**: `Cmd+Shift+D`
- **Windows/Linux**: `Ctrl+Shift+D`

### CLI Parameters

Trigo Whispr supports command-line flags for controlling a running instance and customizing startup behavior. These work on all platforms (macOS, Windows, Linux). Largely this is a beta feature.

**Remote control flags** (sent to an already-running instance via the single-instance plugin):

```bash
trigo-whispr --toggle-transcription    # Toggle recording on/off
trigo-whispr --toggle-post-process     # Toggle recording with post-processing on/off
trigo-whispr --toggle-meeting          # Start/stop recording a meeting (Windows, macOS)
trigo-whispr --cancel                  # Cancel the current operation
```

**Startup flags:**

```bash
trigo-whispr --start-hidden            # Start without showing the main window
trigo-whispr --no-tray                 # Start without the system tray icon
trigo-whispr --debug                   # Enable debug mode with verbose logging
trigo-whispr --help                    # Show all available flags
```

Flags can be combined for autostart scenarios:

```bash
trigo-whispr --start-hidden --no-tray
```

> **macOS tip:** When Trigo Whispr is installed as an app bundle, invoke the binary directly:
>
> ```bash
> /Applications/Trigo-Whispr.app/Contents/MacOS/Trigo-Whispr --toggle-transcription
> ```

## Known Issues & Current Limitations

This project is actively being developed and has some [known issues](https://github.com/llexa97/trigo-whispr/issues). We believe in transparency about the current state:

### Bluetooth Headset Microphones (macOS)

Using a Bluetooth headset microphone on macOS may temporarily reduce playback quality or volume while recording because Bluetooth switches to bidirectional audio. Keep your headphones as the output device and select your Mac's built-in or an external microphone in Trigo Whispr to avoid this.

### fn and Globe Key Shortcuts (macOS)

Shortcuts that include the `fn` (Globe) key **only work on Apple keyboards** — your Mac's built-in keyboard or an Apple external keyboard. They will never trigger on a third-party keyboard, even while it is connected to the same Mac.

This is a hardware limitation rather than a Trigo Whispr bug. `fn` is not part of the standard USB HID keyboard specification: Apple reports it through a vendor-specific usage that macOS honors only from Apple devices, while third-party keyboards handle their `Fn` key entirely in firmware and send nothing to the computer. There is no event for Trigo Whispr to listen for.

If you switch between a MacBook keyboard and an external one, pick a shortcut built from standard modifiers (`ctrl`, `option`, `shift`, `command`) or a regular key instead.

### Linux Notes

**Text Input Tools:**

For reliable text input on Linux, install the appropriate tool for your display server:

| Display Server | Recommended Tool | Install Command                                    |
| -------------- | ---------------- | -------------------------------------------------- |
| X11            | `xdotool`        | `sudo apt install xdotool`                         |
| Wayland        | `wtype`          | `sudo apt install wtype`                           |
| Both           | `dotool`         | `sudo apt install dotool` (requires `input` group) |

- **X11**: Install `xdotool` for both direct typing and clipboard paste shortcuts
- **Ubuntu 26.04**: Has Wayland display server by default. `wtype` does not work, you need to install `ydotool` and configure systemd as described [here](https://github.com/cjpais/Handy/pull/557#issuecomment-3781249267).
- **Wayland**: Install `wtype` (preferred) or `dotool` for text input to work correctly
- **dotool setup**: Requires adding your user to the `input` group: `sudo usermod -aG input $USER` (then log out and back in)

Without these tools, Trigo Whispr falls back to enigo which may have limited compatibility, especially on Wayland.

**Wayland Support (Linux):**

- Limited support for Wayland display server
- Requires [`wtype`](https://github.com/atx/wtype) or [`dotool`](https://sr.ht/~geb/dotool/) for text input to work correctly (see [Linux Notes](#linux-notes) below for installation)

**Other Notes:**

- **Runtime library dependency (`libgtk-layer-shell.so.0`)**:
  - Trigo Whispr links `gtk-layer-shell` on Linux. If startup fails with `error while loading shared libraries: libgtk-layer-shell.so.0`, install the runtime package for your distro:

    | Distro        | Package to install    | Example command                        |
    | ------------- | --------------------- | -------------------------------------- |
    | Ubuntu/Debian | `libgtk-layer-shell0` | `sudo apt install libgtk-layer-shell0` |
    | Fedora/RHEL   | `gtk-layer-shell`     | `sudo dnf install gtk-layer-shell`     |
    | Arch Linux    | `gtk-layer-shell`     | `sudo pacman -S gtk-layer-shell`       |

  - For building from source on Ubuntu/Debian, you may also need `libgtk-layer-shell-dev`.

- The recording overlay is disabled by default on Linux (`Overlay Position: None`) because certain compositors treat it as the active window. When the overlay is visible it can steal focus, which prevents Trigo Whispr from pasting back into the application that triggered transcription. If you enable the overlay anyway, be aware that clipboard-based pasting might fail or end up in the wrong window.
- Trigo Whispr disables the WebKit DMA-BUF renderer by default on Linux, but preserves explicit `WEBKIT_DISABLE_DMABUF_RENDERER` values. For rendering workarounds, see [Troubleshooting → Linux Startup Crashes or Instability](#linux-startup-crashes-or-instability).
- If Trigo Whispr fails to start reliably on Linux, see [Troubleshooting → Linux Startup Crashes or Instability](#linux-startup-crashes-or-instability).
- **Global keyboard shortcuts (Wayland):** On Wayland, system-level shortcuts must be configured through your desktop environment or window manager. Use the [CLI flags](#cli-parameters) as the command for your custom shortcut.

  **GNOME:**
  1. Open **Settings > Keyboard > Keyboard Shortcuts > Custom Shortcuts**
  2. Click the **+** button to add a new shortcut
  3. Set the **Name** to `Toggle Trigo Whispr Transcription`
  4. Set the **Command** to `trigo-whispr --toggle-transcription`
  5. Click **Set Shortcut** and press your desired key combination (e.g., `Super+O`)

  **KDE Plasma:**
  1. Open **System Settings > Shortcuts > Custom Shortcuts**
  2. Click **Edit > New > Global Shortcut > Command/URL**
  3. Name it `Toggle Trigo Whispr Transcription`
  4. In the **Trigger** tab, set your desired key combination
  5. In the **Action** tab, set the command to `trigo-whispr --toggle-transcription`

  **Sway / i3:**

  Add to your config file (`~/.config/sway/config` or `~/.config/i3/config`):

  ```ini
  bindsym $mod+o exec trigo-whispr --toggle-transcription
  ```

  **Hyprland:**

  Add to your config file (`~/.config/hypr/hyprland.conf`):

  ```ini
  bind = $mainMod, O, exec, trigo-whispr --toggle-transcription
  ```

- You can also trigger Trigo Whispr externally via Unix signals or the CLI flags, which lets Wayland window managers or other hotkey daemons keep ownership of keybindings:

  | Action                                    | Trigger                                                                |
  | ----------------------------------------- | ---------------------------------------------------------------------- |
  | Toggle transcription                      | `pkill -USR2 -n trigo-whispr` or `trigo-whispr --toggle-transcription` |
  | Toggle transcription with post-processing | `trigo-whispr --toggle-post-process`                                   |

  Example Sway config:

  ```ini
  bindsym $mod+o exec pkill -USR2 -n trigo-whispr
  bindsym $mod+p exec trigo-whispr --toggle-post-process
  ```

  `pkill` here simply delivers the signal—it does not terminate the process.

  > **Behavior change:** older releases also accepted `SIGUSR1` for toggling transcription with post-processing. WebKitGTK — the webview engine embedded in Trigo Whispr on Linux — uses SIGUSR1 internally to coordinate JavaScript garbage collection, so listening for it caused phantom recordings and interrupted dictations every few minutes ([#1660](https://github.com/cjpais/Handy/issues/1660)). Trigo Whispr no longer listens for SIGUSR1 on Linux; the post-processing toggle is still available via `trigo-whispr --toggle-post-process`. **Remove any `pkill -USR1` bindings**: the signal is now delivered straight to WebKit's internal handler and can crash the app.

**Overlay & Pasting Issues (Linux):**

- The recording overlay window can interfere with pasting transcribed text into target applications on Linux (X11)
- **Solution:** Open **Settings > Advanced** and set **"Overlay Position"** to **"None"** to disable the overlay
- Enable **"Audio Feedback"** (also in Advanced) if you still want audible confirmation of recording state
- Users who upgrade from older versions or import settings from other platforms may need to manually apply this change

## Verify Release Signatures

Trigo Whispr release artifacts are signed with Tauri's updater signature format. The public key is stored in [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json) under `plugins.updater.pubkey`.

To verify a release manually, set `ARTIFACT` to the filename you downloaded, save the `pubkey` value from `src-tauri/tauri.conf.json` to `trigo-whispr.pub.b64`, then decode the public key and matching `.sig` file from base64 and verify the artifact with `minisign`:

```bash
# Replace with the file you downloaded
ARTIFACT="Trigo-Whispr_0.8.1_amd64.AppImage"

python3 - "$ARTIFACT" <<'PY'
import base64, pathlib, sys

artifact = sys.argv[1]

pub = pathlib.Path("trigo-whispr.pub.b64").read_text().strip()
pathlib.Path("trigo-whispr.pub").write_bytes(base64.b64decode(pub))

sig = pathlib.Path(f"{artifact}.sig").read_text().strip()
pathlib.Path(f"{artifact}.minisig").write_bytes(base64.b64decode(sig))
PY

minisign -Vm "$ARTIFACT" \
  -p trigo-whispr.pub \
  -x "$ARTIFACT.minisig"
```

On success, `minisign` prints:

```text
Signature and comment signature verified
```

Do not use `gpg` for these `.sig` files.

## Troubleshooting

### Previous Clipboard Content Is Pasted Instead of the Transcription

If the transcription is correct in **History** but Trigo Whispr inserts text you copied earlier, see [issue #502](https://github.com/cjpais/Handy/issues/502). With the standard clipboard paste method, Trigo Whispr restores your previous clipboard after a fixed delay. Under load, the receiving application may read the clipboard only after that restoration.

1. Open Trigo Whispr's settings window and press `Cmd+Shift+D` (macOS) or `Ctrl+Shift+D` (Windows/Linux) to reveal **Debug**.
2. On **macOS and Windows**, try **Reliable Paste (Beta)** in Debug with a clipboard paste method selected. It uses clipboard read notifications to delay restoration instead of relying on the standard fixed delay. Test it in the application where the problem occurs; it is still experimental.
3. If Reliable Paste is disabled or unavailable, increase **Paste Delay (After)** in Debug and test again. This controls the wait before restoring your previous clipboard. **Paste Delay (Before)** controls the wait before sending the paste keystroke and addresses a different part of the operation. These delay settings apply to the standard paste path, not Reliable Paste.

If the problem persists, add your Trigo Whispr version, operating system, receiving application, paste method, Reliable Paste setting, and before/after delays to the existing issue. Redact private dictated text before sharing logs.

### Manual Model Installation (For Proxy Users or Network Restrictions)

If you're behind a proxy, firewall, or in a restricted network environment where Trigo Whispr cannot download models automatically, you can manually download and install them. The URLs are publicly accessible from any browser.

#### Step 1: Find Your App Data Directory

1. Open Trigo Whispr settings
2. Navigate to the **About** section
3. Copy the "App Data Directory" path shown there, or use the shortcuts:
   - **macOS**: `Cmd+Shift+D` to open debug menu
   - **Windows/Linux**: `Ctrl+Shift+D` to open debug menu

The typical paths are:

- **macOS**: `~/Library/Application Support/com.trigo.trigo-whispr/`
- **Windows**: `C:\Users\{username}\AppData\Roaming\com.trigo.trigo-whispr\`
- **Linux**: `~/.config/com.trigo.trigo-whispr/`

#### Step 2: Create Models Directory

Inside your app data directory, create a `models` folder if it doesn't already exist:

```bash
# macOS/Linux
mkdir -p ~/Library/Application\ Support/com.trigo.trigo-whispr/models

# Windows (PowerShell)
New-Item -ItemType Directory -Force -Path "$env:APPDATA\com.trigo.trigo-whispr\models"
```

#### Step 3: Download Model Files

Download the models you want from below

**Whisper Models (single .bin files):**

- Small (487 MB): `https://blob.handy.computer/ggml-small.bin`
- Medium (492 MB): `https://blob.handy.computer/whisper-medium-q4_1.bin`
- Turbo (1600 MB): `https://blob.handy.computer/ggml-large-v3-turbo.bin`
- Large (1100 MB): `https://blob.handy.computer/ggml-large-v3-q5_0.bin`

**Parakeet Unified EN 0.6B (single `.gguf` file, recommended):**

- Q8_0 (731 MB): `https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/main/parakeet-unified-en-0.6b-Q8_0.gguf`

#### Step 4: Install Models

**For Whisper Models (.bin files):**

Simply place the `.bin` file directly into the `models` directory:

```
{app_data_dir}/models/
├── ggml-small.bin
├── whisper-medium-q4_1.bin
├── ggml-large-v3-turbo.bin
└── ggml-large-v3-q5_0.bin
```

**For GGUF Models (.gguf files):**

Place the `.gguf` file directly into the `models` directory, exactly like the Whisper `.bin` files above. Trigo Whispr also picks up models already present in the shared Hugging Face cache (`~/.cache/huggingface/hub`), so a copy downloaded by another tool works without being moved.

**Important Notes:**

- Do not rename the `.bin` or `.gguf` files—use the exact filenames from the download URLs
- After placing the files, restart Trigo Whispr to detect the new models

#### Step 5: Verify Installation

1. Restart Trigo Whispr
2. Open Settings → Models
3. Your manually installed models should now appear as "Downloaded"
4. Select the model you want to use and test transcription

### Custom Whisper Models

Trigo Whispr can auto-discover custom Whisper GGML models placed in the `models` directory. This is useful for users who want to use fine-tuned or community models not included in the default model list.

**How to use:**

1. Obtain a Whisper model in GGML `.bin` format (e.g., from [Hugging Face](https://huggingface.co/models?search=whisper%20ggml))
2. Place the `.bin` file in your `models` directory (see paths above)
3. Restart Trigo Whispr to discover the new model
4. The model will appear in the "Custom Models" section of the Models settings page

**Important:**

- Community models are user-provided and may not receive troubleshooting assistance
- The model must be a valid Whisper GGML format (`.bin` file)
- Model name is derived from the filename (e.g., `my-custom-model.bin` → "My Custom Model")

### Linux Startup Crashes or Instability

If Trigo Whispr fails to start reliably on Linux — for example, it crashes shortly after launch, never shows its window, or reports a Wayland protocol error — try the steps below in order.

**1. Install (or reinstall) `gtk-layer-shell`**

Trigo Whispr uses `gtk-layer-shell` for its recording overlay and links against it at runtime. A missing or broken installation is the most common cause of startup failures and can manifest as a crash or a hang well before any window is shown. Make sure the runtime package is installed for your distro:

| Distro        | Package to install    | Example command                        |
| ------------- | --------------------- | -------------------------------------- |
| Ubuntu/Debian | `libgtk-layer-shell0` | `sudo apt install libgtk-layer-shell0` |
| Fedora/RHEL   | `gtk-layer-shell`     | `sudo dnf install gtk-layer-shell`     |
| Arch Linux    | `gtk-layer-shell`     | `sudo pacman -S gtk-layer-shell`       |

If it is already installed and you still see startup problems, try reinstalling it (e.g. `sudo pacman -S gtk-layer-shell` again) in case the library files were corrupted by a partial upgrade.

**2. Disable the GTK layer shell overlay (`TRIGO_WHISPR_NO_GTK_LAYER_SHELL`)**

If installing the library does not help, you can skip `gtk-layer-shell` initialization entirely as a workaround. On some compositors (notably KDE Plasma under Wayland) it has been reported to interact poorly with the recording overlay. With this variable set, the overlay falls back to a regular always-on-top window:

```bash
TRIGO_WHISPR_NO_GTK_LAYER_SHELL=1 trigo-whispr
```

**3. Configure WebKit DMA-BUF rendering (`WEBKIT_DISABLE_DMABUF_RENDERER`)**

Trigo Whispr disables the WebKitGTK DMA-BUF renderer by default on Linux because it can cause blank windows or crashes on some GPU/driver combinations. Explicit environment-variable values are preserved, including in the Nix package. To explicitly keep the renderer disabled:

```bash
WEBKIT_DISABLE_DMABUF_RENDERER=1 trigo-whispr
```

Some users on WebKitGTK 2.54 report a flickering or partially drawn recording overlay with the renderer disabled ([#2166](https://github.com/cjpais/Handy/issues/2166)). If affected, try opting out of the default workaround:

```bash
WEBKIT_DISABLE_DMABUF_RENDERER=0 trigo-whispr
```

This allows WebKit to use the renderer when supported, but can reintroduce the GPU/driver-specific crashes described above. Remove the override or use `=1` if that happens.

Fully quit Trigo Whispr, including its tray instance, before testing either value. Running a second instance with `--toggle-transcription` does not change the running process's environment.

**Making a workaround permanent**

Set the variable in the environment of the launcher that actually starts Trigo Whispr. Shell profile exports may not apply to desktop-menu or autostart launches. If you launch Trigo Whispr from a `.desktop` file, you can prefix the `Exec=` line, e.g.:

```ini
Exec=env TRIGO_WHISPR_NO_GTK_LAYER_SHELL=1 trigo-whispr
```

Trigo Whispr's built-in autostart rewrites its generated desktop entry, so editing that entry is not a durable workaround. For autostart, use an appropriate session/service environment, or disable Trigo Whispr's built-in autostart and manage your own startup entry with the required variable.

If a workaround helps you, please [open an issue](https://github.com/llexa97/trigo-whispr/issues) describing your distro, desktop environment, and session type — that information helps us narrow down the underlying bug.

### Empty recording overlay on Hyprland / Omarchy

If the recording overlay is empty or bordered, fully quit Trigo Whispr and launch a
[native installation](BUILD.md#linux-install-from-source) with Wayland enabled:

```bash
env -u TRIGO_WHISPR_NO_GTK_LAYER_SHELL GDK_BACKEND=wayland trigo-whispr
```

If this works, apply `GDK_BACKEND=wayland` only to Trigo Whispr's launcher. This
workaround does not work with the 0.9.6 AppImage, which forces X11.

### Vulkan Overlays and Capture Tools on Windows (`TRIGO_WHISPR_KEEP_VULKAN_IMPLICIT_LAYERS`)

On Windows, Trigo Whispr asks the Vulkan loader to skip implicit layers to avoid crashes caused by overlay and capture hooks ([#2049](https://github.com/cjpais/Handy/issues/2049)). GPU acceleration remains enabled; this does not change system-wide settings.

To opt out for GPU selection or debugging tools, fully quit Trigo Whispr (including the tray icon), then run both commands in the same PowerShell window:

```powershell
$env:TRIGO_WHISPR_KEEP_VULKAN_IMPLICIT_LAYERS = "1"
& "$env:ProgramFiles\Trigo-Whispr\trigo-whispr.exe"
```

Adjust the executable path if needed. This override only applies to apps launched from that PowerShell session, not the Start menu. Trigo Whispr also preserves any existing `VK_LOADER_LAYERS_DISABLE` value.

### How to Contribute

1. **Check existing issues** at [github.com/llexa97/trigo-whispr/issues](https://github.com/llexa97/trigo-whispr/issues)
2. **Fork the repository** and create a feature branch
3. **Test thoroughly** on your target platform
4. **Submit a pull request** with clear description of changes
5. **Join the discussion** - reach out at [contact@handy.computer](mailto:contact@handy.computer)

The goal is to create both a useful tool and a foundation for others to build upon—a well-patterned, simple codebase that serves the community.

## Related Projects

- **[Handy CLI](https://github.com/cjpais/handy-cli)** - The original Python command-line version
- **[handy.computer](https://handy.computer)** - Project website with demos and documentation

## License

MIT License - see [LICENSE](LICENSE) file for details.

Trigo Whispr is open-source software, but the Trigo Whispr name, logo, icon, and brand assets are not open-source. Unofficial forks, rewrites, and redistributions must use their own branding and must not imply endorsement or affiliation.

## Acknowledgments

- **Whisper** by OpenAI for the speech recognition model
- **ggml** for an amazing cross-platform tensor library
- **Silero** for great lightweight VAD
- **Tauri** team for the excellent Rust-based app framework
- **Community contributors** helping make Trigo Whispr better
