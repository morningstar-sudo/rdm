use reqwest::blocking::{Client, Response};
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, LOCATION, RANGE, SET_COOKIE,
};
use std::ffi::{c_char, CStr};
use std::fs::{self, File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BUFFER_SIZE: usize = 64 * 1024;
const MAX_SEGMENT_COUNT: usize = 20;
const MAX_STORED_SEGMENT_FILES: usize = 100;
const MAX_CONNECTION_COUNT: usize = 16;
const TEMP_URL_MARKER: &str = ".rdm-url";
const TEMP_DESTINATION_MARKER: &str = ".rdm-destination";
const RESULT_OK: i32 = 0;
const RESULT_INTERRUPTED: i32 = 1;
const RESULT_ERROR: i32 = -1;

pub type ProgressCallback =
    Option<unsafe extern "C" fn(downloaded: u64, total: u64, context: *mut std::ffi::c_void)>;
pub type CancelCallback = Option<unsafe extern "C" fn(context: *mut std::ffi::c_void) -> bool>;

#[derive(Default)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
}

fn sidecar_path(part_path: &Path) -> PathBuf {
    let mut name = part_path.as_os_str().to_os_string();
    name.push(".meta");
    PathBuf::from(name)
}

#[cfg(windows)]
fn replace_download_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let succeeded = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_download_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

fn legacy_part_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

fn part_path(temp_dir: &Path) -> PathBuf {
    temp_dir.join("download.part")
}

fn url_hash(url: &str) -> String {
    let hash = url
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    format!("{hash:016x}")
}

fn temporary_directory_prefix(url: &str) -> String {
    format!(".rdm-{}-", url_hash(url))
}

fn temporary_directory_parent(destination: &Path) -> &Path {
    destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn matches_temp_directory(directory: &Path, destination: &Path, url: &str) -> Result<bool, String> {
    let stored_url = match fs::read_to_string(directory.join(TEMP_URL_MARKER)) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let stored_destination = match fs::read_to_string(directory.join(TEMP_DESTINATION_MARKER)) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    Ok(stored_url == url && stored_destination.as_str() == destination.to_string_lossy().as_ref())
}

fn find_temporary_directory(destination: &Path, url: &str) -> Result<Option<PathBuf>, String> {
    let parent = temporary_directory_parent(destination);
    if !parent.exists() {
        return Ok(None);
    }
    let prefix = temporary_directory_prefix(url);
    for entry in fs::read_dir(parent).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
            || !entry.file_name().to_string_lossy().starts_with(&prefix)
        {
            continue;
        }
        let directory = entry.path();
        if matches_temp_directory(&directory, destination, url)? {
            return Ok(Some(directory));
        }
    }
    Ok(None)
}

fn temporary_directory_path(destination: &Path, url: &str) -> Result<PathBuf, String> {
    let parent = temporary_directory_parent(destination);
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    if let Some(directory) = find_temporary_directory(destination, url)? {
        migrate_legacy_temp_files(destination, &directory).map_err(|error| error.to_string())?;
        return Ok(directory);
    }

    let prefix = temporary_directory_prefix(url);
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let mut random_hasher = DefaultHasher::new();
    timestamp.hash(&mut random_hasher);
    std::process::id().hash(&mut random_hasher);
    counter.hash(&mut random_hasher);
    let directory = parent.join(format!("{prefix}{:016x}", random_hasher.finish()));
    fs::create_dir(&directory).map_err(|error| error.to_string())?;
    let marker_result = fs::write(directory.join(TEMP_URL_MARKER), url).and_then(|()| {
        fs::write(
            directory.join(TEMP_DESTINATION_MARKER),
            destination.to_string_lossy().as_bytes(),
        )
    });
    if let Err(error) = marker_result {
        let _ = fs::remove_dir_all(&directory);
        return Err(error.to_string());
    }
    migrate_legacy_temp_files(destination, &directory).map_err(|error| error.to_string())?;
    Ok(directory)
}

