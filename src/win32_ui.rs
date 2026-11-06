use super::{
    BrowserCopyData, DownloadManager, EntryAction, QueueFilter, BROWSER_DOWNLOAD_COPYDATA_ID,
};
use std::mem::zeroed;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawFocusRect, DrawTextW, FillRect, FrameRect, GetStockObject,
    GetSysColor, GetSysColorBrush, InvalidateRect, RedrawWindow, SetBkColor, SetTextColor,
    UpdateWindow,
    COLOR_BTNFACE, COLOR_BTNSHADOW, COLOR_BTNTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT,
    DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, RDW_ALLCHILDREN, RDW_ERASE,
    RDW_INVALIDATE,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, ICC_DATE_CLASSES, ICC_LISTVIEW_CLASSES,
    ICC_PROGRESS_CLASS, LVCOLUMNW, LVITEMW, NMHDR, DTM_GETSYSTEMTIME, DTM_SETFORMATW,
    DTM_SETSYSTEMTIME, DTS_SHOWNONE, DRAWITEMSTRUCT, GDT_NONE, GDT_VALID, LVCF_FMT, LVCF_TEXT,
    LVCF_WIDTH, LVIF_TEXT, LVIS_SELECTED, LVNI_SELECTED, LVM_DELETEALLITEMS, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, ODT_BUTTON,
    PBM_SETPOS, PBM_SETRANGE32, LVN_ITEMCHANGED,
};
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const MAIN_CLASS: &str = "RdmMainWindow";
const PROGRESS_CLASS: &str = "RdmProgressWindow";
const MAIN_TIMER: usize = 1;
const MAIN_MIN_CLIENT_WIDTH: i32 = 900;
const MAIN_MIN_CLIENT_HEIGHT: i32 = 560;
const PROGRESS_MIN_CLIENT_WIDTH: i32 = 800;
const PROGRESS_MIN_CLIENT_HEIGHT: i32 = 420;

const URL_EDIT: i32 = 101;
const FOLDER_EDIT: i32 = 102;
const SCHEDULE_EDIT: i32 = 103;
const SEARCH_EDIT: i32 = 104;
const MAIN_LIST: i32 = 105;
const LINK_LABEL: i32 = 106;
const STATUS_LABEL: i32 = 107;

const CHECK_LINK: i32 = 201;
const BROWSE_FOLDER: i32 = 202;
const ADD_QUEUE: i32 = 203;
const DOWNLOAD_NOW: i32 = 204;
const RUN_QUEUE: i32 = 205;
const SHOW_PROGRESS: i32 = 209;
const FILTER_MENU: i32 = 210;
const CLEAR_COMPLETED: i32 = 211;
const ENTRY_ACTIONS: i32 = 212;
const THEME_MENU: i32 = 213;

const FILTER_ALL: u32 = 301;
const FILTER_ACTIVE: u32 = 302;
const FILTER_COMPLETED: u32 = 303;
const FILTER_FAILED: u32 = 304;
const OPEN_FOLDER: u32 = 311;
const COPY_URL: u32 = 312;
const RETRY: u32 = 313;
const REMOVE: u32 = 314;
const THEME_SYSTEM: u32 = 321;
const THEME_LIGHT: u32 = 322;
const THEME_DARK: u32 = 323;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    System,
    Light,
    Dark,
}

const PROGRESS_SELECT: i32 = 501;
const PROGRESS_STATUS: i32 = 503;
const PROGRESS_FILE: i32 = 504;
const PROGRESS_DESTINATION: i32 = 505;
const PROGRESS_TOTAL: i32 = 506;
const PROGRESS_OVERALL: i32 = 507;
const PROGRESS_PAUSE: i32 = 508;
const PROGRESS_CANCEL: i32 = 509;
const PROGRESS_RESUME: i32 = 510;
const PROGRESS_COPY: i32 = 511;
const PROGRESS_OPEN: i32 = 512;
const SEGMENT_BAR: i32 = 516;
const SEGMENT_STATUS: i32 = 517;
const PROGRESS_ACCEPT: i32 = 518;
const PROGRESS_REJECT: i32 = 519;
const PROGRESS_ELAPSED_LABEL: i32 = 520;
const PROGRESS_ELAPSED_VALUE: i32 = 521;
const LVM_SETBKCOLOR_MESSAGE: u32 = 0x1001;
const LVM_SETTEXTCOLOR_MESSAGE: u32 = 0x1024;
const LVM_SETTEXTBKCOLOR_MESSAGE: u32 = 0x1026;
const WM_SETREDRAW_MESSAGE: u32 = 0x000B;
const TB_RESUME: i32 = 220;
const TB_STOP: i32 = 221;
const TB_DELETE: i32 = 222;
const SS_OWNERDRAW: i32 = 0x000D;
const SS_ENDELLIPSIS: i32 = 0x00004000;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_SETCURSEL: u32 = 0x014E;
const CBN_SELCHANGE: u16 = 1;

struct UiState {
    manager: DownloadManager,
    instance: isize,
    main_window: HWND,
    progress_window: HWND,
    progress_visible: bool,
    progress_only: bool,
    main_visible_ids: Vec<u64>,
    main_rendered_rows: Vec<[String; 3]>,
    progress_visible_ids: Vec<u64>,
    progress_rendered_items: Vec<String>,
    rendered_progress_total: u64,
    selected_progress_id: u64,
    theme: ThemeChoice,
    dark_brush: windows_sys::Win32::Graphics::Gdi::HBRUSH,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn settings_path() -> Result<PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "LOCALAPPDATA was not found.".to_owned())?;
    Ok(local_app_data.join("RDM").join("settings.json"))
}

fn read_settings() -> Result<serde_json::Value, String> {
    let path = settings_path()?;
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({}))
        }
        Err(error) => return Err(format!("Could not read settings: {error}")),
    };
    serde_json::from_slice(&data).map_err(|error| format!("Settings file is invalid: {error}"))
}

fn load_theme(settings: &serde_json::Value) -> Result<ThemeChoice, String> {
    match settings.get("theme").and_then(serde_json::Value::as_str) {
        None | Some("system") => Ok(ThemeChoice::System),
        Some("light") => Ok(ThemeChoice::Light),
        Some("dark") => Ok(ThemeChoice::Dark),
        Some(value) => Err(format!("Unsupported theme setting: {value}")),
    }
}

fn save_settings(settings: &mut serde_json::Value) -> Result<(), String> {
    let path = settings_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| "The settings path is invalid.".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create the settings directory: {error}"))?;
    let data = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("Could not serialize theme settings: {error}"))?;
    let temp_path = path.with_extension("json.tmp");
    std::fs::write(&temp_path, data)
        .map_err(|error| format!("Could not save theme settings: {error}"))?;
    use std::os::windows::ffi::OsStrExt;
    let source: Vec<u16> = temp_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let succeeded = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        return Err(format!(
            "Could not update theme settings (Windows error {}).",
            unsafe { windows_sys::Win32::Foundation::GetLastError() }
        ));
    }
    Ok(())
}

fn save_theme(theme: ThemeChoice) -> Result<(), String> {
    let mut settings = read_settings()?;
    settings["theme"] = serde_json::Value::String(
        match theme {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
        .to_owned(),
    );
    save_settings(&mut settings)
}

fn handle_from_id(id: i32) -> windows_sys::Win32::UI::WindowsAndMessaging::HMENU {
    id as usize as windows_sys::Win32::UI::WindowsAndMessaging::HMENU
}

unsafe fn create_control(
    parent: HWND,
    class_name: &str,
    text: &str,
    style: u32,
    id: i32,
    instance: isize,
) -> HWND {
    let class_name = wide(class_name);
    let text = wide(text);
    let handle = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            text.as_ptr(),
            style,
            0,
            0,
            100,
            24,
            parent,
            handle_from_id(id),
            instance as _,
            null(),
        )
    };
    if !handle.is_null() {
        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        unsafe {
            SendMessageW(handle, WM_SETFONT, font as usize, 1);
        }
    }
    handle
}

unsafe fn create_button(parent: HWND, text: &str, id: i32, instance: isize) {
    unsafe {
        create_control(
            parent,
            "BUTTON",
            text,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
            id,
            instance,
        );
    }
}

