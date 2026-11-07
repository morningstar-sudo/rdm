#![cfg_attr(windows, windows_subsystem = "windows")]

const OVERALL_PROGRESS_SCALE: u64 = 10_000;
const DOWNLOAD_PROGRESS_WEIGHT: u64 = 9_500;

fn overall_progress(
    downloaded: u64,
    total: u64,
    merging: bool,
    merge_downloaded: u64,
    merge_total: u64,
    completed: bool,
) -> (u64, u64) {
    if completed {
        return (OVERALL_PROGRESS_SCALE, OVERALL_PROGRESS_SCALE);
    }
    if total == 0 {
        return (0, OVERALL_PROGRESS_SCALE);
    }

    let download_progress = u128::from(downloaded.min(total))
        * u128::from(DOWNLOAD_PROGRESS_WEIGHT)
        / u128::from(total);
    let progress = if merging && merge_total > 0 {
        let merge_weight = OVERALL_PROGRESS_SCALE - DOWNLOAD_PROGRESS_WEIGHT;
        let merge_progress = u128::from(merge_downloaded.min(merge_total))
            * u128::from(merge_weight)
            / u128::from(merge_total);
        u64::try_from(u128::from(DOWNLOAD_PROGRESS_WEIGHT) + merge_progress)
            .unwrap_or(OVERALL_PROGRESS_SCALE)
    } else {
        u64::try_from(download_progress).unwrap_or(DOWNLOAD_PROGRESS_WEIGHT)
    };
    (
        progress.min(OVERALL_PROGRESS_SCALE - 1),
        OVERALL_PROGRESS_SCALE,
    )
}

#[cfg(windows)]
mod native_host;

#[cfg(windows)]
mod windows_app {
    use chrono::{Local, NaiveDateTime, TimeZone};
    use rdm_engine::{
        discard_partial_download, download_segmented_with_progress, probe_download, rdm_download,
        CancelCallback, LinkInfo, ProgressCallback,
    };
    use std::ffi::{CStr, CString};
    use std::fs;
    use std::io::Write;
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    struct TransferProgress {
        downloaded: AtomicU64,
        total: AtomicU64,
        cancelled: AtomicBool,
        cancel_requested: AtomicBool,
    }

    #[repr(C)]
    struct BrowserCopyData {
        data_id: usize,
        byte_count: u32,
        data: *mut std::ffi::c_void,
    }

    const BROWSER_DOWNLOAD_COPYDATA_ID: usize = 0x5244_4D42;
    const EDGE_EXTENSION_ID: &str = "agdholmngaiajkafecfkonjoijmnjfic";
    const NATIVE_HOST_EXECUTABLE: &str = "rdm-host.exe";
    const NATIVE_HOST_NAME: &str = "com.rdm.download";

    pub fn setup_first_run() -> Result<(), String> {
        let executable =
            std::env::current_exe().map_err(|error| format!("Could not locate RDM: {error}"))?;
        let executable = executable
            .canonicalize()
            .map_err(|error| format!("Could not resolve RDM path: {error}"))?;
        let executable_text = executable.to_string_lossy();
        let protocol_command = format!("\"{executable_text}\" --capture-url \"%1\"");

        register_protocol_handler(&protocol_command)?;

        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "The LOCALAPPDATA environment variable is not set.".to_owned())?;
        let app_data = local_app_data.join("RDM");
        fs::create_dir_all(&app_data)
            .map_err(|error| format!("Could not create {}: {error}", app_data.display()))?;

        let setup_marker = app_data.join("edge-integration-v1");
        let executable_directory = executable
            .parent()
            .ok_or_else(|| "Could not locate the RDM installation directory.".to_owned())?;
        let bundled_extension = executable_directory.join("Rdm.Extension");
        if !bundled_extension.is_dir() {
            let protocol_marker = app_data.join("protocol-registered-v1");
            if !protocol_marker.exists() {
                fs::write(&protocol_marker, b"")
                    .map_err(|error| format!("Could not write setup marker: {error}"))?;
                show_setup_message(
                    "RDM has registered the rdm:// link handler. The Edge extension folder was not found beside rdm.exe, so browser integration is not configured.",
                );
            }
            return Ok(());
        }

        let installed_extension = app_data.join("Rdm.Extension");
        copy_extension_directory(&bundled_extension, &installed_extension)?;

        let host_manifest_path = app_data.join("com.rdm.download.json");
        let host_executable = executable_directory.join(NATIVE_HOST_EXECUTABLE);
        let host_executable = if host_executable.is_file() {
            host_executable
        } else {
            executable.clone()
        };
        let host_manifest = serde_json::json!({
            "name": NATIVE_HOST_NAME,
            "description": "RDM browser URL handoff",
            "path": host_executable.to_string_lossy(),
            "type": "stdio",
            "allowed_origins": [format!("chrome-extension://{EDGE_EXTENSION_ID}/")],
        });
        fs::write(
            &host_manifest_path,
            serde_json::to_vec_pretty(&host_manifest)
                .map_err(|error| format!("Could not create native host manifest: {error}"))?,
        )
        .map_err(|error| {
            format!(
                "Could not write native host manifest {}: {error}",
                host_manifest_path.display()
            )
        })?;

        let native_host_registry_key =
            r"HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.rdm.download";
        register_registry_string(
            native_host_registry_key,
            None,
            &host_manifest_path.to_string_lossy(),
        )?;

        if !setup_marker.exists() {
            fs::write(&setup_marker, b"")
                .map_err(|error| format!("Could not write setup marker: {error}"))?;
            show_setup_message(&format!(
                "RDM registered rdm:// and the Edge native messaging host.\n\nTo finish installing the Edge extension, enable Developer mode at edge://extensions, choose Load unpacked, and select:\n{}",
                installed_extension.display()
            ));
            open_edge_extensions()?;
        }

