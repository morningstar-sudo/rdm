#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

cargo fmt --check
cargo test
cargo build --release --target x86_64-pc-windows-gnu --bins
python3 -c 'import json, pathlib; root=pathlib.Path("apps/Rdm.Extension"); manifest=json.loads((root / "manifest.json").read_text()); files=[manifest["action"]["default_popup"], manifest["background"]["service_worker"], "popup.css", "popup.js"]; missing=[name for name in files if not (root / name).is_file()]; assert not missing, f"Missing extension files: {missing}"; assert {"downloads", "nativeMessaging", "storage"}.issubset(manifest["permissions"]), "Extension is missing a required permission"; worker=(root / manifest["background"]["service_worker"]).read_text(); assert "chrome.downloads.onCreated" in worker and "chrome.downloads.pause" in worker and "chrome.downloads.cancel" in worker and "autoDownload" in worker and "item.finalUrl" in worker and "lastDownloadStatus" in worker, "Automatic download handoff or visible status is not wired"; popup=(root / "popup.js").read_text(); assert "chrome.storage.local.get" in popup and "download-status" in (root / "popup.html").read_text(), "Download status is not visible in the extension popup"; host=pathlib.Path("src/native_host.rs").read_text(); ui=pathlib.Path("src/win32_ui.rs").read_text(); app=pathlib.Path("src/main.rs").read_text(); assert "--browser-download" in host and "wait_for_app_decision" in host and "decide_browser_download" in app and "PROGRESS_ACCEPT" in ui and "PROGRESS_REJECT" in ui, "In-app download confirmation is not wired"; assert "forward_browser_handoff" in app and "WM_COPYDATA" in ui and "FindWindowW" in app, "Existing RDM instance handoff is not wired"; assert "session_speed_bps" in app and "Session speed:" in ui, "Session speed display is not wired"; print("Extension, existing-instance handoff, and session speed wiring are valid.")'

echo "WSL tests and static Windows app cross-build passed."
echo "Executable: target/x86_64-pc-windows-gnu/release/rdm.exe"