unsafe fn create_static(parent: HWND, text: &str, id: i32, instance: isize) {
    unsafe {
        create_control(
            parent,
            "STATIC",
            text,
            WS_CHILD | WS_VISIBLE,
            id,
            instance,
        );
    }
}

unsafe fn create_ellipsis_static(parent: HWND, text: &str, id: i32, instance: isize) {
    unsafe {
        create_control(
            parent,
            "STATIC",
            text,
            WS_CHILD | WS_VISIBLE | SS_ENDELLIPSIS as u32,
            id,
            instance,
        );
    }
}

unsafe fn create_main_controls(state: &mut UiState, hwnd: HWND) {
    let instance = state.instance;
    unsafe {
        create_static(hwnd, "URL", 100, instance);
        create_control(
            hwnd,
            "EDIT",
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            URL_EDIT,
            instance,
        );
        create_button(hwnd, "Check link", CHECK_LINK, instance);
        create_static(hwnd, "", LINK_LABEL, instance);

        create_static(hwnd, "Save to", 108, instance);
        create_control(
            hwnd,
            "EDIT",
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            FOLDER_EDIT,
            instance,
        );
        create_button(hwnd, "Browse...", BROWSE_FOLDER, instance);

        create_static(hwnd, "Schedule:", 109, instance);
        create_control(
            hwnd,
            "SysDateTimePick32",
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | DTS_SHOWNONE,
            SCHEDULE_EDIT,
            instance,
        );
        create_button(hwnd, "Add URL", ADD_QUEUE, instance);
        create_button(hwnd, "Download now", DOWNLOAD_NOW, instance);
        create_button(hwnd, "Resume", TB_RESUME, instance);
        create_button(hwnd, "Stop", TB_STOP, instance);
        create_button(hwnd, "Start queue", RUN_QUEUE, instance);
        create_button(hwnd, "Delete", TB_DELETE, instance);
        create_button(hwnd, "Progress", SHOW_PROGRESS, instance);

        create_static(hwnd, "Downloads", 110, instance);
        create_control(
            hwnd,
            "EDIT",
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            SEARCH_EDIT,
            instance,
        );
        create_button(hwnd, "Filter: All", FILTER_MENU, instance);
        create_button(hwnd, "Clear completed", CLEAR_COMPLETED, instance);
        create_button(hwnd, "Theme: System", THEME_MENU, instance);
        let list = create_control(
            hwnd,
            "SysListView32",
            "",
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_BORDER
                | WS_VSCROLL
                | LVS_REPORT
                | LVS_SHOWSELALWAYS
                | LVS_SINGLESEL,
            MAIN_LIST,
            instance,
        );
        initialize_download_columns(list);
        create_button(hwnd, "Selected item actions...", ENTRY_ACTIONS, instance);
        create_static(hwnd, "", STATUS_LABEL, instance);
    }
}

unsafe fn initialize_download_columns(list: HWND) {
    if list.is_null() {
        return;
    }
    for (index, (heading, width)) in [
        ("File", 360),
        ("Status", 180),
        ("Downloaded", 180),
    ]
    .into_iter()
    .enumerate()
    {
        let mut heading = wide(heading);
        let mut column: LVCOLUMNW = unsafe { zeroed() };
        column.mask = LVCF_FMT | LVCF_WIDTH | LVCF_TEXT;
        column.fmt = 0;
        column.cx = width;
        column.pszText = heading.as_mut_ptr();
        column.iSubItem = index as i32;
        unsafe {
            SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                index,
                (&mut column as *mut LVCOLUMNW) as LPARAM,
            );
        }
    }
}

unsafe fn create_progress_controls(state: &mut UiState, hwnd: HWND) {
    let instance = state.instance;
    unsafe {
        create_static(hwnd, "Downloads", 500, instance);
        create_control(
            hwnd,
            "COMBOBOX",
            "",
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_VSCROLL
                | WS_BORDER
                | CBS_DROPDOWNLIST as u32,
            PROGRESS_SELECT,
            instance,
        );
        create_static(hwnd, "", PROGRESS_STATUS, instance);
        create_control(
            hwnd,
            "msctls_progress32",
            "",
            WS_CHILD | WS_VISIBLE,
            PROGRESS_OVERALL,
            instance,
        );
        create_static(hwnd, "", PROGRESS_TOTAL, instance);
        create_static(hwnd, "File:", 513, instance);
        create_ellipsis_static(hwnd, "", PROGRESS_FILE, instance);
        create_static(hwnd, "Save to:", 514, instance);
        create_ellipsis_static(hwnd, "", PROGRESS_DESTINATION, instance);
        create_static(hwnd, "Elapsed:", PROGRESS_ELAPSED_LABEL, instance);
        create_static(hwnd, "", PROGRESS_ELAPSED_VALUE, instance);
        create_static(hwnd, "Download activity", SEGMENT_STATUS, instance);
        create_control(
            hwnd,
            "STATIC",
            "",
            WS_CHILD | WS_VISIBLE | SS_OWNERDRAW as u32,
            SEGMENT_BAR,
            instance,
        );
        create_button(hwnd, "Pause", PROGRESS_PAUSE, instance);
        create_button(hwnd, "Cancel", PROGRESS_CANCEL, instance);
        create_button(hwnd, "Resume", PROGRESS_RESUME, instance);
        create_button(hwnd, "Copy URL", PROGRESS_COPY, instance);
        create_button(hwnd, "Open folder", PROGRESS_OPEN, instance);
        create_button(hwnd, "Download with RDM", PROGRESS_ACCEPT, instance);
        create_button(hwnd, "Keep in browser", PROGRESS_REJECT, instance);
    }
}

unsafe fn child(parent: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(parent, id) }
}

unsafe fn place(parent: HWND, id: i32, x: i32, y: i32, width: i32, height: i32) {
    let handle = unsafe { child(parent, id) };
    if !handle.is_null() {
        unsafe {
            MoveWindow(handle, x, y, width.max(0), height.max(0), 1);
        }
    }
}