fn migrate_legacy_temp_files(destination: &Path, temp_dir: &Path) -> std::io::Result<()> {
    let legacy_part = legacy_part_path(destination);
    let new_part = part_path(temp_dir);
    let mut paths = vec![
        (legacy_part.clone(), new_part.clone()),
        (sidecar_path(&legacy_part), sidecar_path(&new_part)),
        (
            segmented_manifest_path(&legacy_part),
            segmented_manifest_path(&new_part),
        ),
    ];
    paths.extend((0..MAX_STORED_SEGMENT_FILES).map(|index| {
        (
            segment_path(&legacy_part, index),
            segment_path(&new_part, index),
        )
    }));
    for (source, target) in paths {
        if target.exists() {
            continue;
        }
        match fs::rename(source, target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn discard_partial_download(destination: &Path, url: &str) -> Result<(), String> {
    let parent = temporary_directory_parent(destination);
    let prefix = temporary_directory_prefix(url);
    if parent.exists() {
        for entry in fs::read_dir(parent).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
                || !entry.file_name().to_string_lossy().starts_with(&prefix)
            {
                continue;
            }
            let directory = entry.path();
            if matches_temp_directory(&directory, destination, url)? {
                fs::remove_dir_all(directory).map_err(|error| error.to_string())?;
            }
        }
    }

    let legacy_part = legacy_part_path(destination);
    let mut legacy_paths = vec![
        legacy_part.clone(),
        sidecar_path(&legacy_part),
        segmented_manifest_path(&legacy_part),
    ];
    legacy_paths
        .extend((0..MAX_STORED_SEGMENT_FILES).map(|index| segment_path(&legacy_part, index)));
    for path in legacy_paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn load_validators(part_path: &Path) -> Validators {
    let Ok(data) = fs::read_to_string(sidecar_path(part_path)) else {
        return Validators::default();
    };
    let mut lines = data.lines();
    Validators {
        etag: lines.next().filter(|s| !s.is_empty()).map(str::to_owned),
        last_modified: lines.next().filter(|s| !s.is_empty()).map(str::to_owned),
    }
}

fn save_validators(part_path: &Path, validators: &Validators) -> std::io::Result<()> {
    let mut file = File::create(sidecar_path(part_path))?;
    writeln!(file, "{}", validators.etag.as_deref().unwrap_or_default())?;
    writeln!(
        file,
        "{}",
        validators.last_modified.as_deref().unwrap_or_default()
    )
}

fn header_text(response: &Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn parse_total(response: &Response) -> u64 {
    if let Some(value) = response.headers().get(CONTENT_RANGE) {
        if let Ok(value) = value.to_str() {
            if let Some(total) = value.rsplit('/').next().and_then(|s| s.parse().ok()) {
                return total;
            }
        }
    }
    response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

fn parse_content_range(response: &Response) -> Option<(u64, u64, u64)> {
    response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (range, total) = value.split_once(' ')?.1.split_once('/')?;
            let (start, end) = range.split_once('-')?;
            Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub status: u16,
    pub filename: String,
    pub server_filename: Option<String>,
    pub content_type: Option<String>,
    pub size: Option<u64>,
    pub supports_ranges: bool,
    pub has_validator: bool,
}

pub fn probe_download(url: &str) -> Result<LinkInfo, String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("Only HTTP and HTTPS links are supported.".to_owned());
    }

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("RDM/0.1")
        .build()
        .map_err(|error| error.to_string())?;
    let response = send_range_request(&client, parsed.as_str(), 0, 0, None)?;
    let status = response.status();
    let supports_ranges = status == reqwest::StatusCode::PARTIAL_CONTENT
        && matches!(parse_content_range(&response), Some((0, 0, _)));
    let size = parse_content_range(&response)
        .map(|(_, _, total)| total)
        .or_else(|| {
            response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
        });
    let content_type = header_text(&response, reqwest::header::CONTENT_TYPE);
    let has_validator =
        response.headers().contains_key(ETAG) || response.headers().contains_key(LAST_MODIFIED);
    let server_filename = response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(filename_from_content_disposition);
    let filename = server_filename
        .clone()
        .or_else(|| {
            parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "download".to_owned());

    Ok(LinkInfo {
        status: status.as_u16(),
        filename,
        server_filename,
        content_type,
        size,
        supports_ranges,
        has_validator,
    })
}

fn filename_from_content_disposition(value: &str) -> Option<String> {
    let mut regular_filename = None;
    for parameter in value.split(';').skip(1) {
        let Some((key, raw_value)) = parameter.trim().split_once('=') else {
            continue;
        };
        let raw_value = raw_value.trim().trim_matches('"');
        if key.trim().eq_ignore_ascii_case("filename*") {
            if let Some(filename) = decode_extended_filename(raw_value) {
                return Some(filename);
            }
        } else if key.trim().eq_ignore_ascii_case("filename") && !raw_value.is_empty() {
            regular_filename = Some(raw_value.replace("\\\"", "\"").replace("\\\\", "\\"));
        }
    }
    regular_filename
}

fn decode_extended_filename(value: &str) -> Option<String> {
    let (_, encoded) = value.split_once("''")?;
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut chars = encoded.bytes();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let high = (chars.next()? as char).to_digit(16)?;
            let low = (chars.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    let filename = String::from_utf8(bytes).ok()?;
    (!filename.is_empty()).then_some(filename)
}

fn download(
    url: &str,
    destination: &Path,
    progress: ProgressCallback,
    cancel: CancelCallback,
    context: *mut std::ffi::c_void,
) -> Result<i32, String> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;

    let temp_dir = temporary_directory_path(destination, url)?;
    let part_path = part_path(&temp_dir);
    let mut offset = fs::metadata(&part_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let mut validators = load_validators(&part_path);
    if offset > 0 && validators.etag.is_none() && validators.last_modified.is_none() {
        offset = 0;
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(24 * 60 * 60))
        .cookie_store(true)
        .user_agent("RDM/0.1")
        .build()
        .map_err(|error| error.to_string())?;

    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
        if let Some(etag) = validators.etag.as_deref() {
            request = request.header(IF_RANGE, etag);
        } else if let Some(modified) = validators.last_modified.as_deref() {
            request = request.header(IF_RANGE, modified);
        }
    }
    let mut response = request.send().map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("The server returned HTTP {}", response.status()));
    }

    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let range = parse_content_range(&response);
        if offset == 0
            || !matches!(range, Some((start, end, total)) if start == offset && end.saturating_add(1) == total)
        {
            return Err(
                "The server returned an invalid byte range; partial data was preserved.".to_owned(),
            );
        }
    }
    if offset > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        offset = 0;
    }
    let total = parse_total(&response);
    let response_validators = Validators {
        etag: header_text(&response, ETAG),
        last_modified: header_text(&response, LAST_MODIFIED),
    };
    if offset > 0
        && (validators
            .etag
            .as_ref()
            .zip(response_validators.etag.as_ref())
            .map(|(previous, current)| previous != current)
            .unwrap_or(false)
            || validators
                .last_modified
                .as_ref()
                .zip(response_validators.last_modified.as_ref())
                .map(|(previous, current)| previous != current)
                .unwrap_or(false))
    {
        return Err("The server resource changed; partial data was preserved.".to_owned());
    }
    validators = response_validators;
    save_validators(&part_path, &validators).map_err(|error| error.to_string())?;

    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(&part_path)
        .map_err(|error| error.to_string())?;
    let mut downloaded = offset;
    let mut buffer = vec![0u8; BUFFER_SIZE];

    loop {
        if unsafe { cancel.map(|callback| callback(context)).unwrap_or(false) } {
            output.flush().map_err(|error| error.to_string())?;
            output.sync_all().map_err(|error| error.to_string())?;
            return Ok(RESULT_INTERRUPTED);
        }
        let count = response
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        downloaded = downloaded.saturating_add(count as u64);
        unsafe {
            if let Some(callback) = progress {
                callback(downloaded, total, context);
            }
        }
    }
    output.flush().map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())?;
    drop(output);

    replace_download_file(&part_path, destination).map_err(|error| error.to_string())?;
    fs::remove_dir_all(&temp_dir).map_err(|error| error.to_string())?;
    Ok(RESULT_OK)
}

