# RDM

RDM is a Windows HTTP/HTTPS download manager written in Rust. Its interface uses Win32 Common Controls and does not require the Windows App Runtime. The download engine and browser native messaging host are also written in Rust. Windows 10/11 visual styles are provided through the Common Controls v6 manifest.

## Features

- Add HTTP/HTTPS URLs to a queue and download them sequentially.
- Start a URL immediately without waiting for the queue; the download remains visible in the progress window and can be paused.
- Check a link with a one-byte range request to inspect its HTTP status, filename, content type, and size without downloading the full file.
- Pause, resume, or cancel an active download. Temporary data is kept in a uniquely named, URL-hash-scoped folder beside the destination; pausing preserves it, while completion or cancellation removes it without deleting an existing completed destination.
- Detect server range and validator support automatically. When possible, split larger files into at most 20 work units, about 5% of the original file each. Do not subdivide a work unit further once it is at or below 5% of the file. Start with up to 4 connections; workers take the next work unit as soon as they finish. A shared HTTP client pools eligible keep-alive connections so sequential work units can reuse them; servers that close connections require a new connection. The scheduler cautiously tests additional connections and keeps them only when measured aggregate throughput improves, backing off and retrying after a cooldown if it does not. The hard limit is 16 connections, not a target.
- Save the queue and download folder in `%LOCALAPPDATA%\RDM\queue.json`. Active downloads are restored as paused after restart.
- Use the Windows Known Folder `Downloads` as the default destination, including when the folder has been moved. Legacy default paths are migrated while custom folders are preserved.
- Choose a folder with the Windows folder picker or enter a path.
- Schedule downloads with the native date/time picker; clear the picker to download when the queue runs. Schedules and the queue persist across restarts.
- Choose a dark, light, or Windows-synchronized theme. The preference is stored in `%LOCALAPPDATA%\RDM\settings.json`.
- Search and filter the queue by status; retry failed items, remove individual entries, or clear completed downloads.
- Open the destination folder and copy a download URL from its entry.
- View the queue in a native Windows report table with separate file, status, and progress columns. The progress window selects downloads from a filename drop-down and shows overall progress plus a segmented activity bar; individual segment rows are hidden. Pause, resume, or cancel the selected download.
- The overall progress bar reserves its final 5% for assembling downloaded segments, so it reaches 100% only when the finished file is ready.
- Show each file's cumulative download duration, preserving elapsed time across pauses, resumes, and app restarts.
- View the smoothed aggregate transfer speed for the active download session in the main window and progress window.
- The Chrome/Edge extension pauses newly created HTTP/HTTPS downloads and opens an in-app confirmation in RDM. It prefers the browser's final redirect URL and forwards the suggested filename so opaque URLs still display and save with a useful name. The browser download is canceled only after the user selects **Download with RDM**; choosing **Keep in browser** resumes it. The extension status is visible from its toolbar popup. Browser-only `blob:`/`data:` downloads and downloads requiring browser-only authentication cannot be transferred to RDM and remain in Edge.
- Normal downloads do not require administrator privileges.

The interface uses built-in Win32 Common Controls and follows Windows visual styles on Windows 10 and 11. It is not WinUI 3 and does not provide Windows 11-specific Fluent controls.

The extension detects actual HTTP/HTTPS downloads reported by the browser; it does not discover every file link on a page or monitor every visited page. Use **Download link with RDM** from the link context menu for a specific direct link. Browser-only `blob:`/`data:` URLs and downloads that require cookies or authorization held only by the browser cannot be fetched independently by RDM. Multi-connection downloads are enabled only when the server supports byte ranges and provides a stable ETag or Last-Modified validator. Work units are saved inside the URL-scoped temporary folder so completed units can be reused after a pause or restart; the folder is removed after the final file is assembled.

## Build requirements

- Windows 10 version 1809 or later, or Windows 11 x64
- Rust stable with the `x86_64-pc-windows-msvc` target
- Visual Studio C++ Build Tools and the Windows SDK

Run this in PowerShell from the repository root:

```powershell
.\build.ps1
```