unsafe fn layout_main(hwnd: HWND) {
    let mut rect: RECT = unsafe { zeroed() };
    unsafe {
        GetClientRect(hwnd, &mut rect);
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let margin = 12;
    let gap = 8;
    let full = width - margin * 2;
    let edit_width = (full - 150 - gap).max(200);
    unsafe {
        place(hwnd, 100, margin, 10, 40, 24);
        place(hwnd, URL_EDIT, margin + 44, 8, edit_width - 44, 28);
        place(hwnd, CHECK_LINK, margin + edit_width + gap, 8, 150, 28);
        place(hwnd, LINK_LABEL, margin, 38, full, 22);
        place(hwnd, 108, margin, 64, 54, 24);
        place(hwnd, FOLDER_EDIT, margin + 58, 62, full - 140, 28);
        place(hwnd, BROWSE_FOLDER, margin + full - 74, 62, 74, 28);
        place(hwnd, 109, margin, 98, 70, 24);
        place(hwnd, SCHEDULE_EDIT, margin + 76, 96, 300, 28);
        let mut x = margin;
        let buttons = [
            (ADD_QUEUE, 90),
            (DOWNLOAD_NOW, 108),
            (TB_RESUME, 78),
            (TB_STOP, 66),
            (RUN_QUEUE, 104),
            (TB_DELETE, 72),
            (SHOW_PROGRESS, 86),
        ];
        for (id, button_width) in buttons {
            place(hwnd, id, x, 136, button_width, 29);
            x += button_width + 6;
        }
        place(hwnd, 110, margin, 176, 88, 24);
        place(hwnd, SEARCH_EDIT, margin + 92, 173, 220, 28);
        place(hwnd, FILTER_MENU, margin + 320, 173, 138, 28);
        place(hwnd, CLEAR_COMPLETED, margin + full - 320, 173, 156, 28);
        place(hwnd, THEME_MENU, margin + full - 156, 173, 156, 28);
        let list_y = 208;
        let button_y = (height - 44).max(list_y + 80);
        place(
            hwnd,
            MAIN_LIST,
            margin,
            list_y,
            full,
            (button_y - list_y - 8).max(60),
        );
        let list = child(hwnd, MAIN_LIST);
        if !list.is_null() {
            let file_width = (full * 48 / 100).max(240);
            let status_width = (full * 27 / 100).max(150);
            let downloaded_width = (full - file_width - status_width).max(180);
            for (column, column_width) in [file_width, status_width, downloaded_width]
                .into_iter()
                .enumerate()
            {
                SendMessageW(list, 0x101E, column, column_width as LPARAM);
            }
        }
        place(hwnd, ENTRY_ACTIONS, margin, button_y, 174, 28);
        place(hwnd, STATUS_LABEL, margin + 184, button_y, (full - 184).max(80), 32);
    }
}

unsafe fn layout_progress(hwnd: HWND) {
    let mut rect: RECT = unsafe { zeroed() };
    unsafe {
        GetClientRect(hwnd, &mut rect);
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let margin = 12;
    let full = width - margin * 2;
    let detail_x = margin;
    let detail_width = full;
    unsafe {
        place(hwnd, 500, margin, 10, 78, 24);
        place(hwnd, PROGRESS_SELECT, margin + 82, 8, (full - 306).max(180), 240);
        place(hwnd, PROGRESS_ACCEPT, detail_x + detail_width - 220, 8, 126, 28);
        place(hwnd, PROGRESS_REJECT, detail_x + detail_width - 90, 8, 90, 28);
        place(hwnd, PROGRESS_STATUS, detail_x, 42, detail_width, 24);
        place(hwnd, PROGRESS_OVERALL, detail_x, 70, detail_width, 22);
        place(hwnd, PROGRESS_TOTAL, detail_x, 94, detail_width, 22);
        place(hwnd, PROGRESS_ELAPSED_LABEL, detail_x, 122, 76, 22);
        place(hwnd, PROGRESS_ELAPSED_VALUE, detail_x + 80, 122, 120, 22);
        place(hwnd, 513, detail_x, 148, 38, 22);
        place(hwnd, PROGRESS_FILE, detail_x + 42, 148, detail_width - 42, 38);
        place(hwnd, 514, detail_x, 188, 76, 22);
        place(
            hwnd,
            PROGRESS_DESTINATION,
            detail_x + 80,
            188,
            detail_width - 80,
            38,
        );
        place(hwnd, SEGMENT_STATUS, detail_x, 228, detail_width, 20);
        place(hwnd, SEGMENT_BAR, detail_x, 252, detail_width, 24);
        let button_y = (height - 40).max(370);
        let buttons = [
            (PROGRESS_PAUSE, 82),
            (PROGRESS_CANCEL, 76),
            (PROGRESS_RESUME, 82),
            (PROGRESS_COPY, 112),
            (PROGRESS_OPEN, 100),
        ];
        let mut x = detail_x;
        for (id, button_width) in buttons {
            place(hwnd, id, x, button_y, button_width, 28);
            x += button_width + 6;
        }
    }
}

unsafe fn text(hwnd: HWND, id: i32) -> String {
    let control = unsafe { child(hwnd, id) };
    if control.is_null() {
        return String::new();
    }
    let length = unsafe { GetWindowTextLengthW(control) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let copied = unsafe { GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

unsafe fn set_text(hwnd: HWND, id: i32, value: &str) {
    let control = unsafe { child(hwnd, id) };
    if !control.is_null() {
        if unsafe { text(hwnd, id) } == value {
            return;
        }
        let value = wide(value);
        unsafe {
            SetWindowTextW(control, value.as_ptr());
        }
    }
}

unsafe fn reset_report_list(hwnd: HWND, id: i32) {
    let control = unsafe { child(hwnd, id) };
    if !control.is_null() {
        unsafe {
            SendMessageW(control, LVM_DELETEALLITEMS, 0, 0);
        }
    }
}

fn download_row_values(entry: &super::DownloadEntry) -> [String; 3] {
    let downloaded = entry
        .progress
        .downloaded
        .load(std::sync::atomic::Ordering::Relaxed);
    let total = entry
        .progress
        .total
        .load(std::sync::atomic::Ordering::Relaxed);
    let downloaded_text = if total == 0 {
        format!("{} / unknown", super::format_bytes(downloaded))
    } else {
        format!(
            "{} / {}",
            super::format_bytes(downloaded),
            super::format_bytes(total)
        )
    };
    [
        download_name(entry),
        entry.status.clone(),
        downloaded_text,
    ]
}

unsafe fn set_report_cell(control: HWND, row: usize, column: usize, value: &str) {
    let mut value = wide(value);
    let mut subitem: LVITEMW = unsafe { zeroed() };
    subitem.iSubItem = column as i32;
    subitem.pszText = value.as_mut_ptr();
    unsafe {
        SendMessageW(
            control,
            LVM_SETITEMTEXTW,
            row,
            (&mut subitem as *mut LVITEMW) as LPARAM,
        );
    }
}

unsafe fn insert_report_row(control: HWND, index: usize, values: &[String]) -> Option<usize> {
    if control.is_null() {
        return None;
    }
    let mut file_name = wide(&values[0]);
    let mut item: LVITEMW = unsafe { zeroed() };
    item.mask = LVIF_TEXT;
    item.iItem = index as i32;
    item.pszText = file_name.as_mut_ptr();
    let row = unsafe {
        SendMessageW(
            control,
            LVM_INSERTITEMW,
            0,
            (&mut item as *mut LVITEMW) as LPARAM,
        )
    };
    if row < 0 {
        return None;
    }
    for (column, value) in values.iter().enumerate().skip(1) {
        unsafe { set_report_cell(control, row as usize, column, value) };
    }
    Some(row as usize)
}

unsafe fn add_download_row(hwnd: HWND, id: i32, index: usize, values: &[String; 3]) {
    let control = unsafe { child(hwnd, id) };
    unsafe { insert_report_row(control, index, values) };
}

unsafe fn update_report_row(
    hwnd: HWND,
    id: i32,
    row: usize,
    old: &[String],
    new: &[String],
) {
    let control = unsafe { child(hwnd, id) };
    if control.is_null() {
        return;
    }
    for (column, value) in new.iter().enumerate() {
        if old.get(column) != Some(value) {
            unsafe { set_report_cell(control, row, column, value) };
        }
    }
}

fn download_name(entry: &super::DownloadEntry) -> String {
    entry
        .destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn progress_item_values(manager: &DownloadManager) -> Vec<String> {
    manager
        .entries
        .iter()
        .map(download_name)
        .collect()
}

fn format_elapsed(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

unsafe fn replace_combo_items(hwnd: HWND, id: i32, items: &[String]) {
    let control = unsafe { child(hwnd, id) };
    if control.is_null() {
        return;
    }
    unsafe {
        SendMessageW(control, CB_RESETCONTENT, 0, 0);
        for item in items {
            let item = wide(item);
            SendMessageW(control, CB_ADDSTRING, 0, item.as_ptr() as LPARAM);
        }
    }
}

unsafe fn set_combo_selection(hwnd: HWND, id: i32, index: Option<usize>) {
    let control = unsafe { child(hwnd, id) };
    if !control.is_null() {
        let current = unsafe { SendMessageW(control, CB_GETCURSEL, 0, 0) };
        let requested = index.map_or(usize::MAX, |index| index);
        if current != requested as isize {
            unsafe {
                SendMessageW(control, CB_SETCURSEL, requested, 0);
            }
        }
    }
}

fn link_info_text(manager: &DownloadManager) -> String {
    let Some((checked_url, result)) = &manager.checked_link else {
        return String::new();
    };
    if checked_url != manager.url_input.trim() {
        return String::new();
    }
    match result {
        Ok(info) => format!(
            "HTTP {} · {} · {} · Ranges: {} · Type: {} · Resume: {}",
            info.status,
            info.filename,
            info.size
                .map(super::format_bytes)
                .unwrap_or_else(|| "unknown size".to_owned()),
            if info.supports_ranges { "yes" } else { "no" },
            info.content_type.as_deref().unwrap_or("unknown"),
            if info.has_validator { "yes" } else { "no" }
        ),
        Err(error) => format!("Link check failed: {error}"),
    }
}

fn filter_label(filter: QueueFilter) -> &'static str {
    match filter {
        QueueFilter::All => "Filter: All",
        QueueFilter::Active => "Filter: Active",
        QueueFilter::Completed => "Filter: Completed",
        QueueFilter::Failed => "Filter: Failed",
    }
}

fn theme_label(theme: ThemeChoice) -> &'static str {
    match theme {
        ThemeChoice::System => "Theme: System",
        ThemeChoice::Light => "Theme: Light",
        ThemeChoice::Dark => "Theme: Dark",
    }
}

fn is_dark_theme(theme: ThemeChoice) -> bool {
    match theme {
        ThemeChoice::Light => false,
        ThemeChoice::Dark => true,
        ThemeChoice::System => {
            let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
            let value_name = wide("AppsUseLightTheme");
            let mut value = 1u32;
            let mut size = std::mem::size_of::<u32>() as u32;
            let result = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    value_name.as_ptr(),
                    RRF_RT_REG_DWORD,
                    null_mut(),
                    (&mut value as *mut u32).cast(),
                    &mut size,
                )
            };
            if result == 0 {
                return value == 0;
            }
            let color = unsafe { windows_sys::Win32::Graphics::Gdi::GetSysColor(COLOR_WINDOW) };
            let red = (color & 0xff) as u32;
            let green = ((color >> 8) & 0xff) as u32;
            let blue = ((color >> 16) & 0xff) as u32;
            red * 299 + green * 587 + blue * 114 < 128_000
        }
    }
}

fn color_ref(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
}

unsafe fn popup_command(
    hwnd: HWND,
    anchor_id: i32,
    items: &[(u32, &str)],
) -> Result<Option<u32>, String> {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return Err(format!(
            "Could not create the actions menu (Windows error {}).",
            unsafe { windows_sys::Win32::Foundation::GetLastError() }
        ));
    }

    for (id, label) in items {
        let label = wide(label);
        if unsafe { AppendMenuW(menu, MF_STRING, *id as usize, label.as_ptr()) } == 0 {
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            unsafe {
                DestroyMenu(menu);
            }
            return Err(format!(
                "Could not add a menu item (Windows error {error})."
            ));
        }
    }

    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if anchor_id != 0 {
        let anchor = unsafe { child(hwnd, anchor_id) };
        if anchor.is_null() || unsafe { GetWindowRect(anchor, &mut rect) } == 0 {
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            unsafe {
                DestroyMenu(menu);
            }
            return Err(format!(
                "Could not determine the menu position (Windows error {error})."
            ));
        }
    } else {
        let mut point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut point) } == 0 {
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            unsafe {
                DestroyMenu(menu);
            }
            return Err(format!(
                "Could not determine the menu position (Windows error {error})."
            ));
        }
        rect.left = point.x;
        rect.bottom = point.y;
    }

    let selected = unsafe {
        TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD,
            rect.left,
            rect.bottom,
            0,
            hwnd,
            null(),
        )
    };
    unsafe {
        DestroyMenu(menu);
    }
    Ok((selected > 0).then_some(selected as u32))
}