pub enum SegmentedOutcome {
    Complete,
    Cancelled,
    Unsupported,
}

fn choose_segment_count(total: u64) -> usize {
    total.clamp(2, MAX_SEGMENT_COUNT as u64) as usize
}

fn choose_connection_count(segment_count: usize) -> usize {
    segment_count.min(MAX_CONNECTION_COUNT)
}

fn choose_initial_connection_count(maximum: usize) -> usize {
    maximum.min(4)
}

struct ConcurrencyTuner {
    current: usize,
    maximum: usize,
    probe: Option<(usize, u64)>,
    cooldown_until: Option<std::time::Instant>,
}

impl ConcurrencyTuner {
    fn new(maximum: usize) -> Self {
        Self {
            current: choose_initial_connection_count(maximum),
            maximum,
            probe: None,
            cooldown_until: None,
        }
    }

    fn observe(&mut self, speed_bps: u64, now: std::time::Instant) -> usize {
        if speed_bps == 0 {
            return self.current;
        }
        if let Some((previous_limit, baseline_speed)) = self.probe.take() {
            if u128::from(speed_bps) * 100 >= u128::from(baseline_speed) * 105 {
                if self.current < self.maximum {
                    self.probe = Some((self.current, speed_bps));
                    self.current += 1;
                }
            } else {
                self.current = previous_limit;
                self.cooldown_until = Some(now + Duration::from_secs(6));
            }
        } else if self.current < self.maximum
            && self.cooldown_until.is_none_or(|until| now >= until)
        {
            self.probe = Some((self.current, speed_bps));
            self.current += 1;
            self.cooldown_until = None;
        }
        self.current
    }
}

pub fn download_segmented<P, C>(
    url: &str,
    destination: &Path,
    segment_count: usize,
    progress: P,
    cancelled: C,
) -> Result<SegmentedOutcome, String>
where
    P: Fn(u64, u64) + Send + Sync,
    C: Fn() -> bool + Send + Sync,
{
    download_segmented_with_progress(
        url,
        destination,
        segment_count,
        progress,
        cancelled,
        |_| {},
        |_, _, _, _| {},
    )
}

