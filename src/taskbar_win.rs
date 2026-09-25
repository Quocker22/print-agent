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

/// Nháy cửa sổ cho NV để ý khi máy in có sự cố (hợp đồng v2 §4.4).
///
/// Cửa sổ là WS_EX_TOOLWINDOW (không có nút taskbar — xem `an_khoi_taskbar`)
/// nên phần FLASHW_TRAY không có gì để nháy; cái NV thấy là THANH TIÊU ĐỀ nháy
/// (FLASHW_CAPTION), kéo dài tới khi cửa sổ được đưa lên trước (TIMERNOFG).
/// Windows không cho app chạy nền giành focus, nên nháy là cách được phép để
/// "gọi" người dùng.
#[cfg(windows)]
pub fn nhay_cua_so(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, FLASHWINFO, FLASHWINFO_FLAGS, FLASHW_ALL, FLASHW_TIMERNOFG,
    };
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd: HWND(hwnd as *mut core::ffi::c_void),
        dwFlags: FLASHWINFO_FLAGS(FLASHW_ALL.0 | FLASHW_TIMERNOFG.0),
        uCount: 0,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

#[cfg(not(windows))]
pub fn nhay_cua_so(_hwnd: isize) {}