unsafe fn select_report_item(hwnd: HWND, id: i32, selected: usize) {
    let control = unsafe { child(hwnd, id) };
    if control.is_null() {
        return;
    }
    let mut item: LVITEMW = unsafe { zeroed() };
    item.state = LVIS_SELECTED;
    item.stateMask = LVIS_SELECTED;
    unsafe {
        SendMessageW(
            control,
            LVM_SETITEMSTATE,
            selected,
            (&mut item as *mut LVITEMW) as LPARAM,
        );
    }
}

unsafe fn report_selection(control: HWND) -> Option<usize> {
    if control.is_null() {
        return None;
    }
    let selected = unsafe {
        SendMessageW(
            control,
            LVM_GETNEXTITEM,
            (-1isize) as usize,
            LVNI_SELECTED as LPARAM,
        )
    };
    (selected >= 0).then_some(selected as usize)
}

fn filtered_ids(manager: &DownloadManager) -> Vec<u64> {
    let query = manager.search_input.trim().to_lowercase();
    manager
        .entries
        .iter()
        .filter(|entry| {
            let matches_filter = match manager.queue_filter {
                QueueFilter::All => true,
                QueueFilter::Active => {
                    entry.status == "Downloading"
                        || entry.status == "Queued"
                        || entry.status == "Awaiting confirmation"
                        || entry.status == "Pausing..."
                        || entry.status == "Paused"
                        || entry.status == "Scheduled"
                }
                QueueFilter::Completed => entry.status == "Completed",
                QueueFilter::Failed => entry.status.starts_with("Error:"),
            };
            let name = entry
                .destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            matches_filter
                && (query.is_empty() || name.contains(&query) || entry.url.to_lowercase().contains(&query))
        })
        .map(|entry| entry.id)
        .collect()
}

unsafe fn set_progress(
    hwnd: HWND,
    id: i32,
    downloaded: u64,
    total: u64,
    previous_total: &mut u64,
) {
    let control = unsafe { child(hwnd, id) };
    if control.is_null() {
        return;
    }
    let total = total.min(i32::MAX as u64) as i32;
    let position = downloaded.min(total.max(0) as u64) as i32;
    unsafe {
        if *previous_total != total as u64 {
            SendMessageW(control, PBM_SETRANGE32, 0, total as LPARAM);
            *previous_total = total as u64;
        }
        SendMessageW(control, PBM_SETPOS, position as WPARAM, 0);
    }
}