pub fn download_segmented_with_progress<P, C, M, S>(
    url: &str,
    destination: &Path,
    segment_count: usize,
    progress: P,
    cancelled: C,
    mode: M,
    segment_progress: S,
) -> Result<SegmentedOutcome, String>
where
    P: Fn(u64, u64) + Send + Sync,
    C: Fn() -> bool + Send + Sync,
    M: Fn(usize) + Send + Sync,
    S: Fn(usize, u64, u64, bool) + Send + Sync,
{
    let requested_segment_count = segment_count;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(24 * 60 * 60))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("RDM/0.1")
        .build()
        .map_err(|error| error.to_string())?;
    let probe = send_range_request(&client, url, 0, 0, None)?;
    if probe.status() != reqwest::StatusCode::PARTIAL_CONTENT
        || !matches!(parse_content_range(&probe), Some((0, 0, total)) if total >= 4 * 1024 * 1024)
    {
        return Ok(SegmentedOutcome::Unsupported);
    }
    let total = parse_total(&probe);
    let segment_count = if requested_segment_count == 0 {
        choose_segment_count(total)
    } else {
        requested_segment_count.clamp(2, MAX_SEGMENT_COUNT)
    };
    let validators = Validators {
        etag: header_text(&probe, ETAG),
        last_modified: header_text(&probe, LAST_MODIFIED),
    };
    if validators.etag.is_none() && validators.last_modified.is_none() {
        return Ok(SegmentedOutcome::Unsupported);
    }
    drop(probe);

    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temp_dir = temporary_directory_path(destination, url)?;
    let part_path = part_path(&temp_dir);
    let manifest = segmented_manifest_path(&part_path);
    let expected_manifest = format!(
        "rdm-segmented-v2\n{url}\n{total}\n{segment_count}\n{}\n{}\n",
        validators.etag.as_deref().unwrap_or_default(),
        validators.last_modified.as_deref().unwrap_or_default()
    );
    let reset_segments = match fs::read_to_string(&manifest) {
        Ok(existing) => existing != expected_manifest,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.to_string()),
    };
    if reset_segments {
        for index in 0..MAX_STORED_SEGMENT_FILES {
            let _ = fs::remove_file(segment_path(&part_path, index));
        }
        fs::write(&manifest, expected_manifest).map_err(|error| error.to_string())?;
    }

    let ranges = Arc::new(
        (0..segment_count)
            .map(|index| {
                let start = (u128::from(total) * index as u128 / segment_count as u128) as u64;
                let next = (u128::from(total) * (index + 1) as u128 / segment_count as u128) as u64;
                (start, next - 1, segment_path(&part_path, index))
            })
            .collect::<Vec<_>>(),
    );
    let mut initial_downloaded = 0u64;
    for (start, end, path) in ranges.iter() {
        let length = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if length > end - start + 1 {
            return Err("A temporary segment exceeds its expected size.".to_owned());
        }
        initial_downloaded = initial_downloaded.saturating_add(length);
    }
    let downloaded = Arc::new(AtomicU64::new(initial_downloaded));
    progress(initial_downloaded, total);
    mode(segment_count);
    let progress = Arc::new(progress);
    let segment_progress = Arc::new(segment_progress);
    let cancelled = Arc::new(cancelled);
    let worker_result = thread::scope(|scope| {
        let next_segment = Arc::new(AtomicUsize::new(0));
        let connection_limit = Arc::new(AtomicUsize::new(choose_initial_connection_count(
            choose_connection_count(segment_count),
        )));
        let failed = Arc::new(AtomicBool::new(false));
        let was_cancelled = Arc::new(AtomicBool::new(false));
        let worker_error = Arc::new(std::sync::Mutex::new(None));
        let worker_count = choose_connection_count(segment_count);
        let stop_tuner = Arc::new(AtomicBool::new(false));
        let tuner_downloaded = Arc::clone(&downloaded);
        let tuner_limit = Arc::clone(&connection_limit);
        let tuner_stop = Arc::clone(&stop_tuner);
        let tuner_failed = Arc::clone(&failed);
        let tuner_cancelled = Arc::clone(&cancelled);
        let tuner_handle = scope.spawn(move || {
            let mut tuner = ConcurrencyTuner::new(worker_count);
            let mut sampled_bytes = tuner_downloaded.load(Ordering::Relaxed);
            let mut sampled_at = std::time::Instant::now();
            loop {
                thread::sleep(Duration::from_secs(2));
                if tuner_stop.load(Ordering::Relaxed)
                    || tuner_failed.load(Ordering::Relaxed)
                    || tuner_cancelled()
                {
                    break;
                }
                let now = std::time::Instant::now();
                let current_bytes = tuner_downloaded.load(Ordering::Relaxed);
                let elapsed = now.duration_since(sampled_at).as_secs_f64();
                let speed_bps = if elapsed > 0.0 {
                    ((current_bytes.saturating_sub(sampled_bytes)) as f64 / elapsed) as u64
                } else {
                    0
                };
                sampled_bytes = current_bytes;
                sampled_at = now;
                tuner_limit.store(tuner.observe(speed_bps, now), Ordering::Relaxed);
            }
        });
        let mut handles = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let client = client.clone();
            let url = url.to_owned();
            let validators = Validators {
                etag: validators.etag.clone(),
                last_modified: validators.last_modified.clone(),
            };
            let downloaded = Arc::clone(&downloaded);
            let progress = Arc::clone(&progress);
            let segment_progress = Arc::clone(&segment_progress);
            let cancelled = Arc::clone(&cancelled);
            let next_segment = Arc::clone(&next_segment);
            let failed = Arc::clone(&failed);
            let was_cancelled = Arc::clone(&was_cancelled);
            let worker_error = Arc::clone(&worker_error);
            let ranges = Arc::clone(&ranges);
            let connection_limit = Arc::clone(&connection_limit);
            handles.push(scope.spawn(move || loop {
                if failed.load(Ordering::Relaxed) || cancelled() {
                    break;
                }
                if worker_index >= connection_limit.load(Ordering::Relaxed) {
                    if next_segment.load(Ordering::Relaxed) >= ranges.len() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let index = next_segment.fetch_add(1, Ordering::Relaxed);
                let Some((start, end, path)) = ranges.get(index) else {
                    break;
                };
                let context = SegmentDownloadContext {
                    client: &client,
                    url: &url,
                    total,
                    validators: &validators,
                    downloaded: &downloaded,
                    progress: &*progress,
                    cancelled: &*cancelled,
                };
                match download_segment(&context, index, *start, *end, path, &*segment_progress) {
                    Ok(()) => {}
                    Err(SegmentError::Cancelled) => {
                        was_cancelled.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(SegmentError::Failed(message)) => {
                        failed.store(true, Ordering::Relaxed);
                        *worker_error.lock().expect("worker error lock poisoned") = Some(message);
                        break;
                    }
                }
            }));
        }
        let mut worker_panicked = false;
        for handle in handles {
            worker_panicked |= handle.join().is_err();
        }
        stop_tuner.store(true, Ordering::Relaxed);
        let tuner_panicked = tuner_handle.join().is_err();
        if worker_panicked || tuner_panicked {
            return Err("A download segment worker failed.".to_owned());
        }
        let worker_error = worker_error
            .lock()
            .expect("worker error lock poisoned")
            .take();
        if let Some(error) = worker_error {
            Err(error)
        } else {
            Ok(!was_cancelled.load(Ordering::Relaxed) && !cancelled())
        }
    })?;
    if !worker_result {
        return Ok(SegmentedOutcome::Cancelled);
    }

    let mut output = File::create(&part_path).map_err(|error| error.to_string())?;
    let mut merged = 0u64;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    segment_progress(segment_count, 0, total, true);
    for (start, end, path) in ranges.iter() {
        let expected = end - start + 1;
        if fs::metadata(path).map_err(|error| error.to_string())?.len() != expected {
            return Err("A downloaded segment has an unexpected size.".to_owned());
        }
        let mut input = File::open(path).map_err(|error| error.to_string())?;
        loop {
            if cancelled() {
                segment_progress(segment_count, merged, total, false);
                return Ok(SegmentedOutcome::Cancelled);
            }
            let read = input.read(&mut buffer).map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|error| error.to_string())?;
            merged = merged.saturating_add(read as u64);
            progress(merged, total);
            segment_progress(segment_count, merged, total, true);
        }
    }
    segment_progress(segment_count, total, total, false);
    output.sync_all().map_err(|error| error.to_string())?;
    drop(output);
    replace_download_file(&part_path, destination).map_err(|error| error.to_string())?;
    fs::remove_dir_all(&temp_dir).map_err(|error| error.to_string())?;
    progress(total, total);
    Ok(SegmentedOutcome::Complete)
}

