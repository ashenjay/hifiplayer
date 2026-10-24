//! Previous / play-pause / next buttons under the window's taskbar preview.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Duration;

use hifiplayer::player::{Command, Player, Status};
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HGDIOBJ,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList3, SetWindowSubclass, THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_ENABLED, THUMBBUTTON,
    TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, RegisterWindowMessageW, SM_CXSMICON, WM_COMMAND,
};
use windows::core::w;

const THBN_CLICKED: u32 = 0x1800;
const PREV: u32 = 1;
const TOGGLE: u32 = 2;
const NEXT: u32 = 3;

static PLAYER: OnceLock<Player> = OnceLock::new();

struct Icons {
    prev: HICON,
    play: HICON,
    pause: HICON,
    next: HICON,
}

thread_local! {
    // Main (UI) thread only: COM object and icons for this window.
    static TASKBAR: RefCell<Option<(ITaskbarList3, HWND, Icons, bool)>> = const { RefCell::new(None) };
}

/// Install the buttons on the main window; they follow the play state.
pub fn install(app: &AppHandle, player: Player) {
    let Some(window) = app.get_webview_window("main") else { return };
    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = HWND(hwnd.0);
    let _ = PLAYER.set(player.clone());
    let addr = hwnd.0 as isize;
    let _ = app.run_on_main_thread(move || unsafe { setup(HWND(addr as _)) });

    // Swap the play/pause icon when the state changes.
    let app = app.clone();
    std::thread::spawn(move || {
        let mut last = None;
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let playing = matches!(player.state().status, Status::Playing | Status::Loading);
            if last != Some(playing) {
                last = Some(playing);
                if app.run_on_main_thread(move || update(playing)).is_err() {
                    break;
                }
            }
        }
    });
}

unsafe fn setup(hwnd: HWND) {
    let Ok(list) = (unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) }) else {
        return;
    };
    let _ = unsafe { list.HrInit() };
    let icons = Icons { prev: icon(Shape::Prev), play: icon(Shape::Play), pause: icon(Shape::Pause), next: icon(Shape::Next) };
    TASKBAR.with(|t| *t.borrow_mut() = Some((list, hwnd, icons, false)));
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(subclass), 0x4846_5021, 0);
    }
    // Works if the taskbar button exists already; otherwise
    // "TaskbarButtonCreated" arrives once the window is shown.
    add_buttons();
}

fn buttons(icons: &Icons, playing: bool) -> [THUMBBUTTON; 3] {
    let make = |id, icon, tip: &str| {
        let mut b = THUMBBUTTON {
            dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
            iId: id,
            hIcon: icon,
            dwFlags: THBF_ENABLED,
            ..Default::default()
        };
        for (d, s) in b.szTip.iter_mut().zip(tip.encode_utf16()) {
            *d = s;
        }
        b
    };
    [
        make(PREV, icons.prev, "Previous"),
        if playing { make(TOGGLE, icons.pause, "Pause") } else { make(TOGGLE, icons.play, "Play") },
        make(NEXT, icons.next, "Next"),
    ]
}

fn add_buttons() {
    TASKBAR.with(|t| {
        if let Some((list, hwnd, icons, added)) = t.borrow_mut().as_mut() {
            let playing = PLAYER.get().is_some_and(|p| matches!(p.state().status, Status::Playing | Status::Loading));
            *added = unsafe { list.ThumbBarAddButtons(*hwnd, &buttons(icons, playing)) }.is_ok();
        }
    });
}

fn update(playing: bool) {
    TASKBAR.with(|t| {
        if let Some((list, hwnd, icons, true)) = t.borrow().as_ref() {
            let _ = unsafe { list.ThumbBarUpdateButtons(*hwnd, &buttons(icons, playing)) };
        }
    });
}

unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    static CREATED: OnceLock<u32> = OnceLock::new();
    let created = *CREATED.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) });
    if msg == created {
        // Also sent again after Explorer restarts.
        add_buttons();
    } else if msg == WM_COMMAND && (wparam.0 >> 16) as u32 == THBN_CLICKED {
        if let Some(p) = PLAYER.get() {
            match (wparam.0 & 0xFFFF) as u32 {
                PREV => p.send(Command::Previous),
                TOGGLE => p.send(Command::TogglePause),
                NEXT => p.send(Command::Next),
                _ => {}
            }
        }
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[derive(Clone, Copy)]
enum Shape {
    Prev,
    Play,
    Pause,
    Next,
}

/// Point-in-shape test in unit coordinates (0..1).
fn inside(shape: Shape, x: f32, y: f32) -> bool {
    let tri = |ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32| {
        let s = |px: f32, py: f32, qx: f32, qy: f32| (x - qx) * (py - qy) - (px - qx) * (y - qy);
        let (d1, d2, d3) = (s(ax, ay, bx, by), s(bx, by, cx, cy), s(cx, cy, ax, ay));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    let rect = |x0: f32, x1: f32| x >= x0 && x <= x1 && (0.2..=0.8).contains(&y);
    match shape {
        Shape::Play => tri(0.3, 0.18, 0.3, 0.82, 0.82, 0.5),
        Shape::Pause => rect(0.26, 0.42) || rect(0.58, 0.74),
        Shape::Prev => rect(0.2, 0.3) || tri(0.8, 0.2, 0.8, 0.8, 0.32, 0.5),
        Shape::Next => rect(0.7, 0.8) || tri(0.2, 0.2, 0.2, 0.8, 0.68, 0.5),
    }
}

/// A white anti-aliased icon at the small-icon size.
fn icon(shape: Shape) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let mut bits = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else { return HICON::default() };
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        const SS: i32 = 4;
        for py in 0..size {
            for px in 0..size {
                let mut hits = 0;
                for sy in 0..SS {
                    for sx in 0..SS {
                        let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                        let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                        hits += inside(shape, x, y) as u32;
                    }
                }
                let a = hits * 255 / (SS * SS) as u32;
                pixels[(py * size + px) as usize] = (a << 24) | 0x00FF_FFFF;
            }
        }
        let mask = CreateBitmap(size, size, 1, 1, None);
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}