unsafe fn refresh_ui(state: &mut UiState) {
    let manager = &state.manager;
    let visible_ids = filtered_ids(manager);
    let visible_rows = visible_ids
        .iter()
        .filter_map(|id| {
            manager
                .entries
                .iter()
                .find(|entry| entry.id == *id)
                .map(download_row_values)
        })
        .collect::<Vec<_>>();
    let ids_changed = visible_ids != state.main_visible_ids;
    let row_changes = ids_changed
        || visible_rows
            .iter()
            .zip(&state.main_rendered_rows)
            .any(|(new, old)| new != old);
    let main_list = unsafe { child(state.main_window, MAIN_LIST) };
    if row_changes && !main_list.is_null() {
        unsafe {
            SendMessageW(main_list, WM_SETREDRAW_MESSAGE, 0, 0);
            if ids_changed {
                reset_report_list(state.main_window, MAIN_LIST);
                for (index, row) in visible_rows.iter().enumerate() {
                    add_download_row(state.main_window, MAIN_LIST, index, row);
                }
            } else {
                for (index, (old, new)) in state
                    .main_rendered_rows
                    .iter()
                    .zip(&visible_rows)
                    .enumerate()
                {
                    if old != new {
                        update_report_row(
                            state.main_window,
                            MAIN_LIST,
                            index,
                            old,
                            new,
                        );
                    }
                }
            }
            SendMessageW(main_list, WM_SETREDRAW_MESSAGE, 1, 0);
            RedrawWindow(main_list, null(), null_mut(), RDW_INVALIDATE);
        }
    }
    state.main_visible_ids = visible_ids;
    state.main_rendered_rows = visible_rows;
    if let Some(index) = state
        .main_visible_ids
        .iter()
        .position(|id| *id == manager.selected_download_id)
    {
        let list = unsafe { child(state.main_window, MAIN_LIST) };
        if unsafe { report_selection(list) } != Some(index) {
            unsafe {
                select_report_item(state.main_window, MAIN_LIST, index);
            }
        }
    }

    let counts = format!(
        "{} downloading · {} waiting · {} completed · Session speed: {}",
        manager
            .entries
            .iter()
            .filter(|entry| entry.status == "Downloading")
            .count(),
        manager
            .entries
            .iter()
            .filter(|entry| {
                entry.status == "Queued"
                    || entry.status == "Awaiting confirmation"
                    || entry.status == "Paused"
                    || entry.status == "Scheduled"
            })
            .count(),
        manager
            .entries
            .iter()
            .filter(|entry| entry.status == "Completed")
            .count(),
        super::format_speed(manager.session_speed_bps)
    );
    unsafe {
        set_text(state.main_window, LINK_LABEL, &link_info_text(manager));
        set_text(
            state.main_window,
            RUN_QUEUE,
            if manager.queue_running {
                "Stop queue"
            } else {
                "Run queue"
            },
        );
        set_text(
            state.main_window,
            FILTER_MENU,
            filter_label(manager.queue_filter),
        );
        set_text(state.main_window, THEME_MENU, theme_label(state.theme));
        set_text(
            state.main_window,
            STATUS_LABEL,
            &format!("{counts}  |  {}", manager.status),
        );
        let selected = manager
            .entries
            .iter()
            .find(|entry| entry.id == manager.selected_download_id);
        let can_resume = manager.active_id.is_none()
            && selected.is_some_and(|entry| entry.status == "Paused" || entry.status == "Queued");
        let can_stop = selected.is_some_and(|entry| manager.active_id == Some(entry.id));
        let can_delete =
            selected.is_some_and(|entry| manager.active_id != Some(entry.id));
        let _ = EnableWindow(child(state.main_window, TB_RESUME), can_resume as i32);
        let _ = EnableWindow(child(state.main_window, TB_STOP), can_stop as i32);
        let _ = EnableWindow(child(state.main_window, TB_DELETE), can_delete as i32);
    }

    if state.progress_window.is_null() {
        return;
    }
    let progress_ids = manager.entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
    let progress_items = progress_item_values(manager);
    if progress_items != state.progress_rendered_items {
        unsafe {
            replace_combo_items(state.progress_window, PROGRESS_SELECT, &progress_items);
        }
    }
    state.progress_visible_ids = progress_ids;
    state.progress_rendered_items = progress_items;
    let selected_id = if manager
        .entries
        .iter()
        .any(|entry| entry.id == state.selected_progress_id)
    {
        state.selected_progress_id
    } else {
        manager.selected_download_id
    };
    state.selected_progress_id = selected_id;
    let selected_index = state
        .progress_visible_ids
        .iter()
        .position(|id| *id == selected_id);
    unsafe {
        set_combo_selection(state.progress_window, PROGRESS_SELECT, selected_index);
    }
    if let Some(entry) = manager.entries.iter().find(|entry| entry.id == selected_id) {
        let downloaded = entry
            .progress
            .downloaded
            .load(std::sync::atomic::Ordering::Relaxed);
        let total = entry
            .progress
            .total
            .load(std::sync::atomic::Ordering::Relaxed);
        unsafe {
            set_text(
                state.progress_window,
                PROGRESS_STATUS,
                &format!(
                    "Status: {}  |  Session speed: {}",
                    entry.status,
                    super::format_speed(manager.session_speed_bps)
                ),
            );
            set_text(
                state.progress_window,
                PROGRESS_ELAPSED_VALUE,
                &format_elapsed(entry.elapsed_download()),
            );
            set_progress(
                state.progress_window,
                PROGRESS_OVERALL,
                downloaded,
                total,
                &mut state.rendered_progress_total,
            );
            set_text(
                state.progress_window,
                PROGRESS_TOTAL,
                &format!(
                    "{} / {}",
                    super::format_bytes(downloaded),
                    if total == 0 {
                        "unknown".to_owned()
                    } else {
                        super::format_bytes(total)
                    }
                ),
            );
            set_text(state.progress_window, PROGRESS_FILE, &download_name(entry));
            set_text(
                state.progress_window,
                PROGRESS_DESTINATION,
                &entry.destination.to_string_lossy(),
            );
            let segment_status = if entry.browser_handoff.is_some() {
                "Waiting for your confirmation".to_owned()
            } else if entry.merging {
                format!(
                    "Assembling file: {} / {}",
                    super::format_bytes(entry.merge_downloaded),
                    super::format_bytes(entry.merge_total)
                )
            } else {
                let active = entry.segment_lanes.iter().filter(|lane| lane.active).count();
                format!("Download activity · {active} active connections")
            };
            set_text(
                state.progress_window,
                SEGMENT_STATUS,
                &segment_status,
            );
            let segment_bar = child(state.progress_window, SEGMENT_BAR);
            if !segment_bar.is_null() {
                InvalidateRect(segment_bar, null(), 1);
            }
            let active = manager.active_id == Some(entry.id);
            let awaiting_confirmation = entry.browser_handoff.is_some();
            let _ = ShowWindow(
                child(state.progress_window, PROGRESS_ACCEPT),
                if awaiting_confirmation { SW_SHOW } else { SW_HIDE },
            );
            let _ = ShowWindow(
                child(state.progress_window, PROGRESS_REJECT),
                if awaiting_confirmation { SW_SHOW } else { SW_HIDE },
            );
            let _ = EnableWindow(
                child(state.progress_window, PROGRESS_ACCEPT),
                awaiting_confirmation as i32,
            );
            let _ = EnableWindow(
                child(state.progress_window, PROGRESS_REJECT),
                awaiting_confirmation as i32,
            );
            let _ = EnableWindow(child(state.progress_window, PROGRESS_PAUSE), active as i32);
            let _ = EnableWindow(child(state.progress_window, PROGRESS_CANCEL), active as i32);
            let _ = EnableWindow(
                child(state.progress_window, PROGRESS_RESUME),
                (!active && entry.status == "Paused") as i32,
            );
        }
    }
}

unsafe fn sync_inputs(state: &mut UiState) {
    state.manager.url_input = unsafe { text(state.main_window, URL_EDIT) };
    state.manager.folder_input = unsafe { text(state.main_window, FOLDER_EDIT) };
    state.manager.schedule_input = unsafe { schedule_text(state.main_window) };
    state.manager.search_input = unsafe { text(state.main_window, SEARCH_EDIT) };
}

unsafe fn reset_download_inputs(state: &mut UiState) {
    unsafe {
        set_text(state.main_window, URL_EDIT, "");
        SendMessageW(
            child(state.main_window, SCHEDULE_EDIT),
            DTM_SETSYSTEMTIME,
            GDT_NONE as WPARAM,
            0,
        );
    }
}

unsafe fn schedule_text(hwnd: HWND) -> String {
    let mut system_time: SYSTEMTIME = unsafe { zeroed() };
    let result = unsafe {
        SendMessageW(
            child(hwnd, SCHEDULE_EDIT),
            DTM_GETSYSTEMTIME,
            0,
            (&mut system_time as *mut SYSTEMTIME) as LPARAM,
        )
    };
    if result != GDT_VALID as isize {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        system_time.wYear,
        system_time.wMonth,
        system_time.wDay,
        system_time.wHour,
        system_time.wMinute
    )
}

unsafe fn show_progress_window(state: &mut UiState) {
    if state.progress_window.is_null() {
        let class = wide(PROGRESS_CLASS);
        let title = wide("Download progress - RDM");
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                850,
                560,
                if state.progress_only {
                    null_mut()
                } else {
                    state.main_window
                },
                null_mut(),
                state.instance as _,
                (state as *mut UiState).cast(),
            )
        };
        if window.is_null() {
            state.manager.status = format!(
                "Could not open the progress window (Windows error {}).",
                unsafe { windows_sys::Win32::Foundation::GetLastError() }
            );
            return;
        }
        state.progress_window = window;
    }
    state.manager.progress_window_open = true;
    if !state.progress_visible {
        state.progress_visible = true;
        unsafe {
            ShowWindow(state.progress_window, SW_SHOW);
            UpdateWindow(state.progress_window);
            SetForegroundWindow(state.progress_window);
        }
    }
}