enum SegmentError {
    Cancelled,
    Failed(String),
}

struct SegmentDownloadContext<'a, P, C> {
    client: &'a Client,
    url: &'a str,
    total: u64,
    validators: &'a Validators,
    downloaded: &'a AtomicU64,
    progress: &'a P,
    cancelled: &'a C,
}

type SegmentProgressCallback<'a> = dyn Fn(usize, u64, u64, bool) + Send + Sync + 'a;

fn segmented_manifest_path(part_path: &Path) -> PathBuf {
    let mut name = part_path.as_os_str().to_os_string();
    name.push(".segments.meta");
    PathBuf::from(name)
}

fn segment_path(part_path: &Path, index: usize) -> PathBuf {
    let mut name = part_path.as_os_str().to_os_string();
    name.push(format!(".segment-{index}"));
    PathBuf::from(name)
}

fn download_segment<P, C>(
    context: &SegmentDownloadContext<'_, P, C>,
    index: usize,
    start: u64,
    end: u64,
    path: &Path,
    segment_progress: &SegmentProgressCallback<'_>,
) -> Result<(), SegmentError>
where
    P: Fn(u64, u64),
    C: Fn() -> bool,
{
    let length = end - start + 1;
    let offset = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if offset == length {
        segment_progress(index, offset, length, false);
        return Ok(());
    }

    segment_progress(index, offset, length, true);
    let result = download_segment_inner(context, start, end, path, index, segment_progress);
    let downloaded = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(offset);
    segment_progress(index, downloaded, length, false);
    result
}

fn download_segment_inner<P, C>(
    context: &SegmentDownloadContext<'_, P, C>,
    start: u64,
    end: u64,
    path: &Path,
    index: usize,
    segment_progress: &SegmentProgressCallback<'_>,
) -> Result<(), SegmentError>
where
    P: Fn(u64, u64),
    C: Fn() -> bool,
{
    let mut offset = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let length = end - start + 1;
    if offset == length {
        return Ok(());
    }
    let range_start = start + offset;
    let mut response = send_range_request(
        context.client,
        context.url,
        range_start,
        end,
        Some(context.validators),
    )
    .map_err(SegmentError::Failed)?;
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT
        || parse_content_range(&response) != Some((range_start, end, context.total))
        || context
            .validators
            .etag
            .as_ref()
            .is_some_and(|expected| header_text(&response, ETAG).as_ref() != Some(expected))
        || context
            .validators
            .last_modified
            .as_ref()
            .is_some_and(|expected| {
                header_text(&response, LAST_MODIFIED).as_ref() != Some(expected)
            })
    {
        return Err(SegmentError::Failed(
            "The server did not provide the same resource for all segments.".to_owned(),
        ));
    }
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(path)
        .map_err(|error| SegmentError::Failed(error.to_string()))?;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    while offset < length {
        if (context.cancelled)() {
            output
                .flush()
                .and_then(|_| output.sync_all())
                .map_err(|error| SegmentError::Failed(error.to_string()))?;
            return Err(SegmentError::Cancelled);
        }
        let count = response
            .read(&mut buffer[..(length - offset).min(BUFFER_SIZE as u64) as usize])
            .map_err(|error| SegmentError::Failed(error.to_string()))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| SegmentError::Failed(error.to_string()))?;
        offset += count as u64;
        let current = context
            .downloaded
            .fetch_add(count as u64, Ordering::Relaxed)
            + count as u64;
        (context.progress)(current, context.total);
        segment_progress(index, offset, length, true);
    }
    output
        .flush()
        .and_then(|_| output.sync_all())
        .map_err(|error| SegmentError::Failed(error.to_string()))?;
    if offset != length {
        return Err(SegmentError::Failed(
            "The server interrupted a segment before it completed.".to_owned(),
        ));
    }
    Ok(())
}

