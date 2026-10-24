//! Small native Win32 window and notification-area controller.
//! UI state is a snapshot of the worker; painting never performs recognition.

use std::{
    mem::size_of,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{DRAWITEMSTRUCT, ODS_SELECTED},
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

use crate::app::{AppState, Control};

const CLASS: PCWSTR = w!("VoiceControl.MainWindow");
const TRAY_MESSAGE: u32 = WM_APP + 17;
const TIMER: usize = 1;
const LISTEN: usize = 101;
const RELOAD: usize = 102;
const CONFIG: usize = 103;
const LOG: usize = 104;
const RESYNC: usize = 105;
const SHOW: usize = 201;
const EXIT: usize = 202;

const BG: COLORREF = color(17, 22, 29);
const PANEL: COLORREF = color(25, 33, 43);
const PANEL_ALT: COLORREF = color(32, 42, 54);
const TEXT: COLORREF = color(235, 241, 246);
const MUTED: COLORREF = color(150, 166, 182);
const ACCENT: COLORREF = color(118, 220, 192);
const WARNING: COLORREF = color(245, 176, 117);

const fn color(r: u32, g: u32, b: u32) -> COLORREF {
    COLORREF(r | g << 8 | b << 16)
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct Ui {
    shared: Arc<Mutex<AppState>>,
    tx: Sender<Control>,
    config: PathBuf,
    log: PathBuf,
    title_font: HFONT,
    heading_font: HFONT,
    body_font: HFONT,
    small_font: HFONT,
    buttons: Vec<(usize, HWND)>,
    last_paused: Option<bool>,
    taskbar_created: u32,
    idle_icon: HICON,
    speaking_icon: HICON,
    window_icon: HICON,
    speaking_window_icon: HICON,
    speaking: bool,
    tray_registered: bool,
}

impl Drop for Ui {
    fn drop(&mut self) {
        unsafe {
            for font in [
                self.title_font,
                self.heading_font,
                self.body_font,
                self.small_font,
            ] {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
            for icon in [
                self.idle_icon,
                self.speaking_icon,
                self.window_icon,
                self.speaking_window_icon,
            ] {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

impl Ui {
    fn control(&self, command: Control) {
        if self.tx.try_send(command).is_err() {
            if let Ok(mut state) = self.shared.lock() {
                state.error = "Не удалось передать команду рабочему потоку".into();
            }
        }
    }

    unsafe fn tray(&self, hwnd: HWND, operation: NOTIFY_ICON_MESSAGE) -> bool {
        unsafe {
            let mut data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: TRAY_MESSAGE,
                hIcon: if self.speaking {
                    self.speaking_icon
                } else {
                    self.idle_icon
                },
                ..Default::default()
            };
            let text = self
                .shared
                .lock()
                .map(|s| {
                    format!(
                        "Voice Control: {}",
                        if self.speaking {
                            "слышу речь"
                        } else {
                            &s.status
                        }
                    )
                })
                .unwrap_or_else(|_| "Voice Control".into());
            let tip = wide(&text);
            let len = (tip.len() - 1).min(data.szTip.len() - 1);
            data.szTip[..len].copy_from_slice(&tip[..len]);
            Shell_NotifyIconW(operation, &data).as_bool()
        }
    }

    unsafe fn create_buttons(&mut self, hwnd: HWND) {
        unsafe {
            for (id, label) in [
                (LISTEN, "Приостановить"),
                (RELOAD, "Перечитать настройки"),
                (CONFIG, "Настройки"),
                (LOG, "Журнал"),
                (RESYNC, "Сбросить оценку Discord"),
            ] {
                let caption = wide(label);
                if let Ok(button) = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("BUTTON"),
                    PCWSTR(caption.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                    0,
                    0,
                    1,
                    1,
                    Some(hwnd),
                    Some(HMENU(id as *mut _)),
                    None,
                    None,
                ) {
                    let _ = SendMessageW(
                        button,
                        WM_SETFONT,
                        Some(WPARAM(self.body_font.0 as usize)),
                        None,
                    );
                    self.buttons.push((id, button));
                }
            }
            self.layout(hwnd);
        }
    }

    unsafe fn layout(&self, hwnd: HWND) {
        unsafe {
            let mut bounds = RECT::default();
            let _ = GetClientRect(hwnd, &mut bounds);
            let width = bounds.right - bounds.left;
            let bottom = bounds.bottom - 90;
            let third = (width - 80) / 3;
            for (id, button) in &self.buttons {
                let (x, y, w, h) = match *id {
                    LISTEN => (28, bottom, 185, 40),
                    RELOAD => (225, bottom, width - 253, 40),
                    CONFIG => (28, bottom + 48, third, 30),
                    LOG => (40 + third, bottom + 48, third, 30),
                    RESYNC => (52 + third * 2, bottom + 48, third, 30),
                    _ => continue,
                };
                let _ = MoveWindow(*button, x, y, w, h, true);
            }
        }
    }

    unsafe fn update(&mut self, hwnd: HWND) {
        unsafe {
            if let Ok(state) = self.shared.lock() {
                let speaking = state.speaking && state.ready && !state.paused;
                if self.speaking != speaking {
                    self.speaking = speaking;
                    let small = if speaking {
                        self.speaking_icon
                    } else {
                        self.idle_icon
                    };
                    let large = if speaking {
                        self.speaking_window_icon
                    } else {
                        self.window_icon
                    };
                    let _ = SendMessageW(
                        hwnd,
                        WM_SETICON,
                        Some(WPARAM(ICON_SMALL as usize)),
                        Some(LPARAM(small.0 as isize)),
                    );
                    let _ = SendMessageW(
                        hwnd,
                        WM_SETICON,
                        Some(WPARAM(ICON_BIG as usize)),
                        Some(LPARAM(large.0 as isize)),
                    );
                }
                if self.last_paused != Some(state.paused) {
                    self.last_paused = Some(state.paused);
                    if let Some((_, button)) = self.buttons.iter().find(|(id, _)| *id == LISTEN) {
                        let text = wide(if state.paused {
                            "Продолжить"
                        } else {
                            "Приостановить"
                        });
                        let _ = SetWindowTextW(*button, PCWSTR(text.as_ptr()));
                    }
                }
            }
            self.tray(hwnd, NIM_MODIFY);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }

    unsafe fn menu(&self, hwnd: HWND) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let _ = AppendMenuW(menu, MF_STRING, SHOW, w!("Открыть Voice Control"));
            let paused = self.shared.lock().map(|s| s.paused).unwrap_or(false);
            let label = wide(if paused {
                "Продолжить прослушивание"
            } else {
                "Приостановить прослушивание"
            });
            let _ = AppendMenuW(menu, MF_STRING, LISTEN, PCWSTR(label.as_ptr()));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, CONFIG, w!("Настройки"));
            let _ = AppendMenuW(menu, MF_STRING, LOG, w!("Журнал"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, EXIT, w!("Выход"));
            let mut point = windows::Win32::Foundation::POINT::default();
            let _ = GetCursorPos(&mut point);
            let _ = SetForegroundWindow(hwnd);
            let choice = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                Some(0),
                hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            if choice.0 != 0 {
                self.command(hwnd, choice.0 as usize);
            }
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }

    unsafe fn command(&self, hwnd: HWND, id: usize) {
        unsafe {
            match id {
                LISTEN => self.control(Control::TogglePause),
                RELOAD => self.control(Control::Reload),
                RESYNC => self.control(Control::ResyncDiscord),
                CONFIG | LOG => {
                    let path = if id == CONFIG {
                        &self.config
                    } else {
                        &self.log
                    };
                    let file = wide(&path.to_string_lossy());
                    let result = ShellExecuteW(
                        Some(hwnd),
                        w!("open"),
                        PCWSTR(file.as_ptr()),
                        None,
                        None,
                        SW_SHOWNORMAL,
                    );
                    if result.0 as isize <= 32 {
                        if let Ok(mut state) = self.shared.lock() {
                            state.error = format!("Не удалось открыть {}", path.display());
                        }
                    }
                }
                SHOW => {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    let _ = SetForegroundWindow(hwnd);
                }
                EXIT => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
        }
    }

    unsafe fn paint(&self, hwnd: HWND) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let screen = BeginPaint(hwnd, &mut ps);
            let mut area = RECT::default();
            let _ = GetClientRect(hwnd, &mut area);
            let width = area.right;
            let height = area.bottom;
            let dc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, width.max(1), height.max(1));
            let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
            fill(dc, area, BG);
            let _ = SetBkMode(dc, TRANSPARENT);

            text(
                dc,
                self.title_font,
                TEXT,
                28,
                24,
                width - 56,
                40,
                "Голос под контролем",
            );
            text(
                dc,
                self.body_font,
                MUTED,
                29,
                67,
                width - 58,
                25,
                "Медиаплеер Windows и Discord. Четыре короткие команды.",
            );

            if let Ok(state) = self.shared.lock() {
                fill(dc, rect(28, 110, width - 28, 196), PANEL);
                let active = state.ready && !state.paused && state.error.is_empty();
                let indicator = if active {
                    ACCENT
                } else if !state.error.is_empty() {
                    WARNING
                } else {
                    MUTED
                };
                fill(dc, rect(44, 128, 50, 178), indicator);
                text(
                    dc,
                    self.heading_font,
                    TEXT,
                    65,
                    125,
                    width - 110,
                    28,
                    if state.speaking && state.ready && !state.paused {
                        "Слышу речь"
                    } else {
                        &state.status
                    },
                );
                let device = if state.microphone.is_empty() {
                    "Микрофон: подготовка устройства".into()
                } else {
                    format!("Микрофон: {}", state.microphone)
                };
                text(
                    dc,
                    self.small_font,
                    MUTED,
                    65,
                    161,
                    width - 110,
                    22,
                    &device,
                );

                text(
                    dc,
                    self.small_font,
                    MUTED,
                    28,
                    211,
                    135,
                    20,
                    "УРОВЕНЬ ГОЛОСА",
                );
                let bar_left = 176;
                let bar_width = (width - bar_left - 28).max(1);
                fill(dc, rect(bar_left, 217, width - 28, 224), PANEL_ALT);
                let level = state.level.min(100) as i32;
                if level > 0 && !state.paused {
                    fill(
                        dc,
                        rect(bar_left, 217, bar_left + bar_width * level / 100, 224),
                        ACCENT,
                    );
                }

                text(
                    dc,
                    self.small_font,
                    MUTED,
                    28,
                    247,
                    width - 56,
                    20,
                    "ПОСЛЕДНЯЯ КОМАНДА",
                );
                let phrase = if state.last_phrase.is_empty() {
                    "Скажите «играй», «пауза», «мут» или «размут»".into()
                } else {
                    format!("«{}»", state.last_phrase)
                };
                text(
                    dc,
                    self.heading_font,
                    TEXT,
                    28,
                    272,
                    width - 56,
                    29,
                    &phrase,
                );
                let action = if state.last_action.is_empty() {
                    "Результат появится здесь"
                } else {
                    &state.last_action
                };
                text(dc, self.small_font, MUTED, 29, 308, width - 58, 23, action);

                let top = 352;
                for (index, (phrase, action)) in [
                    ("играй", "Продолжить воспроизведение"),
                    ("пауза", "Остановить воспроизведение"),
                    ("мут", "Discord · Insert"),
                    ("размут", "Discord · Insert"),
                ]
                .iter()
                .enumerate()
                {
                    let y = top + index as i32 * 31;
                    text(dc, self.body_font, ACCENT, 29, y, 130, 25, phrase);
                    text(dc, self.body_font, MUTED, 166, y, width - 194, 25, action);
                }
                text(
                    dc,
                    self.small_font,
                    MUTED,
                    29,
                    top + 127,
                    width - 58,
                    21,
                    "Discord: оценка состояния по Insert, без подтверждения от приложения.",
                );

                let error_y = height - 132;
                if !state.error.is_empty() {
                    text(
                        dc,
                        self.small_font,
                        WARNING,
                        29,
                        error_y,
                        width - 58,
                        32,
                        &state.error,
                    );
                } else {
                    text(
                        dc,
                        self.small_font,
                        MUTED,
                        29,
                        error_y,
                        width - 58,
                        27,
                        "Закрытие окна оставляет прослушивание в трее. Выход — в меню значка.",
                    );
                }
            }

            let _ = BitBlt(screen, 0, 0, width, height, Some(dc), 0, 0, SRCCOPY);
            let _ = SelectObject(dc, old_bitmap);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(dc);
            let _ = EndPaint(hwnd, &ps);
        }
    }

    unsafe fn draw_button(&self, item: &DRAWITEMSTRUCT) {
        unsafe {
            let primary = item.CtlID as usize == LISTEN;
            let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
            let bg = if primary {
                if pressed { color(85, 184, 157) } else { ACCENT }
            } else if pressed {
                color(48, 61, 75)
            } else {
                PANEL_ALT
            };
            fill(item.hDC, item.rcItem, bg);
            let _ = SetBkMode(item.hDC, TRANSPARENT);
            let _ = SetTextColor(item.hDC, if primary { BG } else { TEXT });
            let old = SelectObject(item.hDC, HGDIOBJ(self.body_font.0));
            let mut label = [0u16; 160];
            let count = GetWindowTextW(item.hwndItem, &mut label);
            let mut bounds = item.rcItem;
            let _ = DrawTextW(
                item.hDC,
                &mut label[..count as usize],
                &mut bounds,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if item.itemState.0 & windows::Win32::UI::Controls::ODS_FOCUS.0 != 0 {
                let mut focus = item.rcItem;
                focus.left += 4;
                focus.top += 4;
                focus.right -= 4;
                focus.bottom -= 4;
                let _ = DrawFocusRect(item.hDC, &focus);
            }
            let _ = SelectObject(item.hDC, old);
        }
    }
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

unsafe fn fill(dc: HDC, bounds: RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let _ = FillRect(dc, &bounds, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

// Keep the Win32 drawing coordinates explicit at the layout call sites.
#[allow(clippy::too_many_arguments)]
unsafe fn text(
    dc: HDC,
    font: HFONT,
    color: COLORREF,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    value: &str,
) {
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let _ = SetTextColor(dc, color);
        let mut data: Vec<u16> = value.encode_utf16().collect();
        let mut bounds = rect(x, y, x + width, y + height);
        let _ = DrawTextW(
            dc,
            &mut data,
            &mut bounds,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        let _ = SelectObject(dc, old);
    }
}

unsafe fn font(height: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        )
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
        if !pointer.is_null() {
            let ui = &mut *pointer;
            if message == ui.taskbar_created && message != 0 {
                ui.tray_registered = ui.tray(hwnd, NIM_ADD);
                return LRESULT(0);
            }
            match message {
                WM_CREATE => {
                    let _ = SendMessageW(
                        hwnd,
                        WM_SETICON,
                        Some(WPARAM(ICON_SMALL as usize)),
                        Some(LPARAM(ui.idle_icon.0 as isize)),
                    );
                    let _ = SendMessageW(
                        hwnd,
                        WM_SETICON,
                        Some(WPARAM(ICON_BIG as usize)),
                        Some(LPARAM(ui.window_icon.0 as isize)),
                    );
                    ui.create_buttons(hwnd);
                    ui.tray_registered = ui.tray(hwnd, NIM_ADD);
                    SetTimer(Some(hwnd), TIMER, 150, None);
                    return LRESULT(0);
                }
                WM_TIMER => {
                    ui.update(hwnd);
                    return LRESULT(0);
                }
                WM_SIZE => {
                    ui.layout(hwnd);
                    return LRESULT(0);
                }
                WM_PAINT => {
                    ui.paint(hwnd);
                    return LRESULT(0);
                }
                WM_ERASEBKGND => return LRESULT(1),
                WM_DRAWITEM => {
                    ui.draw_button(&*(lparam.0 as *const DRAWITEMSTRUCT));
                    return LRESULT(1);
                }
                WM_COMMAND => {
                    ui.command(hwnd, wparam.0 & 0xffff);
                    return LRESULT(0);
                }
                WM_GETMINMAXINFO => {
                    let limits = &mut *(lparam.0 as *mut MINMAXINFO);
                    limits.ptMinTrackSize.x = 690;
                    limits.ptMinTrackSize.y = 720;
                    return LRESULT(0);
                }
                WM_CLOSE => {
                    if ui.tray_registered {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    } else {
                        // If Explorer could not host the icon, never leave an
                        // unreachable background listener after closing the window.
                        let _ = DestroyWindow(hwnd);
                    }
                    return LRESULT(0);
                }
                TRAY_MESSAGE => {
                    match lparam.0 as u32 {
                        WM_LBUTTONDBLCLK => ui.command(hwnd, SHOW),
                        WM_RBUTTONUP | WM_CONTEXTMENU => ui.menu(hwnd),
                        _ => {}
                    }
                    return LRESULT(0);
                }
                WM_DESTROY => {
                    let _ = KillTimer(Some(hwnd), TIMER);
                    ui.tray(hwnd, NIM_DELETE);
                    ui.control(Control::Quit);
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
                WM_NCDESTROY => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(pointer));
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

/// ICO files contain a directory of PNG resources. Select a size at least as
/// large as the requested icon, then let Windows decode and scale that resource.
unsafe fn icon_from_ico(bytes: &[u8], desired: i32) -> Result<HICON> {
    unsafe {
        anyhow::ensure!(
            bytes.len() >= 6 && bytes[..4] == [0, 0, 1, 0],
            "Некорректный файл значка"
        );
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        anyhow::ensure!(
            count > 0 && bytes.len() >= 6 + count * 16,
            "Нет изображений в значке"
        );
        let entry = (0..count)
            .map(|index| 6 + index * 16)
            .min_by_key(|entry| {
                let width = if bytes[*entry] == 0 {
                    256
                } else {
                    i32::from(bytes[*entry])
                };
                if width >= desired {
                    width - desired
                } else {
                    1_000 + desired - width
                }
            })
            .context("Нет подходящего размера значка")?;
        let length = u32::from_le_bytes(bytes[entry + 8..entry + 12].try_into()?) as usize;
        let offset = u32::from_le_bytes(bytes[entry + 12..entry + 16].try_into()?) as usize;
        let end = offset
            .checked_add(length)
            .context("Некорректная длина значка")?;
        let image = bytes
            .get(offset..end)
            .context("Повреждённые данные значка")?;
        CreateIconFromResourceEx(image, true, 0x0003_0000, desired, desired, LR_DEFAULTCOLOR)
            .context("Windows не смогла загрузить значок приложения")
    }
}

pub fn run(
    shared: Arc<Mutex<AppState>>,
    tx: Sender<Control>,
    config_path: PathBuf,
    log_path: PathBuf,
) -> Result<()> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let small_size = GetSystemMetrics(SM_CXSMICON).max(16);
        let idle_icon = icon_from_ico(include_bytes!("../assets/icon-idle.ico"), small_size)?;
        let speaking_icon =
            icon_from_ico(include_bytes!("../assets/icon-speaking.ico"), small_size)?;
        let window_icon = icon_from_ico(include_bytes!("../assets/icon-idle.ico"), 64)?;
        let speaking_window_icon =
            icon_from_ico(include_bytes!("../assets/icon-speaking.ico"), 64)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: window_icon,
            ..Default::default()
        };
        anyhow::ensure!(
            RegisterClassW(&class) != 0,
            "Не удалось зарегистрировать окно Voice Control"
        );
        let ui = Box::new(Ui {
            shared,
            tx,
            config: config_path,
            log: log_path,
            title_font: font(31, 600),
            heading_font: font(20, 600),
            body_font: font(15, 400),
            small_font: font(13, 400),
            buttons: Vec::new(),
            last_paused: None,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            idle_icon,
            speaking_icon,
            window_icon,
            speaking_window_icon,
            speaking: false,
            tray_registered: false,
        });
        let pointer = Box::into_raw(ui);
        let result = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            w!("Voice Control"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            720,
            750,
            None,
            None,
            Some(instance),
            Some(pointer.cast()),
        );
        let hwnd = result.context("Не удалось создать окно Voice Control")?;
        // Native dark caption on supported Windows versions; failure is harmless.
        #[link(name = "dwmapi")]
        unsafe extern "system" {
            fn DwmSetWindowAttribute(
                hwnd: HWND,
                attribute: u32,
                value: *const std::ffi::c_void,
                size: u32,
            ) -> i32;
        }
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            20,
            (&dark as *const i32).cast(),
            size_of::<i32>() as u32,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            if result.0 == -1 {
                anyhow::bail!("Ошибка очереди сообщений Windows");
            }
            if result.0 == 0 {
                break;
            }
            if !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}