        Ok(())
    }

    fn open_edge_extensions() -> Result<(), String> {
        let candidates = [
            std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
            std::env::var_os("ProgramFiles").map(PathBuf::from),
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        ];
        let edge = candidates
            .into_iter()
            .flatten()
            .map(|root| {
                root.join("Microsoft")
                    .join("Edge")
                    .join("Application")
                    .join("msedge.exe")
            })
            .find(|path| path.is_file());

        if let Some(edge) = edge {
            Command::new(edge)
                .arg("edge://extensions")
                .spawn()
                .map_err(|error| format!("Could not open Edge Extensions: {error}"))?;
        } else {
            Command::new("explorer.exe")
                .arg("microsoft-edge://extensions")
                .spawn()
                .map_err(|error| format!("Could not open Edge Extensions: {error}"))?;
        }
        Ok(())
    }

    fn register_protocol_handler(command: &str) -> Result<(), String> {
        let protocol_key = r"HKCU\Software\Classes\rdm";
        register_registry_string(protocol_key, None, "URL:RDM Protocol")?;
        register_registry_string(protocol_key, Some("URL Protocol"), "")?;
        register_registry_string(
            r"HKCU\Software\Classes\rdm\shell\open\command",
            None,
            command,
        )
    }

    fn register_registry_string(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let mut command = Command::new("reg.exe");
        command.arg("add").arg(key);
        if let Some(name) = name {
            command.arg("/v").arg(name);
        } else {
            command.arg("/ve");
        }
        let output = command
            .args(["/t", "REG_SZ", "/d", value, "/f"])
            .output()
            .map_err(|error| format!("Could not update Windows registry key {key}: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "Could not update Windows registry key {key}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn copy_extension_directory(source: &Path, destination: &Path) -> Result<(), String> {
        fs::create_dir_all(destination)
            .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
        let entries = fs::read_dir(source)
            .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("Could not read extension entry: {error}"))?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect {}: {error}", source_path.display()))?;
            if file_type.is_dir() {
                copy_extension_directory(&source_path, &destination_path)?;
            } else if file_type.is_file() {
                let contents = fs::read(&source_path).map_err(|error| {
                    format!("Could not read {}: {error}", source_path.display())
                })?;
                if fs::read(&destination_path).ok().as_deref() != Some(contents.as_slice()) {
                    fs::write(&destination_path, contents).map_err(|error| {
                        format!("Could not install {}: {error}", destination_path.display())
                    })?;
                }
            }
        }
        Ok(())
    }

    fn show_setup_message(message: &str) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

        let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "RDM setup".encode_utf16().chain(Some(0)).collect();
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    fn send_browser_decision(handoff: &BrowserHandoff, accepted: bool) -> Result<(), String> {
        let mut stream =
            TcpStream::connect_timeout(&handoff.address, std::time::Duration::from_secs(3))
                .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .map_err(|error| error.to_string())?;
        let payload = serde_json::to_vec(&serde_json::json!({
            "token": handoff.token,
            "accepted": accepted,
        }))
        .map_err(|error| error.to_string())?;
        stream
            .write_all(&payload)
            .map_err(|error| error.to_string())
    }

    impl TransferProgress {
        fn new() -> Self {
            Self {
                downloaded: AtomicU64::new(0),
                total: AtomicU64::new(0),
                cancelled: AtomicBool::new(false),
                cancel_requested: AtomicBool::new(false),
            }
        }
    }

    #[derive(Clone, Default)]
    struct SegmentLane {
        downloaded: u64,
        total: u64,
        active: bool,
    }

    #[derive(Clone)]
    struct BrowserHandoff {
        address: SocketAddr,
        token: String,
    }

    #[derive(Clone)]
    struct DownloadEntry {
        id: u64,
        url: String,
        destination: PathBuf,
        scheduled_at: Option<chrono::DateTime<Local>>,
        status: String,
        progress: Arc<TransferProgress>,
        segment_lanes: Vec<SegmentLane>,
        merge_downloaded: u64,
        merge_total: u64,
        merging: bool,
        browser_handoff: Option<BrowserHandoff>,
        elapsed_download_ms: u64,
        active_download_since: Option<Instant>,
    }

    impl DownloadEntry {
        fn elapsed_download(&self) -> Duration {
            Duration::from_millis(self.elapsed_download_ms).saturating_add(
                self.active_download_since
                    .map(|started| started.elapsed())
                    .unwrap_or_default(),
            )
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct PersistedQueue {
        version: u32,
        folder: String,
        entries: Vec<PersistedEntry>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct PersistedEntry {
        id: u64,
        url: String,
        destination: String,
        scheduled_at: Option<String>,
        status: String,
        #[serde(default)]
        elapsed_download_ms: u64,
    }

    fn normalize_saved_status(status: String) -> String {
        match status.as_str() {
            "Đang tải" | "Đang tạm dừng…" | "Đã tạm dừng" => "Paused".to_owned(),
            "Đang chờ" => "Queued".to_owned(),
            "Đã lên lịch" => "Scheduled".to_owned(),
            "Hoàn tất" => "Completed".to_owned(),
            "Đã hủy" | "Đang hủy…" => "Cancelled".to_owned(),
            _ if status.starts_with("Lỗi:") => {
                format!("Error:{}", status.trim_start_matches("Lỗi:"))
            }
            _ => status,
        }
    }

    enum UiEvent {
        Progress(u64, u64, u64),
        SegmentMode(u64, usize),
        SegmentProgress(u64, usize, u64, u64, bool),
        DownloadFilenameResolved(u64, Result<Option<PathBuf>, String>),
        DownloadFinished(u64, Result<(), String>),
        LinkChecked(String, Result<LinkInfo, String>),
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum QueueFilter {
        All,
        Active,
        Completed,
        Failed,
    }

    enum EntryAction {
        OpenFolder(u64),
        CopyUrl(u64),
        Retry(u64),
        Remove(u64),
    }

    struct DownloadManager {
        url_input: String,
        folder_input: String,
        schedule_input: String,
        search_input: String,
        queue_filter: QueueFilter,
        entries: Vec<DownloadEntry>,
        next_id: u64,
        active_id: Option<u64>,
        queue_running: bool,
        session_speed_bps: u64,
        speed_last_downloaded: u64,
        speed_last_sample: Instant,
        progress_window_open: bool,
        selected_download_id: u64,
        checking_link: bool,
        checked_link: Option<(String, Result<LinkInfo, String>)>,
        status: String,
        tx: mpsc::Sender<UiEvent>,
        rx: mpsc::Receiver<UiEvent>,
    }

    impl DownloadManager {
        fn new(initial_url: Option<String>) -> Self {
            let default_folder = default_download_folder();
            let (folder_input, entries, next_id, status) = match load_queue() {
                Ok(Some(queue)) => {
                    let saved_folder = PathBuf::from(queue.folder);
                    let folder = if is_legacy_default_download_folder(&saved_folder) {
                        default_folder.clone()
                    } else {
                        saved_folder
                    };
                    let mut entries = Vec::with_capacity(queue.entries.len());
                    let mut failure = None;
                    for saved in queue.entries {
                        let scheduled_at = match saved.scheduled_at.as_deref() {
                            Some(value) => match chrono::DateTime::parse_from_rfc3339(value) {
                                Ok(value) => Some(value.with_timezone(&Local)),
                                Err(error) => {
                                    failure = Some(format!(
                                        "The saved download schedule is invalid: {error}"
                                    ));
                                    break;
                                }
                            },
                            None => None,
                        };
                        let status = normalize_saved_status(saved.status);
                        let destination = PathBuf::from(saved.destination);
                        let progress = Arc::new(TransferProgress::new());
                        if status == "Completed" {
                            if let Ok(metadata) = std::fs::metadata(&destination) {
                                progress.downloaded.store(metadata.len(), Ordering::Relaxed);
                                progress.total.store(metadata.len(), Ordering::Relaxed);
                            }
                        } else if status == "Paused" {
                            let mut partial = destination.as_os_str().to_os_string();
                            partial.push(".part");
                            if let Ok(metadata) = std::fs::metadata(PathBuf::from(partial)) {
                                progress.downloaded.store(metadata.len(), Ordering::Relaxed);
                            }
                        }
                        entries.push(DownloadEntry {
                            id: saved.id,
                            url: saved.url,
                            destination,
                            scheduled_at,
                            status,
                            progress,
                            segment_lanes: Vec::new(),
                            merge_downloaded: 0,
                            merge_total: 0,
                            merging: false,
                            browser_handoff: None,
                            elapsed_download_ms: saved.elapsed_download_ms,
                            active_download_since: None,
                        });
                    }
                    if let Some(error) = failure {
                        (
                            default_folder.to_string_lossy().into_owned(),
                            Vec::new(),
                            1,
                            format!("Could not restore the queue: {error}"),
                        )
                    } else {
                        let next_id = entries
                            .iter()
                            .map(|entry| entry.id)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(1);
                        (
                            folder.to_string_lossy().into_owned(),
                            entries,
                            next_id,
                            "Restored the saved queue.".to_owned(),
                        )
                    }
                }
                Ok(None) => (
                    default_folder.to_string_lossy().into_owned(),
                    Vec::new(),
                    1,
                    "Ready".to_owned(),
                ),
                Err(error) => (
                    default_folder.to_string_lossy().into_owned(),
                    Vec::new(),
                    1,
                    format!("Could not read the saved queue: {error}"),
                ),
            };
            let (tx, rx) = mpsc::channel();
            Self {
                url_input: initial_url.unwrap_or_default(),
                folder_input,
                schedule_input: String::new(),
                search_input: String::new(),
                queue_filter: QueueFilter::All,
                entries,
                next_id,
                active_id: None,
                queue_running: false,
                session_speed_bps: 0,
                speed_last_downloaded: 0,
                speed_last_sample: Instant::now(),
                progress_window_open: false,
                selected_download_id: 0,
                checking_link: false,
                checked_link: None,
                status,
                tx,
                rx,
            }
        }

        fn add_download(&mut self) {
            let (url, destination) = match self.download_target() {
                Ok(target) => target,
                Err(error) => {
                    self.status = error;
                    return;
                }
            };
            let scheduled_at = if self.schedule_input.trim().is_empty() {
                None
            } else {
                let parsed =
                    NaiveDateTime::parse_from_str(self.schedule_input.trim(), "%Y-%m-%d %H:%M")
                        .ok()
                        .and_then(|value| Local.from_local_datetime(&value).single());
                match parsed {
                    Some(value) if value > Local::now() => Some(value),
                    _ => {
                        self.status = "The schedule is invalid or is not in the future.".to_owned();
                        return;
                    }
                }
            };
            let Some(next_id) = self.next_id.checked_add(1) else {
                self.status = "Queue ID limit reached.".to_owned();
                return;
            };

            self.entries.push(DownloadEntry {
                id: self.next_id,
                url,
                destination,
                scheduled_at,
                status: if self.schedule_input.trim().is_empty() {
                    "Queued".to_owned()
                } else {
                    "Scheduled".to_owned()
                },
                progress: Arc::new(TransferProgress::new()),
                segment_lanes: Vec::new(),
                merge_downloaded: 0,
                merge_total: 0,
                merging: false,
                browser_handoff: None,
                elapsed_download_ms: 0,
                active_download_since: None,
            });
            self.next_id = next_id;
            self.url_input.clear();
            self.schedule_input.clear();
            self.status = "Added to the queue.".to_owned();
            self.persist_or_report();
        }

        fn download_now(&mut self) {
            if self.active_id.is_some() {
                self.status =
                    "Wait for the current download to finish or pause it first.".to_owned();
                return;
            }
            let (url, destination) = match self.download_target() {
                Ok(target) => target,
                Err(error) => {
                    self.status = error;
                    return;
                }
            };
            let Some(next_id) = self.next_id.checked_add(1) else {
                self.status = "Download ID limit reached.".to_owned();
                return;
            };
            let id = self.next_id;
            self.entries.push(DownloadEntry {
                id,
                url,
                destination,
                scheduled_at: None,
                status: "Queued".to_owned(),
                progress: Arc::new(TransferProgress::new()),
                segment_lanes: Vec::new(),
                merge_downloaded: 0,
                merge_total: 0,
                merging: false,
                browser_handoff: None,
                elapsed_download_ms: 0,
                active_download_since: None,
            });
            self.next_id = next_id;
            self.url_input.clear();
            self.schedule_input.clear();
            self.start_entry(id);
        }

        fn download_target(&self) -> Result<(String, PathBuf), String> {
            Self::download_target_for(&self.url_input, &self.folder_input, false)
        }

        fn download_target_for(
            url: &str,
            folder: &str,
            allow_missing_filename: bool,
        ) -> Result<(String, PathBuf), String> {
            Self::download_target_with_filename(url, folder, allow_missing_filename, None)
        }

        fn download_target_with_filename(
            url: &str,
            folder: &str,
            allow_missing_filename: bool,
            suggested_filename: Option<&str>,
        ) -> Result<(String, PathBuf), String> {
            let parsed = match reqwest::Url::parse(url.trim()) {
                Ok(url) if url.scheme() == "http" || url.scheme() == "https" => url,
                _ => return Err("Enter a valid HTTP or HTTPS URL.".to_owned()),
            };
            let url_name = parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty());
            let suggested_name = suggested_filename
                .filter(|name| !name.trim().is_empty())
                .map(|name| {
                    name.rsplit(['/', '\\'])
                        .find(|name| !name.is_empty())
                        .unwrap_or(name)
                });
            let name = suggested_name.or(url_name);
            let Some(name) = name.or_else(|| allow_missing_filename.then_some("download")) else {
                return Err("The URL has no filename; use a direct link to a file.".to_owned());
            };
            let folder = if folder.trim().is_empty() {
                default_download_folder()
            } else {
                PathBuf::from(folder.trim())
            };
            Ok((parsed.to_string(), folder.join(safe_file_name(name))))
        }

        fn stage_browser_download(
            &mut self,
            url: String,
            suggested_filename: Option<String>,
            port: u16,
            token: String,
        ) -> Result<(), String> {
            let (url, destination) = Self::download_target_with_filename(
                &url,
                &self.folder_input,
                true,
                suggested_filename.as_deref(),
            )?;
            let Some(next_id) = self.next_id.checked_add(1) else {
                return Err("Download ID limit reached.".to_owned());
            };
            let id = self.next_id;
            self.entries.push(DownloadEntry {
                id,
                url,
                destination,
                scheduled_at: None,
                status: "Awaiting confirmation".to_owned(),
                progress: Arc::new(TransferProgress::new()),
                segment_lanes: Vec::new(),
                merge_downloaded: 0,
                merge_total: 0,
                merging: false,
                browser_handoff: Some(BrowserHandoff {
                    address: SocketAddr::from(([127, 0, 0, 1], port)),
                    token,
                }),
                elapsed_download_ms: 0,
                active_download_since: None,
            });
            self.next_id = next_id;
            self.selected_download_id = id;
            self.status = "Edge is waiting for you to confirm this download.".to_owned();
            self.progress_window_open = true;
            Ok(())
        }

        fn decide_browser_download(&mut self, id: u64, accepted: bool) {
            let Some(handoff) = self
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .and_then(|entry| entry.browser_handoff.clone())
            else {
                return;
            };
            if let Err(error) = send_browser_decision(&handoff, accepted) {
                self.status = format!("Could not respond to Edge: {error}");
                return;
            }
            if accepted {
                if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                    entry.browser_handoff = None;
                    entry.status = "Queued".to_owned();
                }
                self.queue_running = true;
                if let Some(active_id) = self.active_id {
                    self.selected_download_id = active_id;
                    self.status =
                        "Confirmed; the download is queued behind the active transfer.".to_owned();
                } else {
                    self.selected_download_id = id;
                    self.status = "Confirmed; starting the selected download.".to_owned();
                }
                self.persist_or_report();
                if self.active_id.is_none() {
                    self.start_entry(id);
                }
            } else {
                self.entries.retain(|entry| entry.id != id);
                self.status = "Declined; Edge will resume the download.".to_owned();
            }
        }

        fn reject_pending_browser_downloads(&mut self) {
            let pending = self
                .entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .browser_handoff
                        .as_ref()
                        .map(|handoff| (entry.id, handoff.clone()))
                })
                .collect::<Vec<_>>();
            for (_, handoff) in pending {
                if let Err(error) = send_browser_decision(&handoff, false) {
                    self.status = format!("Could not notify Edge while closing: {error}");
                }
            }
            self.entries.retain(|entry| entry.browser_handoff.is_none());
        }

        fn pause_queue(&mut self) {
            self.queue_running = false;
            if let Some(id) = self.active_id {
                if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                    entry
                        .progress
                        .cancel_requested
                        .store(false, Ordering::Relaxed);
                    entry.progress.cancelled.store(true, Ordering::Relaxed);
                    entry.status = "Pausing...".to_owned();
                }
                self.status = "Pausing after saving downloaded data.".to_owned();
            } else {
                self.status = "The queue is paused.".to_owned();
            }
            self.persist_or_report();
        }

        fn start_next(&mut self) {
            if !self.queue_running || self.active_id.is_some() {
                return;
            }
            let now = Local::now();
            let Some(index) = self.entries.iter().position(|entry| {
                entry.status == "Queued"
                    || entry.status == "Paused"
                    || (entry.status == "Scheduled"
                        && entry.scheduled_at.is_some_and(|time| time <= now))
            }) else {
                if self.entries.iter().any(|entry| entry.status == "Scheduled") {
                    self.status = "Waiting for a scheduled download...".to_owned();
                } else {
                    self.queue_running = false;
                    self.status = "The queue is complete.".to_owned();
                }
                return;
            };

            let id = self.entries[index].id;
            self.start_entry(id);
        }

        fn start_entry(&mut self, id: u64) {
            let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
                self.status = "Download not found.".to_owned();
                return;
            };
            entry.status = "Downloading".to_owned();
            entry.active_download_since = Some(Instant::now());
            entry.progress.cancelled.store(false, Ordering::Relaxed);
            entry
                .progress
                .cancel_requested
                .store(false, Ordering::Relaxed);
            entry.segment_lanes.clear();
            entry.merge_downloaded = 0;
            entry.merge_total = 0;
            entry.merging = false;
            let url = entry.url.clone();
            let destination = entry.destination.clone();
            let progress = Arc::clone(&entry.progress);
            self.speed_last_downloaded = progress.downloaded.load(Ordering::Relaxed);
            self.speed_last_sample = Instant::now();
            self.session_speed_bps = 0;
            let tx = self.tx.clone();
            self.active_id = Some(id);
            self.selected_download_id = id;
            self.progress_window_open = true;
            self.status = format!("Downloading {}", destination.display());
            self.persist_or_report();

            thread::spawn(move || {
                let resolved_destination = match probe_download(&url) {
                    Ok(info) => match info.server_filename {
                        Some(filename) => {
                            let path = destination.with_file_name(safe_file_name(&filename));
                            let _ = tx.send(UiEvent::DownloadFilenameResolved(
                                id,
                                Ok(Some(path.clone())),
                            ));
                            path
                        }
                        None => {
                            let _ = tx.send(UiEvent::DownloadFilenameResolved(id, Ok(None)));
                            destination
                        }
                    },
                    Err(error) => {
                        let _ = tx.send(UiEvent::DownloadFilenameResolved(id, Err(error)));
                        destination
                    }
                };
                let result =
                    run_native_download(&url, &resolved_destination, progress, tx.clone(), id);
                let _ = tx.send(UiEvent::DownloadFinished(id, result));
            });
        }

        fn handle_events(&mut self) {
            while let Ok(event) = self.rx.try_recv() {
                match event {
                    UiEvent::Progress(id, downloaded, total) => {
                        if let Some(entry) = self.entries.iter().find(|entry| entry.id == id) {
                            entry
                                .progress
                                .downloaded
                                .fetch_max(downloaded, Ordering::Relaxed);
                            entry.progress.total.store(total, Ordering::Relaxed);
                        }
                    }
                    UiEvent::SegmentMode(id, count) => {
                        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                            entry.segment_lanes = vec![SegmentLane::default(); count];
                            entry.merge_downloaded = 0;
                            entry.merge_total = 0;
                            entry.merging = false;
                        }
                    }
                    UiEvent::SegmentProgress(id, index, downloaded, total, active) => {
                        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                            if index == entry.segment_lanes.len() {
                                entry.merge_downloaded = downloaded;
                                entry.merge_total = total;
                                entry.merging |= active;
                            } else if let Some(lane) = entry.segment_lanes.get_mut(index) {
                                lane.downloaded = downloaded;
                                lane.total = total;
                                lane.active = active;
                            }
                        }
                    }
                    UiEvent::DownloadFilenameResolved(id, result) => match result {
                        Ok(Some(destination)) => {
                            if let Some(entry) =
                                self.entries.iter_mut().find(|entry| entry.id == id)
                            {
                                entry.destination = destination;
                                self.status = format!(
                                    "Using server filename: {}",
                                    entry
                                        .destination
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                );
                                self.persist_or_report();
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.status = format!(
                                "Could not read the server filename ({error}); continuing with the URL name."
                            );
                        }
                    },
                    UiEvent::DownloadFinished(id, result) => {
                        if self.active_id != Some(id) {
                            continue;
                        }
                        self.active_id = None;
                        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                            entry.elapsed_download_ms =
                                entry.elapsed_download().as_millis().min(u64::MAX as u128) as u64;
                            entry.active_download_since = None;
                            entry.merging = false;
                            for lane in &mut entry.segment_lanes {
                                lane.active = false;
                            }
                            match result {
                                Ok(()) => {
                                    entry.status = "Completed".to_owned();
                                    entry.progress.downloaded.store(
                                        entry.progress.total.load(Ordering::Relaxed),
                                        Ordering::Relaxed,
                                    );
                                    self.status =
                                        format!("Downloaded {}", entry.destination.display());
                                }
                                Err(error) if entry.progress.cancelled.load(Ordering::Relaxed) => {
                                    if entry
                                        .progress
                                        .cancel_requested
                                        .swap(false, Ordering::Relaxed)
                                    {
                                        match discard_partial_download(
                                            &entry.destination,
                                            &entry.url,
                                        ) {
                                            Ok(()) => {
                                                entry.status = "Cancelled".to_owned();
                                                self.status = format!(
                                                    "Cancelled and removed partial data: {}",
                                                    entry.destination.display()
                                                );
                                            }
                                            Err(cleanup_error) => {
                                                entry.status =
                                                    "Cancelled (partial data remains)".to_owned();
                                                self.status = format!(
                                                    "Cancelled, but could not remove partial data: {cleanup_error}"
                                                );
                                            }
                                        }
                                    } else {
                                        entry.status = "Paused".to_owned();
                                        self.status = if error.is_empty() {
                                            "Paused. You can resume the download.".to_owned()
                                        } else {
                                            format!("Paused: {error}")
                                        };
                                    }
                                }
                                Err(error) => {
                                    entry.status = format!("Error: {error}");
                                    self.status = format!("Download failed: {error}");
                                }
                            }
                        }
                        self.persist_or_report();
                    }
                    UiEvent::LinkChecked(url, result) => {
                        self.checking_link = false;
                        if self.url_input.trim() == url {
                            self.checked_link = Some((url, result));
                            self.status = "Link check complete.".to_owned();
                        }
                    }
                }
            }
            self.update_session_speed();
        }

        fn update_session_speed(&mut self) {
            let Some(active_id) = self.active_id else {
                self.session_speed_bps = 0;
                self.speed_last_downloaded = 0;
                self.speed_last_sample = Instant::now();
                return;
            };
            let Some(entry) = self.entries.iter().find(|entry| entry.id == active_id) else {
                self.session_speed_bps = 0;
                return;
            };
            let now = Instant::now();
            let elapsed = now.duration_since(self.speed_last_sample);
            if elapsed < std::time::Duration::from_millis(400) {
                return;
            }
            let downloaded = entry.progress.downloaded.load(Ordering::Relaxed);
            let delta = downloaded.saturating_sub(self.speed_last_downloaded);
            let instantaneous = (delta as f64 / elapsed.as_secs_f64()) as u64;
            self.session_speed_bps = if self.session_speed_bps == 0 {
                instantaneous
            } else {
                self.session_speed_bps
                    .saturating_mul(2)
                    .saturating_add(instantaneous)
                    / 3
            };
            self.speed_last_downloaded = downloaded;
            self.speed_last_sample = now;
        }

        fn check_link(&mut self) {
            let url = self.url_input.trim().to_owned();
            if url.is_empty() {
                self.status = "Enter a link to check first.".to_owned();
                return;
            }
            self.checking_link = true;
            self.checked_link = None;
            self.status = "Checking link...".to_owned();
            let tx = self.tx.clone();
            thread::spawn(move || {
                let result = probe_download(&url);
                let _ = tx.send(UiEvent::LinkChecked(url, result));
            });
        }

        fn persist_or_report(&mut self) {
            if let Err(error) = self.persist_queue() {
                self.status = format!("Could not save the queue: {error}");
            }
        }

        fn persist_queue(&self) -> Result<(), String> {
            let queue = PersistedQueue {
                version: 1,
                folder: self.folder_input.clone(),
                entries: self
                    .entries
                    .iter()
                    .filter(|entry| entry.browser_handoff.is_none())
                    .map(|entry| PersistedEntry {
                        id: entry.id,
                        url: entry.url.clone(),
                        destination: entry.destination.to_string_lossy().into_owned(),
                        scheduled_at: entry.scheduled_at.map(|time| time.to_rfc3339()),
                        status: entry.status.clone(),
                        elapsed_download_ms: entry
                            .elapsed_download()
                            .as_millis()
                            .min(u64::MAX as u128)
                            as u64,
                    })
                    .collect(),
            };
            save_queue(&queue)
        }

        fn remove_entry(&mut self, id: u64) {
            if self.active_id == Some(id) {
                self.status = "Cannot remove an active download.".to_owned();
                return;
            }
            self.entries.retain(|entry| entry.id != id);
            self.status = "Removed the download from the queue.".to_owned();
            self.persist_or_report();
        }

        fn pause_entry(&mut self, id: u64) {
            if self.active_id != Some(id) {
                return;
            }
            self.queue_running = false;
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                entry
                    .progress
                    .cancel_requested
                    .store(false, Ordering::Relaxed);
                entry.progress.cancelled.store(true, Ordering::Relaxed);
                entry.status = "Pausing...".to_owned();
                self.status = "Saving downloaded data before pausing...".to_owned();
            }
        }

        fn cancel_entry(&mut self, id: u64) {
            if self.active_id != Some(id) {
                return;
            }
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                entry
                    .progress
                    .cancel_requested
                    .store(true, Ordering::Relaxed);
                entry.progress.cancelled.store(true, Ordering::Relaxed);
                entry.status = "Cancelling...".to_owned();
                self.status = "Cancelling the download and removing partial data...".to_owned();
            }
        }

        fn retry_entry(&mut self, id: u64) {
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                if self.active_id == Some(id) {
                    return;
                }
                entry.status = "Queued".to_owned();
                entry.progress.cancelled.store(false, Ordering::Relaxed);
                entry
                    .progress
                    .cancel_requested
                    .store(false, Ordering::Relaxed);
                entry.progress.downloaded.store(0, Ordering::Relaxed);
                entry.progress.total.store(0, Ordering::Relaxed);
                entry.segment_lanes.clear();
                entry.merge_downloaded = 0;
                entry.merge_total = 0;
                entry.merging = false;
                entry.elapsed_download_ms = 0;
                entry.active_download_since = None;
                self.status = "Added the download to the queue for retry.".to_owned();
                self.persist_or_report();
            }
        }

        fn clear_completed(&mut self) {
            let previous = self.entries.len();
            self.entries.retain(|entry| entry.status != "Completed");
            let removed = previous - self.entries.len();
            self.status = format!("Removed {removed} completed downloads from the queue.");
            self.persist_or_report();
        }

        fn handle_entry_action(&mut self, action: EntryAction) {
            match action {
                EntryAction::OpenFolder(id) => {
                    if let Some(entry) = self.entries.iter().find(|entry| entry.id == id) {
                        let folder = entry.destination.parent().unwrap_or_else(|| Path::new("."));
                        match open_download_folder(folder) {
                            Ok(()) => self.status = "Opened the download folder.".to_owned(),
                            Err(error) => {
                                self.status = format!("Could not open the download folder: {error}")
                            }
                        }
                    }
                }
                EntryAction::CopyUrl(id) => {
                    if let Some(entry) = self.entries.iter().find(|entry| entry.id == id) {
                        match copy_to_clipboard(&entry.url) {
                            Ok(()) => self.status = "URL copied.".to_owned(),
                            Err(error) => self.status = format!("Could not copy the URL: {error}"),
                        }
                    }
                }
                EntryAction::Retry(id) => self.retry_entry(id),
                EntryAction::Remove(id) => self.remove_entry(id),
            }
        }
    }

    mod win32_ui {
        include!("win32_ui.rs");
    }

    pub fn run(
        initial_url: Option<String>,
        auto_download: bool,
        browser_handoff: Option<(String, String, u16, String)>,
    ) -> Result<(), String> {
        win32_ui::run(initial_url, auto_download, browser_handoff)
    }

    pub fn forward_browser_handoff(
        url: &str,
        suggested_filename: &str,
        port: u16,
        token: &str,
    ) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            FindWindowW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_COPYDATA,
        };

        let class_name: Vec<u16> = "RdmMainWindow".encode_utf16().chain(Some(0)).collect();
        let window = unsafe { FindWindowW(class_name.as_ptr(), std::ptr::null()) };
        if window.is_null() {
            return false;
        }
        let Ok(payload) = serde_json::to_vec(&serde_json::json!({
            "url": url,
            "filename": suggested_filename,
            "port": port,
            "token": token,
        })) else {
            return false;
        };
        if payload.len() > u32::MAX as usize {
            return false;
        }
        let copy_data = BrowserCopyData {
            data_id: BROWSER_DOWNLOAD_COPYDATA_ID,
            byte_count: payload.len() as u32,
            data: payload.as_ptr().cast_mut().cast(),
        };
        let mut result = 0usize;
        let delivered = unsafe {
            SendMessageTimeoutW(
                window,
                WM_COPYDATA,
                0,
                (&copy_data as *const BrowserCopyData) as isize,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                2000,
                &mut result,
            )
        };
        delivered != 0 && result != 0
    }

    fn default_download_folder() -> PathBuf {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::System::Com::CoTaskMemFree;
        use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};

        let mut path = std::ptr::null_mut();
        let result = unsafe {
            SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut path)
        };
        if result >= 0 && !path.is_null() {
            let mut length = 0;
            unsafe {
                while *path.add(length) != 0 {
                    length += 1;
                }
            }
            let value =
                unsafe { std::ffi::OsString::from_wide(std::slice::from_raw_parts(path, length)) };
            unsafe { CoTaskMemFree(path.cast()) };
            return PathBuf::from(value);
        }

        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .map(|path| path.join("Downloads"))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn is_legacy_default_download_folder(folder: &Path) -> bool {
        let Some(profile) = std::env::var_os("USERPROFILE") else {
            return false;
        };
        let legacy = PathBuf::from(profile).join("Downloads");
        let normalize = |path: &Path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_owned()
        };
        normalize(folder).eq_ignore_ascii_case(&normalize(&legacy))
    }

    fn queue_file() -> Result<PathBuf, String> {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "LOCALAPPDATA was not found.".to_owned())?;
        Ok(local_app_data.join("RDM").join("queue.json"))
    }

    fn load_queue() -> Result<Option<PersistedQueue>, String> {
        let path = queue_file()?;
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let queue: PersistedQueue =
            serde_json::from_slice(&data).map_err(|error| error.to_string())?;
        if queue.version != 1 {
            return Err(format!("Queue version {} is not supported.", queue.version));
        }
        Ok(Some(queue))
    }

    fn save_queue(queue: &PersistedQueue) -> Result<(), String> {
        let path = queue_file()?;
        let parent = path
            .parent()
            .ok_or_else(|| "The queue path is invalid.".to_owned())?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temp_path = path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(queue).map_err(|error| error.to_string())?;
        let mut file = std::fs::File::create(&temp_path).map_err(|error| error.to_string())?;
        use std::io::Write;
        file.write_all(&data).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);

        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let source: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let succeeded = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    fn run_native_download(
        url: &str,
        destination: &Path,
        progress: Arc<TransferProgress>,
        tx: mpsc::Sender<UiEvent>,
        id: u64,
    ) -> Result<(), String> {
        std::fs::create_dir_all(destination.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|error| error.to_string())?;
        let progress_for_download = Arc::clone(&progress);
        let cancel_for_download = Arc::clone(&progress);
        let tx_progress = tx.clone();
        match download_segmented_with_progress(
            url,
            destination,
            0,
            move |downloaded, total| {
                progress_for_download
                    .downloaded
                    .fetch_max(downloaded, Ordering::Relaxed);
                progress_for_download.total.store(total, Ordering::Relaxed);
                let _ = tx_progress.send(UiEvent::Progress(id, downloaded, total));
            },
            move || cancel_for_download.cancelled.load(Ordering::Relaxed),
            {
                let tx_mode = tx.clone();
                move |count| {
                    let _ = tx_mode.send(UiEvent::SegmentMode(id, count));
                }
            },
            {
                let tx_segment = tx.clone();
                move |index, downloaded, total, active| {
                    let _ = tx_segment.send(UiEvent::SegmentProgress(
                        id, index, downloaded, total, active,
                    ));
                }
            },
        )? {
            rdm_engine::SegmentedOutcome::Complete => return Ok(()),
            rdm_engine::SegmentedOutcome::Cancelled => return Err(String::new()),
            rdm_engine::SegmentedOutcome::Unsupported => {
                let _ = tx.send(UiEvent::SegmentMode(id, 1));
                let _ = tx.send(UiEvent::SegmentProgress(id, 0, 0, 0, true));
            }
        }
        run_serial_download(url, destination, progress, tx, id)
    }

    fn run_serial_download(
        url: &str,
        destination: &Path,
        progress: Arc<TransferProgress>,
        tx: mpsc::Sender<UiEvent>,
        id: u64,
    ) -> Result<(), String> {
        let url = CString::new(url).map_err(|_| "The URL contains a NUL character.".to_owned())?;
        let destination = CString::new(destination.to_string_lossy().as_bytes())
            .map_err(|_| "The path contains a NUL character.".to_owned())?;
        let mut error_buffer = vec![0i8; 2048];
        let callback_progress = Box::new((progress, tx, id));
        let context = (&*callback_progress
            as *const (Arc<TransferProgress>, mpsc::Sender<UiEvent>, u64))
            .cast_mut()
            .cast();
        let progress_callback: ProgressCallback = Some(report_progress);
        let cancel_callback: CancelCallback = Some(check_cancelled);
        let result = unsafe {
            rdm_download(
                url.as_ptr(),
                destination.as_ptr(),
                progress_callback,
                cancel_callback,
                context,
                error_buffer.as_mut_ptr(),
                error_buffer.len(),
            )
        };
        match result {
            0 => Ok(()),
            1 => Err(String::new()),
            _ => {
                let message = unsafe { CStr::from_ptr(error_buffer.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                Err(if message.is_empty() {
                    "Could not download the file.".to_owned()
                } else {
                    message
                })
            }
        }
    }

    unsafe extern "C" fn report_progress(
        downloaded: u64,
        total: u64,
        context: *mut std::ffi::c_void,
    ) {
        if context.is_null() {
            return;
        }
        let (progress, tx, id) =
            unsafe { &*context.cast::<(Arc<TransferProgress>, mpsc::Sender<UiEvent>, u64)>() };
        progress.downloaded.store(downloaded, Ordering::Relaxed);
        progress.total.store(total, Ordering::Relaxed);
        let _ = tx.send(UiEvent::Progress(*id, downloaded, total));
        let _ = tx.send(UiEvent::SegmentProgress(*id, 0, downloaded, total, true));
    }

    unsafe extern "C" fn check_cancelled(context: *mut std::ffi::c_void) -> bool {
        if context.is_null() {
            return true;
        }
        let (progress, _, _) =
            unsafe { &*context.cast::<(Arc<TransferProgress>, mpsc::Sender<UiEvent>, u64)>() };
        progress.cancelled.load(Ordering::Relaxed)
    }

    fn safe_file_name(name: &str) -> String {
        let cleaned: String = name
            .chars()
            .map(|character| {
                if character.is_control() || "<>:\"/\\|?*".contains(character) {
                    '_'
                } else {
                    character
                }
            })
            .collect();
        if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
            "download".to_owned()
        } else {
            cleaned
        }
    }

    fn format_bytes(bytes: u64) -> String {
        const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
        let mut value = bytes as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        format!("{value:.1} {}", UNITS[unit])
    }

    fn format_speed(bytes_per_second: u64) -> String {
        format!("{}/s", format_bytes(bytes_per_second))
    }

    fn open_download_folder(folder: &Path) -> Result<(), String> {
        std::process::Command::new("explorer.exe")
            .arg(folder)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn copy_to_clipboard(value: &str) -> Result<(), String> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut process = Command::new("clip.exe")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not start clip.exe: {error}"))?;
        let mut input = process
            .stdin
            .take()
            .ok_or_else(|| "Could not open clip.exe input.".to_owned())?;
        input
            .write_all(value.as_bytes())
            .map_err(|error| format!("Could not send the URL to the clipboard: {error}"))?;
        drop(input);
        let status = process
            .wait()
            .map_err(|error| format!("Could not wait for clip.exe: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("clip.exe exited with code {status}."))
        }
    }
}

#[cfg(windows)]
fn main() {
    if is_native_host_executable() || is_native_messaging_process() {
        if let Err(error) = native_host::run() {
            eprintln!("RDM native messaging error: {error}");
        }
        return;
    }

    #[cfg(windows)]
    fn is_native_host_executable() -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_owned()))
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("rdm-host.exe"))
    }

    if let Err(error) = windows_app::setup_first_run() {
        show_startup_error(&format!(
            "RDM could not configure browser integration: {error}"
        ));
    }

    let mut args = std::env::args().skip(1);
    let mode = args.next();
    let (initial_url, auto_download, browser_handoff) = match mode.as_deref() {
        Some("--capture-url") => match args.next() {
            Some(url) => match parse_capture_url(&url) {
                Ok(url) => (Some(url), false, None),
                Err(error) => {
                    show_startup_error(&format!("RDM could not open this link: {error}"));
                    return;
                }
            },
            None => {
                show_startup_error("RDM did not receive a URL.");
                return;
            }
        },
        Some("--download-url") => (args.next(), true, None),
        Some("--browser-download") => {
            let url = args.next();
            let filename = args.next().unwrap_or_default();
            let port = args.next().and_then(|value| value.parse::<u16>().ok());
            let token = args.next();
            match (url, port, token) {
                (Some(url), Some(port), Some(token)) if !token.is_empty() => {
                    (None, false, Some((url, filename, port, token)))
                }
                _ => {
                    eprintln!("Invalid browser handoff arguments.");
                    return;
                }
            }
        }
        _ => (None, false, None),
    };
    if let Some((url, filename, port, token)) = browser_handoff.as_ref() {
        if windows_app::forward_browser_handoff(url, filename, *port, token) {
            return;
        }
    }
    if let Err(error) = windows_app::run(initial_url, auto_download, browser_handoff) {
        show_startup_error(&format!("RDM could not start: {error}"));
    }
}