fn send_range_request(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    validators: Option<&Validators>,
) -> Result<Response, String> {
    let mut current = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    let mut retried_cookie = false;
    for redirects in 0..=10 {
        let mut request = client
            .get(current.clone())
            .header(RANGE, format!("bytes={start}-{end}"));
        if let Some(validators) = validators {
            if let Some(etag) = validators.etag.as_deref() {
                request = request.header(IF_RANGE, etag);
            } else if let Some(modified) = validators.last_modified.as_deref() {
                request = request.header(IF_RANGE, modified);
            }
        }
        let response = request.send().map_err(|error| error.to_string())?;
        if !retried_cookie
            && response.status().is_success()
            && response.status() != reqwest::StatusCode::PARTIAL_CONTENT
            && response.headers().contains_key(SET_COOKIE)
        {
            retried_cookie = true;
            drop(response);
            continue;
        }
        if !response.status().is_redirection() {
            return Ok(response);
        }
        if redirects == 10 {
            return Err("The server redirected too many times.".to_owned());
        }
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                "The server redirected without providing a destination URL.".to_owned()
            })?;
        let next = current.join(location).map_err(|error| error.to_string())?;
        if next.scheme() != "http" && next.scheme() != "https" {
            return Err("The server redirected to an unsupported protocol.".to_owned());
        }
        current = next;
    }
    unreachable!("redirect loop always returns or errors after ten redirects")
}

#[no_mangle]
/// Downloads an HTTP(S) URL to a destination, optionally reporting progress or cancellation.
///
/// # Safety
/// Non-null pointers must reference valid NUL-terminated UTF-8 strings and a writable error
/// buffer of `error_buffer_len` bytes. Callback context must remain valid for the duration.
pub unsafe extern "C" fn rdm_download(
    url: *const c_char,
    destination: *const c_char,
    progress: ProgressCallback,
    cancel: CancelCallback,
    context: *mut std::ffi::c_void,
    error_buffer: *mut c_char,
    error_buffer_len: usize,
) -> i32 {
    if url.is_null() || destination.is_null() {
        write_error(
            "Missing URL or destination path",
            error_buffer,
            error_buffer_len,
        );
        return RESULT_ERROR;
    }
    let url = match CStr::from_ptr(url).to_str() {
        Ok(value) => value,
        Err(_) => {
            write_error("The URL is not valid UTF-8", error_buffer, error_buffer_len);
            return RESULT_ERROR;
        }
    };
    let destination = match CStr::from_ptr(destination).to_str() {
        Ok(value) => Path::new(value),
        Err(_) => {
            write_error(
                "The destination path is not valid UTF-8",
                error_buffer,
                error_buffer_len,
            );
            return RESULT_ERROR;
        }
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        write_error(
            "Only HTTP and HTTPS URLs are supported",
            error_buffer,
            error_buffer_len,
        );
        return RESULT_ERROR;
    }

    match download(url, destination, progress, cancel, context) {
        Ok(result) => result,
        Err(error) => {
            write_error(&error, error_buffer, error_buffer_len);
            RESULT_ERROR
        }
    }
}

unsafe fn write_error(message: &str, buffer: *mut c_char, capacity: usize) {
    if buffer.is_null() || capacity == 0 {
        return;
    }
    let bytes = message.as_bytes();
    let count = bytes.len().min(capacity - 1);
    ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast(), count);
    *buffer.add(count) = 0;
}