unsafe fn dispatch_command(state: &mut UiState, hwnd: HWND, id: i32, code: u16) {
    if hwnd == state.main_window && code == EN_CHANGE as u16 {
        unsafe {
            sync_inputs(state);
        }
        return;
    }
    match id {
        CHECK_LINK => {
            unsafe { sync_inputs(state) };
            state.manager.check_link();
        }
        BROWSE_FOLDER => {
            let folder = unsafe { text(state.main_window, FOLDER_EDIT) };
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Choose a download folder")
                .set_directory(Path::new(&folder))
                .pick_folder()
            {
                state.manager.folder_input = path.to_string_lossy().into_owned();
                state.manager.persist_or_report();
                unsafe {
                    set_text(state.main_window, FOLDER_EDIT, &state.manager.folder_input);
                }
            }
        }
        ADD_QUEUE => {
            unsafe { sync_inputs(state) };
            let previous_count = state.manager.entries.len();
            state.manager.add_download();
            if state.manager.entries.len() > previous_count {
                unsafe { reset_download_inputs(state) };
            }
        }
        DOWNLOAD_NOW => {
            unsafe { sync_inputs(state) };
            let previous_count = state.manager.entries.len();
            state.manager.download_now();
            if state.manager.entries.len() > previous_count {
                unsafe { reset_download_inputs(state) };
            }
        }
        RUN_QUEUE => {
            if state.manager.queue_running {
                state.manager.pause_queue();
            } else {
                state.manager.queue_running = true;
                state.manager.status = "Running the queue.".to_owned();
                state.manager.persist_or_report();
                state.manager.start_next();
            }
        }
        TB_RESUME => {
            if state.manager.active_id.is_none() {
                state.manager.start_entry(state.manager.selected_download_id);
            }
        }
        TB_STOP => {
            state
                .manager
                .pause_entry(state.manager.selected_download_id);
        }
        TB_DELETE => {
            state.manager.handle_entry_action(EntryAction::Remove(
                state.manager.selected_download_id,
            ));
        }
        SHOW_PROGRESS => unsafe { show_progress_window(state) },
        FILTER_MENU => match unsafe {
            popup_command(
                hwnd,
                FILTER_MENU,
                &[
                    (FILTER_ALL, "All"),
                    (FILTER_ACTIVE, "Active"),
                    (FILTER_COMPLETED, "Completed"),
                    (FILTER_FAILED, "Failed"),
                ],
            )
        } {
            Ok(Some(FILTER_ALL)) => state.manager.queue_filter = QueueFilter::All,
            Ok(Some(FILTER_ACTIVE)) => state.manager.queue_filter = QueueFilter::Active,
            Ok(Some(FILTER_COMPLETED)) => state.manager.queue_filter = QueueFilter::Completed,
            Ok(Some(FILTER_FAILED)) => state.manager.queue_filter = QueueFilter::Failed,
            Ok(None) => {}
            Ok(Some(_)) => {}
            Err(error) => state.manager.status = error,
        },
        THEME_MENU => match unsafe {
            popup_command(
                hwnd,
                THEME_MENU,
                &[
                    (THEME_SYSTEM, "System"),
                    (THEME_LIGHT, "Light"),
                    (THEME_DARK, "Dark"),
                ],
            )
        } {
            Ok(Some(command @ (THEME_SYSTEM | THEME_LIGHT | THEME_DARK))) => {
                state.theme = match command {
                    THEME_SYSTEM => ThemeChoice::System,
                    THEME_LIGHT => ThemeChoice::Light,
                    _ => ThemeChoice::Dark,
                };
                if let Err(error) = save_theme(state.theme) {
                    state.manager.status = error;
                }
                unsafe { repaint_theme(state) };
            }
            Ok(None) | Ok(Some(_)) => {}
            Err(error) => state.manager.status = error,
        },
        CLEAR_COMPLETED => state.manager.clear_completed(),
        ENTRY_ACTIONS => {
            let selected_action = unsafe {
                popup_command(
                    hwnd,
                    ENTRY_ACTIONS,
                    &[
                        (OPEN_FOLDER, "Open folder"),
                        (COPY_URL, "Copy URL"),
                        (RETRY, "Retry"),
                        (REMOVE, "Remove from queue"),
                    ],
                )
            };
            match selected_action {
                Ok(Some(command)) => {
                    let id = state.manager.selected_download_id;
                    let action = match command {
                        OPEN_FOLDER => Some(EntryAction::OpenFolder(id)),
                        COPY_URL => Some(EntryAction::CopyUrl(id)),
                        RETRY => Some(EntryAction::Retry(id)),
                        REMOVE => Some(EntryAction::Remove(id)),
                        _ => None,
                    };
                    if let Some(action) = action {
                        state.manager.handle_entry_action(action);
                    }
                }
                Ok(None) => {}
                Err(error) => state.manager.status = error,
            }
        }
        PROGRESS_PAUSE => state.manager.pause_entry(state.manager.active_id.unwrap_or(0)),
        PROGRESS_CANCEL => state.manager.cancel_entry(state.manager.active_id.unwrap_or(0)),
        PROGRESS_RESUME => {
            if state.manager.active_id.is_none() {
                state.manager.start_entry(state.selected_progress_id);
            }
        }
        PROGRESS_COPY => state
            .manager
            .handle_entry_action(EntryAction::CopyUrl(state.selected_progress_id)),
        PROGRESS_OPEN => state
            .manager
            .handle_entry_action(EntryAction::OpenFolder(state.selected_progress_id)),
        PROGRESS_ACCEPT => state
            .manager
            .decide_browser_download(state.selected_progress_id, true),
        PROGRESS_REJECT => state
            .manager
            .decide_browser_download(state.selected_progress_id, false),
        _ => {}
    }
}

unsafe fn set_user_data(hwnd: HWND, state: *mut UiState) {
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
    }
}

unsafe fn state_from_window(hwnd: HWND) -> *mut UiState {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState }
}

