// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ẩn cửa sổ khỏi taskbar Windows (tray-only). Slint/winit không có API sẵn nên
//! phải xuống Win32: đổi extended style sang WS_EX_TOOLWINDOW. CreateWindow đã tạo
//! taskbar button rồi nên phải hide→đổi style→show để nút biến mất (MSDN).
#[cfg(windows)]
pub fn an_khoi_taskbar(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE,
        WS_EX_TOOLWINDOW, WS_EX_APPWINDOW, SW_HIDE, SW_SHOW,
    };
    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        let mut ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        ex |= WS_EX_TOOLWINDOW.0 as isize;
        ex &= !(WS_EX_APPWINDOW.0 as isize);
        let _ = ShowWindow(h, SW_HIDE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex);
        let _ = ShowWindow(h, SW_SHOW);
    }
}

#[cfg(not(windows))]
pub fn an_khoi_taskbar(_hwnd: isize) {}