#[no_mangle]
pub extern "C" fn rdm_engine_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn chooses_work_unit_and_connection_counts_for_file_size() {
        let file_size = 100 * 1024 * 1024;
        assert_eq!(choose_segment_count(file_size), 20);
        assert_eq!(choose_segment_count(file_size * 2), 20);
        assert_eq!(choose_segment_count(101), 20);
        assert_eq!(choose_segment_count(19), 19);
        assert_eq!(choose_segment_count(1), 2);
        assert_eq!(choose_segment_count(u64::MAX), MAX_SEGMENT_COUNT);
        assert_eq!(MAX_SEGMENT_COUNT, 20);
        assert_eq!(choose_connection_count(4), 4);
        assert_eq!(choose_connection_count(64), 16);
        assert_eq!(MAX_CONNECTION_COUNT, 16);
        assert_eq!(choose_initial_connection_count(1), 1);
        assert_eq!(choose_initial_connection_count(16), 4);
    }

    #[test]
    fn concurrency_tuner_keeps_only_speed_improving_connections() {
        let start = std::time::Instant::now();
        let mut tuner = ConcurrencyTuner::new(16);

        assert_eq!(tuner.observe(100, start), 5);
        assert_eq!(tuner.observe(106, start + Duration::from_secs(2)), 6);
        assert_eq!(tuner.observe(109, start + Duration::from_secs(4)), 5);
        assert_eq!(tuner.observe(100, start + Duration::from_secs(8)), 5);
        assert_eq!(tuner.observe(110, start + Duration::from_secs(11)), 6);
    }

    #[test]
    fn temp_directories_are_url_scoped_and_reused_for_resume() {
        let destination = temp_destination("temp-folder");
        let url = "https://example.test/a?token=sample";
        let first = temporary_directory_path(&destination, url).unwrap();
        let second = temporary_directory_path(&destination, url).unwrap();
        let other = temporary_directory_path(&destination, "https://example.test/b").unwrap();
        let other_destination =
            temporary_directory_path(&destination.with_extension("other"), url).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_ne!(first, other_destination);
        assert!(first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&temporary_directory_prefix(url)));

        discard_partial_download(&destination, url).unwrap();
        assert!(!first.exists());
        assert!(other.exists());
        assert!(other_destination.exists());
        discard_partial_download(&destination, "https://example.test/b").unwrap();
        discard_partial_download(&destination.with_extension("other"), url).unwrap();
    }
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    fn serve_once(
        response: &'static str,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                request.push_str(&line);
            }
            sender.send(request).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}/file"), receiver, handle)
    }

    #[test]
    fn reuses_pooled_connection_for_sequential_ranges() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut requests = Vec::new();
            for index in 0..2 {
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    request.push_str(&line);
                }
                requests.push(request);
                write!(
                    stream,
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {index}-{index}/2\r\nContent-Length: 1\r\nETag: \"stable\"\r\nConnection: keep-alive\r\n\r\n{}",
                    if index == 0 { "a" } else { "b" }
                )
                .unwrap();
                stream.flush().unwrap();
            }
            sender.send(requests).unwrap();
        });

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = format!("http://{address}/file");
        let validators = Validators {
            etag: Some("\"stable\"".to_owned()),
            last_modified: None,
        };
        for index in 0..2 {
            let mut response = send_range_request(
                &client,
                &url,
                index,
                index,
                (index > 0).then_some(&validators),
            )
            .unwrap();
            let mut byte = [0];
            response.read_exact(&mut byte).unwrap();
            assert_eq!(byte[0], b'a' + index as u8);
        }

        server.join().unwrap();
        let requests = receiver.recv().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0]
            .to_ascii_lowercase()
            .contains("range: bytes=0-0"));
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("range: bytes=1-1"));
    }

    fn temp_destination(test_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rdm-{test_name}-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn prepare_partial(destination: &Path, url: &str, bytes: &[u8]) -> PathBuf {
        let temp_dir = temporary_directory_path(destination, url).unwrap();
        let part = part_path(&temp_dir);
        fs::write(&part, bytes).unwrap();
        save_validators(
            &part,
            &Validators {
                etag: Some("\"v1\"".to_owned()),
                last_modified: None,
            },
        )
        .unwrap();
        part
    }

    unsafe extern "C" fn cancel_after_read(context: *mut std::ffi::c_void) -> bool {
        let checks = unsafe { &*(context.cast::<std::sync::atomic::AtomicUsize>()) };
        checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0
    }

    #[test]
    fn resumes_with_range_and_validator() {
        let destination = temp_destination("resume");
        let (url, request, server) = serve_once(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 6-10/11\r\nContent-Length: 5\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nworld",
        );
        let _part = prepare_partial(&destination, &url, b"hello ");

        assert_eq!(
            download(&url, &destination, None, None, ptr::null_mut()).unwrap(),
            RESULT_OK
        );
        server.join().unwrap();
        let request = request.recv().unwrap();
        let request = request.to_ascii_lowercase();
        assert!(request.contains("range: bytes=6-"));
        assert!(request.contains("if-range: \"v1\""));
        assert_eq!(fs::read(&destination).unwrap(), b"hello world");
        assert!(find_temporary_directory(&destination, &url)
            .unwrap()
            .is_none());
        let _ = fs::remove_file(&destination);
    }

    #[test]
    fn probes_link_metadata_without_downloading_the_file() {
        let (url, request, server) = serve_once(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1234\r\nContent-Length: 1\r\nContent-Type: application/zip\r\nContent-Disposition: attachment; filename=\"sample.zip\"\r\nETag: \"stable\"\r\nConnection: close\r\n\r\nx",
        );

        let info = probe_download(&url).unwrap();
        server.join().unwrap();
        let request = request.recv().unwrap().to_ascii_lowercase();

        assert!(request.contains("range: bytes=0-0"));
        assert_eq!(info.status, 206);
        assert_eq!(info.filename, "sample.zip");
        assert_eq!(info.server_filename.as_deref(), Some("sample.zip"));
        assert_eq!(info.content_type.as_deref(), Some("application/zip"));
        assert_eq!(info.size, Some(1234));
        assert!(info.supports_ranges);
        assert!(info.has_validator);
    }

    #[test]
    fn prefers_utf8_extended_content_disposition_filename() {
        assert_eq!(
            filename_from_content_disposition(
                "attachment; filename=\"fallback.zip\"; filename*=UTF-8''t%E1%BA%ADp%20tin.zip"
            )
            .as_deref(),
            Some("tập tin.zip")
        );
    }

    #[test]
    fn restarts_when_server_ignores_range() {
        let destination = temp_destination("restart");
        let (url, request, server) = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nETag: \"v2\"\r\nConnection: close\r\n\r\nfresh",
        );
        let _part = prepare_partial(&destination, &url, b"stale data");

        assert_eq!(
            download(&url, &destination, None, None, ptr::null_mut()).unwrap(),
            RESULT_OK
        );
        server.join().unwrap();
        assert!(request
            .recv()
            .unwrap()
            .to_ascii_lowercase()
            .contains("range: bytes="));
        assert_eq!(fs::read(&destination).unwrap(), b"fresh");
        assert!(find_temporary_directory(&destination, &url)
            .unwrap()
            .is_none());
        let _ = fs::remove_file(&destination);
    }

    #[test]
    fn cancel_preserves_partial_file_for_resume() {
        let destination = temp_destination("pause");
        let (url, request, server) = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"v2\"\r\nConnection: close\r\n\r\nfresh data",
        );
        let part = prepare_partial(&destination, &url, b"partial");
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let cancel: CancelCallback = Some(cancel_after_read);

        assert_eq!(
            download(
                &url,
                &destination,
                None,
                cancel,
                (&checks as *const std::sync::atomic::AtomicUsize)
                    .cast_mut()
                    .cast::<std::ffi::c_void>(),
            )
            .unwrap(),
            RESULT_INTERRUPTED
        );
        server.join().unwrap();
        let _ = request.recv().unwrap();
        assert_eq!(fs::read(&part).unwrap(), b"fresh data");
        assert!(!destination.exists());
        discard_partial_download(&destination, &url).unwrap();
    }

    #[test]
    fn downloads_parallel_ranges_and_assembles_file() {
        let destination = temp_destination("segmented");
        let body = (0..4 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let expected = body.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..21 {
                let (mut stream, _) = listener.accept().unwrap();
                let body = body.clone();
                workers.push(thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request = String::new();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }

                        request.push_str(&line);
                    }
                    let range = request
                        .lines()
                        .find(|line| line.to_ascii_lowercase().starts_with("range:"))
                        .unwrap()
                        .split_once(':')
                        .unwrap()
                        .1
                        .trim();
                    let (start, end) = range
                        .strip_prefix("bytes=")
                        .unwrap()
                        .split_once('-')
                        .unwrap();
                    let start = start.parse::<usize>().unwrap();
                    let end = end.parse::<usize>().unwrap();
                    let selected = &body[start..=end];
                    write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nETag: \"stable\"\r\nConnection: close\r\n\r\n",
                        body.len(),
                        selected.len()
                    )
                    .unwrap();
                    stream.write_all(selected).unwrap();
                    stream.flush().unwrap();
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });

        let reported_lanes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let completed_lanes = Arc::new(AtomicU64::new(0));
        let merged_bytes = Arc::new(AtomicU64::new(0));
        let reported_lanes_callback = Arc::clone(&reported_lanes);
        let completed_lanes_callback = Arc::clone(&completed_lanes);
        let merged_bytes_callback = Arc::clone(&merged_bytes);
        assert!(matches!(
            download_segmented_with_progress(
                &format!("http://{address}/file"),
                &destination,
                20,
                |_, _| {},
                || false,
                move |count| {
                    reported_lanes_callback.store(count, Ordering::Relaxed);
                },
                move |index, downloaded, total, active| {
                    if index < 20 && !active && downloaded == total {
                        completed_lanes_callback.fetch_or(1 << index, Ordering::Relaxed);
                    } else if index == 20 {
                        merged_bytes_callback.store(downloaded, Ordering::Relaxed);
                    }
                },
            )
            .unwrap(),
            SegmentedOutcome::Complete
        ));
        server.join().unwrap();
        assert_eq!(reported_lanes.load(Ordering::Relaxed), 20);
        assert_eq!(completed_lanes.load(Ordering::Relaxed), (1 << 20) - 1);
        assert_eq!(merged_bytes.load(Ordering::Relaxed), expected.len() as u64);
        assert_eq!(fs::read(&destination).unwrap(), expected);
        assert!(
            find_temporary_directory(&destination, &format!("http://{address}/file"))
                .unwrap()
                .is_none()
        );
        let _ = fs::remove_file(destination);
    }

    #[test]
    fn discards_partial_files_without_removing_completed_destination() {
        let destination = temp_destination("discard-partials");
        let url = "https://example.test/download";
        fs::write(&destination, b"completed").unwrap();
        let temp_dir = temporary_directory_path(&destination, url).unwrap();
        let part_path = part_path(&temp_dir);
        fs::write(&part_path, b"partial").unwrap();
        fs::write(sidecar_path(&part_path), b"validators").unwrap();
        fs::write(segmented_manifest_path(&part_path), b"manifest").unwrap();
        fs::write(segment_path(&part_path, 0), b"segment").unwrap();

        discard_partial_download(&destination, url).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"completed");
        assert!(!temp_dir.exists());
        let _ = fs::remove_file(destination);
    }

    #[test]
    #[ignore = "downloads the URL supplied through RDM_TEST_URL"]
    fn downloads_remote_test_url_without_opening_archive() {
        let url = std::env::var("RDM_TEST_URL").expect("RDM_TEST_URL must be set");
        let destination = temp_destination("remote-e2e");
        let total = AtomicU64::new(0);
        let outcome = download_segmented(
            &url,
            &destination,
            4,
            |_, size| {
                total.store(size, Ordering::Relaxed);
            },
            || false,
        )
        .expect("remote segmented download should succeed");

        assert!(matches!(outcome, SegmentedOutcome::Complete));
        assert_eq!(
            fs::metadata(&destination).unwrap().len(),
            total.load(Ordering::Relaxed)
        );
        let _ = fs::remove_file(destination);
    }
}