unsafe fn set_minimum_window_size(hwnd: HWND, lparam: LPARAM, is_progress: bool) {
    let (width, height) = if is_progress {
        (PROGRESS_MIN_CLIENT_WIDTH, PROGRESS_MIN_CLIENT_HEIGHT)
    } else {
        (MAIN_MIN_CLIENT_WIDTH, MAIN_MIN_CLIENT_HEIGHT)
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    unsafe {
        AdjustWindowRectEx(&mut rect, style, 0, ex_style);
        let minmax = &mut *(lparam as *mut MINMAXINFO);
        minmax.ptMinTrackSize.x = rect.right - rect.left;
        minmax.ptMinTrackSize.y = rect.bottom - rect.top;
    }
}

unsafe fn repaint_theme(state: &UiState) {
    let (background, text) = if is_dark_theme(state.theme) {
        (color_ref(32, 32, 32), color_ref(240, 240, 240))
    } else {
        (
            unsafe { GetSysColor(COLOR_WINDOW) },
            unsafe { GetSysColor(COLOR_WINDOWTEXT) },
        )
    };
    for list in [unsafe { child(state.main_window, MAIN_LIST) }] {
        if !list.is_null() {
            unsafe {
                SendMessageW(list, LVM_SETBKCOLOR_MESSAGE, 0, background as LPARAM);
                SendMessageW(list, LVM_SETTEXTCOLOR_MESSAGE, 0, text as LPARAM);
                SendMessageW(list, LVM_SETTEXTBKCOLOR_MESSAGE, 0, background as LPARAM);
            }
        }
    }
    for hwnd in [state.main_window, state.progress_window] {
        if !hwnd.is_null() {
            unsafe {
                RedrawWindow(
                    hwnd,
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                );
            }
        }
    }
}

unsafe fn control_color(state: &UiState, message: u32, wparam: WPARAM) -> LRESULT {
    let dark = is_dark_theme(state.theme);
    let (text_color, background_color, brush) = if dark {
        (
            color_ref(240, 240, 240),
            color_ref(32, 32, 32),
            state.dark_brush,
        )
    } else if message == WM_CTLCOLORSTATIC || message == WM_CTLCOLORBTN {
        (
            unsafe { windows_sys::Win32::Graphics::Gdi::GetSysColor(COLOR_BTNTEXT) },
            unsafe { windows_sys::Win32::Graphics::Gdi::GetSysColor(COLOR_BTNFACE) },
            unsafe {
                windows_sys::Win32::Graphics::Gdi::GetSysColorBrush(COLOR_BTNFACE)
            },
        )
    } else {
        (
            unsafe { windows_sys::Win32::Graphics::Gdi::GetSysColor(COLOR_WINDOWTEXT) },
            unsafe { windows_sys::Win32::Graphics::Gdi::GetSysColor(COLOR_WINDOW) },
            unsafe {
                windows_sys::Win32::Graphics::Gdi::GetSysColorBrush(COLOR_WINDOW)
            },
        )
    };
    unsafe {
        SetTextColor(wparam as _, text_color);
        SetBkColor(wparam as _, background_color);
    }
    brush as LRESULT
}

unsafe fn draw_button(state: &UiState, item: &mut DRAWITEMSTRUCT) {
    let dark = is_dark_theme(state.theme);
    let selected = item.itemState & ODS_SELECTED != 0;
    let disabled = item.itemState & ODS_DISABLED != 0;
    let (fill, text_color, edge_color) = if dark {
        let fill = if selected {
            color_ref(58, 58, 58)
        } else {
            color_ref(43, 43, 43)
        };
        (
            unsafe { CreateSolidBrush(fill) },
            if disabled {
                color_ref(128, 128, 128)
            } else {
                color_ref(240, 240, 240)
            },
            color_ref(82, 82, 82),
        )
    } else {
        (
            unsafe { GetSysColorBrush(COLOR_BTNFACE) },
            unsafe {
                GetSysColor(if disabled {
                    windows_sys::Win32::Graphics::Gdi::COLOR_GRAYTEXT
                } else {
                    COLOR_BTNTEXT
                })
            },
            unsafe { GetSysColor(COLOR_BTNSHADOW) },
        )
    };
    unsafe {
        FillRect(item.hDC, &item.rcItem, fill);
        let edge = CreateSolidBrush(edge_color);
        FrameRect(item.hDC, &item.rcItem, edge);
        DeleteObject(edge);
        if dark {
            DeleteObject(fill);
        }
        SetTextColor(item.hDC, text_color);
        SetBkColor(
            item.hDC,
            if dark {
                color_ref(43, 43, 43)
            } else {
                GetSysColor(COLOR_BTNFACE)
            },
        );
        let length = GetWindowTextLengthW(item.hwndItem).max(0) as usize;
        let mut text = vec![0u16; length + 1];
        let copied = GetWindowTextW(item.hwndItem, text.as_mut_ptr(), text.len() as i32);
        let mut rect = item.rcItem;
        rect.left += 6;
        rect.right -= 6;
        DrawTextW(
            item.hDC,
            text.as_ptr(),
            copied,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        if item.itemState & ODS_FOCUS != 0 {
            rect.left += 2;
            rect.right -= 2;
            rect.top += 2;
            rect.bottom -= 2;
            DrawFocusRect(item.hDC, &rect);
        }
    }
}

unsafe fn draw_segment_bar(state: &UiState, item: &mut DRAWITEMSTRUCT) {
    let entry = state
        .manager
        .entries
        .iter()
        .find(|entry| entry.id == state.selected_progress_id);
    let background_color = if is_dark_theme(state.theme) {
        color_ref(32, 32, 32)
    } else {
        unsafe { GetSysColor(COLOR_WINDOW) }
    };
    let background = unsafe { CreateSolidBrush(background_color) };
    unsafe {
        FillRect(item.hDC, &item.rcItem, background);
        DeleteObject(background);
    }

    let Some(entry) = entry else {
        return;
    };
    let width = item.rcItem.right - item.rcItem.left;
    let height = item.rcItem.bottom - item.rcItem.top;
    if width <= 2 || height <= 2 {
        return;
    }
    let left = item.rcItem.left + 1;
    let top = item.rcItem.top + 1;
    let inner_width = width - 2;
    let inner_height = height - 2;
    if entry.merging && entry.merge_total > 0 {
        let filled = ((u128::from(entry.merge_downloaded.min(entry.merge_total))
            * inner_width as u128)
            / u128::from(entry.merge_total)) as i32;
        if filled > 0 {
            let brush = unsafe { CreateSolidBrush(color_ref(16, 124, 16)) };
            let rect = RECT {
                left,
                top,
                right: left + filled,
                bottom: top + inner_height,
            };
            unsafe {
                FillRect(item.hDC, &rect, brush);
                DeleteObject(brush);
            }
        }
        return;
    }

    let total = entry.segment_lanes.iter().map(|lane| lane.total).sum::<u64>();
    let total = if total == 0 {
        entry
            .progress
            .total
            .load(std::sync::atomic::Ordering::Relaxed)
    } else {
        total
    };
    let mut offset = 0i32;
    for (index, lane) in entry.segment_lanes.iter().enumerate() {
        let segment_width = if total == 0 {
            inner_width / entry.segment_lanes.len().max(1) as i32
        } else {
            ((u128::from(lane.total) * inner_width as u128) / u128::from(total)) as i32
        };
        let next_offset = if index + 1 == entry.segment_lanes.len() {
            inner_width
        } else {
            (offset + segment_width).min(inner_width)
        };
        let lane_width = next_offset - offset;
        let completed_width = if lane.total == 0 {
            0
        } else {
            ((u128::from(lane.downloaded.min(lane.total)) * lane_width as u128)
                / u128::from(lane.total)) as i32
        };
        if completed_width > 0 {
            let brush = unsafe { CreateSolidBrush(color_ref(0, 120, 212)) };
            let rect = RECT {
                left: left + offset,
                top,
                right: left + offset + completed_width,
                bottom: top + inner_height,
            };
            unsafe {
                FillRect(item.hDC, &rect, brush);
                DeleteObject(brush);
            }
        }
        offset = next_offset;
    }
}

unsafe extern "system" fn main_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_GETMINMAXINFO {
        unsafe {
            set_minimum_window_size(hwnd, lparam, false);
        }
        return 0;
    }
    unsafe { window_proc(hwnd, message, wparam, lparam, false) }
}

unsafe extern "system" fn progress_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_GETMINMAXINFO {
        unsafe {
            set_minimum_window_size(hwnd, lparam, true);
        }
        return 0;
    }
    unsafe { window_proc(hwnd, message, wparam, lparam, true) }
}

