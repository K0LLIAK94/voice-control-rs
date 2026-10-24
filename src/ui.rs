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
            HiDpi::*,
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL},
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};

use crate::app::{AppState, Control};
#[path = "ui_scale.rs"]
mod ui_scale;
use ui_scale::Scale;

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
const ZOOM_IN: usize = 203;
const ZOOM_OUT: usize = 204;
const ZOOM_RESET: usize = 205;

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

unsafe fn work_area(hwnd: HWND, suggested: Option<RECT>) -> RECT {
    unsafe {
        let monitor = match suggested {
            Some(bounds) => MonitorFromRect(&bounds, MONITOR_DEFAULTTONEAREST),
            None => MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
        };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            rect(
                0,
                0,
                GetSystemMetrics(SM_CXSCREEN).max(1),
                GetSystemMetrics(SM_CYSCREEN).max(1),
            )
        }
    }
}

unsafe fn zoom_key(message: &MSG) -> Option<usize> {
    unsafe {
        if message.message != WM_KEYDOWN
            || GetAsyncKeyState(i32::from(VK_CONTROL.0)) >= 0
            || GetAsyncKeyState(0x12) < 0
        {
            return None;
        }
        match message.wParam.0 {
            0x6b | 0xbb => Some(ZOOM_IN), // numpad + and OEM + (Ctrl+= also works)
            0x6d | 0xbd => Some(ZOOM_OUT),
            0x30 | 0x60 => Some(ZOOM_RESET),
            _ => None,
        }
    }
}

struct Ui {
    scale: Scale,
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
    unsafe fn scaled_rect(&self, bounds: RECT) -> RECT {
        rect(
            self.scale.px(bounds.left),
            self.scale.px(bounds.top),
            self.scale.px(bounds.right),
            self.scale.px(bounds.bottom),
        )
    }