#[cfg(windows)]
fn parse_capture_url(value: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(value).map_err(|error| error.to_string())?;
    let url = if parsed.scheme() == "rdm" {
        parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "url").then(|| value.into_owned()))
            .ok_or_else(|| "The rdm:// link is missing its url parameter.".to_owned())?
    } else {
        value.to_owned()
    };
    let parsed = reqwest::Url::parse(&url).map_err(|error| error.to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("Only HTTP and HTTPS download links are supported.".to_owned());
    }
    Ok(parsed.into())
}

#[cfg(windows)]
fn show_startup_error(message: &str) {
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "RDM".encode_utf16().chain(Some(0)).collect();
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }
}

#[cfg(windows)]
fn is_native_messaging_process() -> bool {
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};

    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        !input.is_null()
            && !output.is_null()
            && GetFileType(input) == FILE_TYPE_PIPE
            && GetFileType(output) == FILE_TYPE_PIPE
    }
}

#[cfg(all(windows, test))]
mod startup_tests {
    use super::parse_capture_url;

    #[test]
    fn parses_encoded_rdm_link() {
        assert_eq!(
            parse_capture_url(
                "rdm://download?url=https%3A%2F%2Fexample.com%2Ffile.zip%3Ftoken%3Da%252Fb"
            )
            .unwrap(),
            "https://example.com/file.zip?token=a%2Fb"
        );
    }