unsafe fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    is_progress: bool,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam as *const CREATESTRUCTW) };
        let state = create.lpCreateParams.cast::<UiState>();
        if !state.is_null() {
            unsafe {
                set_user_data(hwnd, state);
            }
            if is_progress {
                unsafe { (*state).progress_window = hwnd };
            } else {
                unsafe { (*state).main_window = hwnd };
            }
        }
        return 1;
    }
    let state = unsafe { state_from_window(hwnd) };
    if state.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &mut *state };

    match message {
        WM_COPYDATA => {
            if lparam == 0 {
                return 0;
            }
            let copy_data = unsafe { &*(lparam as *const BrowserCopyData) };
            if copy_data.data_id != BROWSER_DOWNLOAD_COPYDATA_ID
                || copy_data.byte_count == 0
                || copy_data.byte_count > 16 * 1024
                || copy_data.data.is_null()
            {
                return 0;
            }
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    copy_data.data.cast::<u8>(),
                    copy_data.byte_count as usize,
                )
            };
            let Ok(message) = serde_json::from_slice::<serde_json::Value>(bytes) else {
                return 0;
            };
            let (Some(url), filename, Some(port), Some(token)) = (
                message.get("url").and_then(serde_json::Value::as_str),
                message
                    .get("filename")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
                message
                    .get("port")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok()),
                message.get("token").and_then(serde_json::Value::as_str),
            ) else {
                return 0;
            };
            if state
                .manager
                .stage_browser_download(
                    url.to_owned(),
                    (!filename.is_empty()).then(|| filename.to_owned()),
                    port,
                    token.to_owned(),
                )
                .is_err()
            {
                return 0;
            }
            state.progress_only = true;
            unsafe {
                ShowWindow(hwnd, SW_HIDE);
                if !state.progress_window.is_null() {
                    SetWindowLongPtrW(state.progress_window, GWLP_HWNDPARENT, 0);
                }
                show_progress_window(state);
                refresh_ui(state);
            }
            1
        }
        WM_CREATE => {
            if is_progress {
                unsafe { create_progress_controls(state, hwnd) };
                unsafe { layout_progress(hwnd) };
            } else {
                unsafe { create_main_controls(state, hwnd) };
                unsafe { repaint_theme(state) };
                unsafe { layout_main(hwnd) };
                unsafe {
                    SetTimer(hwnd, MAIN_TIMER, 250, None);
                }
                let date_format = wide("yyyy-MM-dd HH:mm");
                unsafe {
                    SendMessageW(
                        child(hwnd, SCHEDULE_EDIT),
                        DTM_SETFORMATW,
                        0,
                        date_format.as_ptr() as LPARAM,
                    );
                    SendMessageW(
                        child(hwnd, SCHEDULE_EDIT),
                        DTM_SETSYSTEMTIME,
                        GDT_NONE as WPARAM,
                        0,
                    );
                }
                unsafe {
                    set_text(hwnd, URL_EDIT, &state.manager.url_input);
                    set_text(hwnd, FOLDER_EDIT, &state.manager.folder_input);
                }
            }
            0
        }
        WM_SIZE => {
            if is_progress {
                unsafe { layout_progress(hwnd) };
            } else {
                unsafe { layout_main(hwnd) };
            }
            0
        }
        WM_COMMAND => {
            let control_id = (wparam & 0xffff) as i32;
            let notification = ((wparam >> 16) & 0xffff) as u16;
            if is_progress && control_id == PROGRESS_SELECT && notification == CBN_SELCHANGE {
                let selection = unsafe {
                    SendMessageW(child(hwnd, PROGRESS_SELECT), CB_GETCURSEL, 0, 0)
                };
                if selection >= 0 {
                    if let Some(id) = state.progress_visible_ids.get(selection as usize) {
                        state.selected_progress_id = *id;
                        state.manager.selected_download_id = *id;
                    }
                }
            }
            unsafe { dispatch_command(state, hwnd, control_id, notification) };
            unsafe { refresh_ui(state) };
            0
        }
        WM_NOTIFY if lparam != 0 => {
            let notification = unsafe { &*(lparam as *const NMHDR) };
            if notification.code == LVN_ITEMCHANGED {
                if !is_progress && notification.idFrom as i32 == MAIN_LIST {
                    let list = unsafe { child(hwnd, MAIN_LIST) };
                    if let Some(index) = unsafe { report_selection(list) } {
                        if let Some(id) = state.main_visible_ids.get(index) {
                            state.manager.selected_download_id = *id;
                            state.selected_progress_id = *id;
                        }
                    }
                }
            }
            0
        }
        WM_TIMER if !is_progress => {
            state.manager.handle_events();
            if state.manager.queue_running && state.manager.active_id.is_none() {
                state.manager.start_next();
            }
            if state.manager.progress_window_open {
                unsafe { show_progress_window(state) };
            }
            unsafe { refresh_ui(state) };
            0
        }
        WM_SETTINGCHANGE => {
            unsafe { repaint_theme(state) };
            0
        }
        WM_DRAWITEM => {
            let item = unsafe { &mut *(lparam as *mut DRAWITEMSTRUCT) };
            if item.CtlID as i32 == SEGMENT_BAR {
                unsafe { draw_segment_bar(state, item) };
                1
            } else if item.CtlType == ODT_BUTTON {
                unsafe { draw_button(state, item) };
                1
            } else {
                0
            }
        }
        WM_ERASEBKGND if is_dark_theme(state.theme) => {
            let mut rect: RECT = unsafe { zeroed() };
            unsafe {
                GetClientRect(hwnd, &mut rect);
                FillRect(wparam as _, &rect, state.dark_brush);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
            unsafe { control_color(state, message, wparam) }
        }
        WM_CLOSE if is_progress => {
            state.manager.progress_window_open = false;
            state.progress_visible = false;
            if state.progress_only {
                state.manager.reject_pending_browser_downloads();
                state.progress_only = false;
                unsafe {
                    ShowWindow(state.main_window, SW_SHOW);
                    SetForegroundWindow(state.main_window);
                }
            }
            unsafe {
                ShowWindow(hwnd, SW_HIDE);
            }
            0
        }
        WM_CLOSE => {
            state.manager.queue_running = false;
            state.manager.reject_pending_browser_downloads();
            if let Err(error) = state.manager.persist_queue() {
                state.manager.status = format!("Could not save the queue on exit: {error}");
            }
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY if !is_progress => {
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn register_window_class(
    instance: isize,
    class_name: &str,
    procedure: WNDPROC,
) -> Result<(), String> {
    let class_name = wide(class_name);
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: procedure,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance as _,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: unsafe {
            windows_sys::Win32::Graphics::Gdi::GetSysColorBrush(COLOR_BTNFACE)
        },
        lpszMenuName: null(),
        lpszClassName: class_name.as_ptr(),
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(format!(
            "Could not register the Win32 window (error {}).",
            unsafe { windows_sys::Win32::Foundation::GetLastError() }
        ));
    }
    Ok(())
}

pub(super) fn run(
    initial_url: Option<String>,
    auto_download: bool,
    browser_handoff: Option<(String, String, u16, String)>,
) -> Result<(), String> {
    let mut common_controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_PROGRESS_CLASS | ICC_DATE_CLASSES | ICC_LISTVIEW_CLASSES,
    };
    if unsafe { InitCommonControlsEx(&mut common_controls) } == 0 {
        return Err("Could not initialize Windows Common Controls.".to_owned());
    }
    let instance = unsafe { GetModuleHandleW(null()) } as isize;
    if instance == 0 {
        return Err("Could not get the current Windows module.".to_owned());
    }
    unsafe {
        register_window_class(instance, MAIN_CLASS, Some(main_window_proc))?;
        register_window_class(instance, PROGRESS_CLASS, Some(progress_window_proc))?;
    }

    let (settings, settings_error) = match read_settings() {
        Ok(settings) => (settings, None),
        Err(error) => (serde_json::json!({}), Some(error)),
    };
    let theme = load_theme(&settings).unwrap_or(ThemeChoice::System);
    let dark_brush = unsafe { CreateSolidBrush(color_ref(32, 32, 32)) };
    if dark_brush.is_null() {
        return Err(format!(
            "Could not create the dark theme background brush (error {}).",
            unsafe { windows_sys::Win32::Foundation::GetLastError() }
        ));
    }
    let progress_only = browser_handoff.is_some();
    let mut manager = DownloadManager::new(initial_url);
    if let Some(error) = settings_error {
        manager.status = error;
    }
    if auto_download {
        manager.download_now();
    }
    if let Some((url, filename, port, token)) = browser_handoff {
        manager.stage_browser_download(
            url,
            (!filename.is_empty()).then_some(filename),
            port,
            token,
        )?;
    }
    let mut state = Box::new(UiState {
        manager,
        instance,
        main_window: null_mut(),
        progress_window: null_mut(),
        progress_visible: false,
        progress_only,
        main_visible_ids: Vec::new(),
        main_rendered_rows: Vec::new(),
        progress_visible_ids: Vec::new(),
        progress_rendered_items: Vec::new(),
        rendered_progress_total: 0,
        selected_progress_id: 0,
        theme,
        dark_brush,
    });
    let class = wide(MAIN_CLASS);
    let title = wide("RDM - Downloads");
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1040,
            800,
            null_mut(),
            null_mut(),
            instance as _,
            (&mut *state as *mut UiState).cast(),
        )
    };
    if window.is_null() {
        unsafe {
            DeleteObject(dark_brush);
        }
        return Err(format!(
            "Could not create the RDM window (error {}).",
            unsafe { windows_sys::Win32::Foundation::GetLastError() }
        ));
    }
    unsafe {
        SendMessageW(window, WM_SETTINGCHANGE, 0, 0);
    }
    state.main_window = window;
    if progress_only {
        unsafe {
            ShowWindow(window, SW_HIDE);
            show_progress_window(&mut state);
        }
    } else {
        unsafe {
            ShowWindow(window, SW_SHOW);
            UpdateWindow(window);
        }
    }

    let mut message: MSG = unsafe { zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result == -1 {
            unsafe {
                DeleteObject(dark_brush);
            }
            return Err(format!(
                "Windows message loop failed (error {}).",
                unsafe { windows_sys::Win32::Foundation::GetLastError() }
            ));
        }
        if result == 0 {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    unsafe {
        DeleteObject(dark_brush);
    }
    Ok(())
}