The `rdm.exe` executable is created in `target\x86_64-pc-windows-msvc\release`. Starting it normally opens the interface; Chrome/Edge native messaging starts the native host, which launches RDM with the requested URL. The MSVC runtime is statically linked; the app uses Windows system DLLs.

GitHub Actions also builds a small Windows installer/updater, executable, Edge extension package, and portable app bundle. Push a version tag such as `v1.0.0` to publish them as GitHub Release assets; the release is marked latest.

## Build and test with WSL

From PowerShell, install Rust/MinGW in Ubuntu WSL with root privileges:

```powershell
wsl.exe -d Ubuntu --user root -- apt-get update
wsl.exe -d Ubuntu --user root -- apt-get install -y rustup gcc-mingw-w64-x86-64
```

Then, as a regular WSL user, install the toolchain:

```bash
rustup toolchain install stable --profile minimal --target x86_64-pc-windows-gnu
rustup component add rustfmt
```

Run tests and cross-build the app:

```bash
bash scripts/test-wsl.sh
```

The Windows executable is created at `target/x86_64-pc-windows-gnu/release/rdm.exe`; the runtime/toolchain is statically linked so MinGW DLLs are not required alongside the app. You can also build natively on Windows with `build.ps1`, Rust MSVC, and Visual Studio C++ Build Tools. Windows App Runtime, .NET Framework 4.8, and MSBuild are not required.

`musl` is a Linux libc target and cannot be used to create a Windows app. For the Windows build, the C/C++ and Rust runtimes are statically linked; Windows still provides the basic UI and networking APIs.

To run the optional network download test with a URL you choose, set `RDM_TEST_URL` in WSL and run:

```bash
RDM_TEST_URL="https://example.com/file" cargo test downloads_remote_test_url_without_opening_archive -- --ignored
```

The test downloads to a temporary directory, verifies the size, and removes the file. It does not open or execute the downloaded content.

## Install RDM and the Edge extension

1. Download and run `RDM-Setup-x64.exe` from the [latest GitHub Release](https://github.com/morningstar-sudo/rdm/releases/latest). Internet access is required: the small installer downloads the latest app bundle from GitHub and verifies its SHA-256 before installing. It installs or updates RDM and its dedicated `rdm-host.exe` under `%LOCALAPPDATA%\Programs\RDM`, registers the `rdm://` link protocol and Edge native messaging host for the current user, and creates a Start menu shortcut. It does not need administrator access or PowerShell script execution. Run the same setup again to update; if Edge reports a native messaging host communication error, update this way and restart Edge.
2. Leave **Launch RDM and finish Edge setup** selected at the end of installation. On first launch, RDM copies the extension to `%LOCALAPPDATA%\RDM\Rdm.Extension` and opens Edge's extensions page.
3. In `edge://extensions`, enable **Developer mode**, choose **Load unpacked**, and select `%LOCALAPPDATA%\RDM\Rdm.Extension`. Edge requires this one-time manual step for unpacked extensions; RDM cannot silently install it.
4. Approve the `downloads` permission if Edge asks.

`RDM-Windows-x64.zip` remains available as a portable bundle; the separately packaged `Rdm-Edge-Extension.zip` is also attached to the release.

The extension ID is pinned in the manifest so its native messaging permission remains stable. RDM registration does not require administrator privileges. The protocol accepts links such as:

```powershell
Start-Process 'rdm://download?url=https%3A%2F%2Fexample.com%2Ffile.zip'
```

For Chrome or a manually managed installation, `scripts\install-browser-host.ps1` remains available; it requires the extension IDs and RDM install directory as parameters.

The extension only receives events when the browser creates an HTTP/HTTPS download. When loading or reloading a version that requests the `downloads` permission, the browser may ask you to grant it.

## Component communication

The Rust interface calls the download engine on a background worker. Progress callbacks report transfer state and receive cancellation requests. The queue is written to a temporary JSON file and atomically replaced to avoid corruption. The extension sends native messaging payloads such as `{ "url": "https://...", "autoDownload": true }` for detected downloads or `{ "url": "https://..." }` for a URL submitted from the extension. RDM accepts only HTTP/HTTPS download URLs. The registered `rdm://download?url=...` handler unwraps and validates the encoded HTTP/HTTPS URL before opening it in the app.