    #[test]
    fn accepts_direct_http_url() {
        assert_eq!(
            parse_capture_url("https://example.com/file.zip").unwrap(),
            "https://example.com/file.zip"
        );
    }

    #[test]
    fn rejects_non_http_download_url() {
        assert!(parse_capture_url("file:///C:/private.txt").is_err());
        assert!(parse_capture_url("rdm://download").is_err());
    }
}

#[cfg(test)]
mod overall_progress_tests {
    use super::*;

    #[test]
    fn overall_progress_includes_assembly_before_completion() {
        assert_eq!(
            overall_progress(50, 100, false, 0, 0, false),
            (4_750, OVERALL_PROGRESS_SCALE)
        );
        assert_eq!(
            overall_progress(100, 100, false, 0, 0, false),
            (DOWNLOAD_PROGRESS_WEIGHT, OVERALL_PROGRESS_SCALE)
        );
        assert_eq!(
            overall_progress(100, 100, true, 50, 100, false),
            (9_750, OVERALL_PROGRESS_SCALE)
        );
        assert_eq!(
            overall_progress(100, 100, true, 100, 100, false),
            (OVERALL_PROGRESS_SCALE - 1, OVERALL_PROGRESS_SCALE)
        );
        assert_eq!(
            overall_progress(100, 100, false, 0, 0, true),
            (OVERALL_PROGRESS_SCALE, OVERALL_PROGRESS_SCALE)
        );
    }

    #[test]
    fn unknown_length_downloads_remain_indeterminate_until_complete() {
        assert_eq!(
            overall_progress(4096, 0, false, 0, 0, false),
            (0, OVERALL_PROGRESS_SCALE)
        );
        assert_eq!(
            overall_progress(4096, 0, false, 0, 0, true),
            (OVERALL_PROGRESS_SCALE, OVERALL_PROGRESS_SCALE)
        );
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("RDM can only run on Windows.");
}