    unsafe fn fill(&self, dc: HDC, bounds: RECT, color: COLORREF) {
        unsafe {
            fill(dc, self.scaled_rect(bounds), color);
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn text(
        &self,
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
            text(
                dc,
                font,
                color,
                self.scale.px(x),
                self.scale.px(y),
                self.scale.px(width),
                self.scale.px(height),
                value,
            );
        }
    }

    unsafe fn wrapped_text(
        &self,
        dc: HDC,
        font: HFONT,
        color: COLORREF,
        bounds: RECT,
        value: &str,
    ) {
        unsafe {
            wrapped_text(dc, font, color, self.scaled_rect(bounds), value);
        }
    }

    unsafe fn refresh_fonts(&mut self) {
        unsafe {
            let old = [
                self.title_font,
                self.heading_font,
                self.body_font,
                self.small_font,
            ];
            self.title_font = font(self.scale.px(31).max(1), 600);
            self.heading_font = font(self.scale.px(20).max(1), 600);
            self.body_font = font(self.scale.px(15).max(1), 400);
            self.small_font = font(self.scale.px(13).max(1), 400);
            for (_, button) in &self.buttons {
                let _ = SendMessageW(
                    *button,
                    WM_SETFONT,
                    Some(WPARAM(self.body_font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            for handle in old {
                let _ = DeleteObject(HGDIOBJ(handle.0));
            }
        }
    }

    unsafe fn refresh_icons(&mut self, hwnd: HWND) {
        unsafe {
            let small = GetSystemMetricsForDpi(SM_CXSMICON, self.scale.dpi).max(16);
            let large = GetSystemMetricsForDpi(SM_CXICON, self.scale.dpi).max(32);
            let icons = (|| -> Result<_> {
                Ok([
                    icon_from_ico(include_bytes!("../assets/icon-idle.ico"), small)?,
                    icon_from_ico(include_bytes!("../assets/icon-speaking.ico"), small)?,
                    icon_from_ico(include_bytes!("../assets/icon-idle.ico"), large)?,
                    icon_from_ico(include_bytes!("../assets/icon-speaking.ico"), large)?,
                ])
            })();
            if let Ok(icons) = icons {
                let old = [
                    self.idle_icon,
                    self.speaking_icon,
                    self.window_icon,
                    self.speaking_window_icon,
                ];
                [
                    self.idle_icon,
                    self.speaking_icon,
                    self.window_icon,
                    self.speaking_window_icon,
                ] = icons;
                let small = if self.speaking {
                    self.speaking_icon
                } else {
                    self.idle_icon
                };
                let large = if self.speaking {
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
                SetClassLongPtrW(hwnd, GCLP_HICON, large.0 as isize);
                // Shell must switch to the new handles before their predecessors are freed.
                self.tray(hwnd, NIM_MODIFY);
                for handle in old {
                    let _ = DestroyIcon(handle);
                }
            }
        }
    }

    unsafe fn apply_scale(
        &mut self,
        hwnd: HWND,
        dpi: u32,
        requested: u32,
        suggested: Option<RECT>,
        initial: bool,
    ) {
        unsafe {
            let old = self.scale;
            let mut bounds = RECT::default();
            let _ = GetWindowRect(hwnd, &mut bounds);
            let work = work_area(hwnd, suggested);
            self.scale = Scale::new(dpi, requested);
            self.scale
                .fit(work.right - work.left, work.bottom - work.top);
            self.refresh_fonts();
            if old.dpi != self.scale.dpi {
                self.refresh_icons(hwnd);
            }
            let logical_width = if initial {
                ui_scale::WIDTH
            } else {
                old.logical(bounds.right - bounds.left)
                    .max(ui_scale::MIN_WIDTH)
            };
            let logical_height = if initial {
                ui_scale::HEIGHT
            } else {
                old.logical(bounds.bottom - bounds.top)
                    .max(ui_scale::MIN_HEIGHT)
            };
            let width = self.scale.px(logical_width).min(work.right - work.left);
            let height = self.scale.px(logical_height).min(work.bottom - work.top);
            let position = suggested.unwrap_or(bounds);
            let left = position.left.clamp(work.left, work.right - width);
            let top = position.top.clamp(work.top, work.bottom - height);
            if IsZoomed(hwnd).as_bool() {
                // The old maximized monitor's logical dimensions are not a
                // restored window size. Use Windows' new maximized placement.
                if let Some(target) = suggested {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        target.left,
                        target.top,
                        target.right - target.left,
                        target.bottom - target.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            } else {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    left,
                    top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            self.layout(hwnd);
            let title = wide(&format!("Voice Control — {}", self.scale.label()));
            let _ = SetWindowTextW(hwnd, PCWSTR(title.as_ptr()));
            let _ = InvalidateRect(Some(hwnd), None, true);
            for (_, button) in &self.buttons {
                let _ = InvalidateRect(Some(*button), None, true);
            }
        }
    }

    unsafe fn zoom(&mut self, hwnd: HWND, id: usize) {
        unsafe {
            let base = self.scale.effective.min(self.scale.requested);
            let requested = match id {
                ZOOM_IN => (base + 25).min(200),
                ZOOM_OUT => base.saturating_sub(25).max(75),
                _ => 100,
            };
            self.apply_scale(hwnd, self.scale.dpi, requested, None, false);
            if let Err(error) = crate::config::save_ui_scale(&self.config, requested) {
                if let Ok(mut state) = self.shared.lock() {
                    state.error = format!("Не удалось сохранить масштаб: {error}");
                }
            }
        }
    }

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
            let width = self.scale.logical(bounds.right - bounds.left);
            let bottom = self.scale.logical(bounds.bottom) - 90;
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
                let _ = MoveWindow(
                    *button,
                    self.scale.px(x),
                    self.scale.px(y),
                    self.scale.px(w),
                    self.scale.px(h),
                    true,
                );
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

    unsafe fn menu(&mut self, hwnd: HWND) {
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
            let current = wide(&self.scale.label());
            let _ = AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, PCWSTR(current.as_ptr()));
            let _ = AppendMenuW(menu, MF_STRING, ZOOM_IN, w!("Крупнее\tCtrl + плюс"));
            let _ = AppendMenuW(menu, MF_STRING, ZOOM_OUT, w!("Мельче\tCtrl + минус"));
            let _ = AppendMenuW(menu, MF_STRING, ZOOM_RESET, w!("Масштаб 100%\tCtrl + 0"));
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

    unsafe fn command(&mut self, hwnd: HWND, id: usize) {
        unsafe {
            match id {
                LISTEN => self.control(Control::TogglePause),
                RELOAD => self.control(Control::Reload),
                RESYNC => self.control(Control::ResyncDiscord),
                ZOOM_IN | ZOOM_OUT | ZOOM_RESET => self.zoom(hwnd, id),
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
            let width = self.scale.logical(area.right);
            let height = self.scale.logical(area.bottom);
            let dc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, area.right.max(1), area.bottom.max(1));
            let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
            fill(dc, area, BG);
            let _ = SetBkMode(dc, TRANSPARENT);

            self.text(
                dc,
                self.title_font,
                TEXT,
                28,
                24,
                width - 56,
                40,
                "Голос под контролем",
            );
            self.text(
                dc,
                self.body_font,
                MUTED,
                29,
                67,
                width - 58,
                25,
                "Медиаплеер Windows и Discord. Четыре короткие команды.",
            );

            let zoom = format!("{} · Ctrl + / − / 0", self.scale.label());
            self.text(dc, self.small_font, MUTED, 29, 91, width - 58, 16, &zoom);

            if let Ok(state) = self.shared.lock() {
                self.fill(dc, rect(28, 110, width - 28, 196), PANEL);
                let active = state.ready && !state.paused && state.error.is_empty();
                let indicator = if active {
                    ACCENT
                } else if !state.error.is_empty() {
                    WARNING
                } else {
                    MUTED
                };
                self.fill(dc, rect(44, 128, 50, 178), indicator);
                self.text(
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
                self.text(
                    dc,
                    self.small_font,
                    MUTED,
                    65,
                    161,
                    width - 110,
                    22,
                    &device,
                );

                self.text(
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
                self.fill(dc, rect(bar_left, 217, width - 28, 224), PANEL_ALT);
                let level = state.level.min(100) as i32;
                if level > 0 && !state.paused {
                    self.fill(
                        dc,
                        rect(bar_left, 217, bar_left + bar_width * level / 100, 224),
                        ACCENT,
                    );
                }

                self.text(
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
                self.text(
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
                // Leave room for two complete result lines before the command
                // list; long status text must not disappear on one clipped line.
                self.wrapped_text(
                    dc,
                    self.small_font,
                    MUTED,
                    rect(29, 308, width - 29, 346),
                    action,
                );

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
                    self.text(dc, self.body_font, ACCENT, 29, y, 130, 25, phrase);
                    self.text(dc, self.body_font, MUTED, 166, y, width - 194, 25, action);
                }
                self.text(
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
                    self.text(
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
                    self.text(
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

            let _ = BitBlt(
                screen,
                0,
                0,
                area.right,
                area.bottom,
                Some(dc),
                0,
                0,
                SRCCOPY,
            );
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
                focus.left += self.scale.px(4);
                focus.top += self.scale.px(4);
                focus.right -= self.scale.px(4);
                focus.bottom -= self.scale.px(4);
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

unsafe fn wrapped_text(dc: HDC, font: HFONT, color: COLORREF, mut bounds: RECT, value: &str) {
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let _ = SetTextColor(dc, color);
        let mut data: Vec<u16> = value.encode_utf16().collect();
        let _ = DrawTextW(
            dc,
            &mut data,
            &mut bounds,
            DT_LEFT | DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS | DT_NOPREFIX,
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
                WM_DPICHANGED => {
                    let suggested = *(lparam.0 as *const RECT);
                    let dpi = (wparam.0 & 0xffff) as u32;
                    ui.apply_scale(hwnd, dpi, ui.scale.requested, Some(suggested), false);
                    return LRESULT(0);
                }
                WM_DISPLAYCHANGE => {
                    ui.apply_scale(hwnd, GetDpiForWindow(hwnd), ui.scale.requested, None, false);
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
                    limits.ptMinTrackSize.x = ui.scale.px(ui_scale::MIN_WIDTH);
                    limits.ptMinTrackSize.y = ui.scale.px(ui_scale::MIN_HEIGHT);
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
    initial_scale_percent: u32,
) -> Result<()> {
    unsafe {
        // The manifest establishes this before startup; these calls also cover
        // a host that loads the library without the application's manifest.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut scale = Scale::new(GetDpiForSystem(), initial_scale_percent);
        let work = work_area(HWND::default(), None);
        scale.fit(work.right - work.left, work.bottom - work.top);
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let small_size = GetSystemMetricsForDpi(SM_CXSMICON, scale.dpi).max(16);
        let idle_icon = icon_from_ico(include_bytes!("../assets/icon-idle.ico"), small_size)?;
        let speaking_icon =
            icon_from_ico(include_bytes!("../assets/icon-speaking.ico"), small_size)?;
        let large_size = GetSystemMetricsForDpi(SM_CXICON, scale.dpi).max(32);
        let window_icon = icon_from_ico(include_bytes!("../assets/icon-idle.ico"), large_size)?;
        let speaking_window_icon =
            icon_from_ico(include_bytes!("../assets/icon-speaking.ico"), large_size)?;
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
            scale,
            shared,
            tx,
            config: config_path,
            log: log_path,
            title_font: font(scale.px(31), 600),
            heading_font: font(scale.px(20), 600),
            body_font: font(scale.px(15), 400),
            small_font: font(scale.px(13), 400),
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
            scale.px(ui_scale::WIDTH),
            scale.px(ui_scale::HEIGHT),
            None,
            None,
            Some(instance),
            Some(pointer.cast()),
        );
        let hwnd = result.context("Не удалось создать окно Voice Control")?;
        (*pointer).apply_scale(
            hwnd,
            GetDpiForWindow(hwnd),
            initial_scale_percent,
            None,
            true,
        );
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
            // Intercept before IsDialogMessageW so zoom also works while a
            // native child button has keyboard focus.
            if let Some(command) = zoom_key(&message) {
                let ui = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui;
                if !ui.is_null() {
                    (*ui).zoom(hwnd, command);
                }
                continue;
            }
            if !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod dpi_text_tests {
    use super::*;

    #[test]
    fn wrapped_result_fits_two_real_gdi_lines_at_high_dpi_and_zoom() {
        unsafe {
            let dc = CreateCompatibleDC(None);
            assert!(!dc.0.is_null());
            for (dpi, zoom) in [(96, 75), (96, 100), (192, 125), (288, 100)] {
                let scale = Scale::new(dpi, zoom);
                let font = font(scale.px(13), 400);
                let old = SelectObject(dc, HGDIOBJ(font.0));
                let mut message: Vec<u16> = "Insert отправлен. Discord должен обработать настроенную горячую клавишу. Состояние микрофона: оценка."
                    .encode_utf16().collect();
                let mut measured = rect(0, 0, scale.px(ui_scale::MIN_WIDTH - 58), 0);
                let height = DrawTextW(
                    dc,
                    &mut message,
                    &mut measured,
                    DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                );
                assert!(
                    height > 0 && measured.bottom <= scale.px(38),
                    "Result text clipped at DPI={dpi}, zoom={zoom}"
                );
                let _ = SelectObject(dc, old);
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
            let _ = DeleteDC(dc);
        }
    }
}
