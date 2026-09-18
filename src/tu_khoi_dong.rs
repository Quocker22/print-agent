// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tự khởi động cùng Windows — ghi/xoá giá trị trong khoá registry
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! VÌ SAO CẦN (sự việc thật 15–18/09): máy in HCM im lặng **hơn 3 ngày**. Không
//! phải app crash, không phải mạng — app chỉ đơn giản KHÔNG BAO GIỜ được bật lại
//! sau khi máy tắt/khởi động lại. Suốt thời gian đó bot vẫn nhận lệnh in, vẫn
//! nói "đã xếp hàng in", job chết lặng sau 5 phút. Người phát hiện đầu tiên là
//! KHÁCH. Không ai ở shop có nhiệm vụ "nhớ bật app mỗi sáng".
//!
//! VÌ SAO HKCU\...\Run chứ không phải Windows service:
//! bản này là app tray-only có UI Slint (`main.rs:98` gọi `ui::chay_ui`), phải
//! chạy TRONG PHIÊN NGƯỜI DÙNG mới có tray icon và mới in được qua driver
//! Windows. Service chạy ở session 0, không thấy tray, và in qua driver từ
//! session 0 là đường đầy bẫy. README cũ hướng dẫn dùng `nssm` — đó là cho bản
//! Rust CŨ chưa có UI, nay KHÔNG còn đúng.
//!
//! VÌ SAO HKCU chứ không HKLM: HKCU không cần quyền admin. NV ở shop chạy bằng
//! tài khoản thường; đòi admin là thêm một bước có thể hỏng.

#[cfg(windows)]
const KHOA_RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Tên giá trị trong khoá Run. Cố định để lần ghi sau ghi đè lần trước, không
/// đẻ ra nhiều mục rác khi người dùng copy exe sang chỗ mới.
#[cfg(windows)]
const TEN_GIA_TRI: &str = "IncokitPrintAgent";

/// Dòng lệnh đưa vào registry: đường dẫn exe + config.ini, mỗi cái trong ngoặc
/// kép (đường dẫn Windows hay có khoảng trắng — "Program Files", tên người dùng
/// có dấu cách). Không có ngoặc kép thì Windows cắt sai ở khoảng trắng đầu tiên
/// và app không bao giờ chạy — hỏng âm thầm, không báo gì.
#[cfg(windows)]
fn dong_lenh() -> std::io::Result<String> {
    let exe = std::env::current_exe()?;
    // config.ini nằm CẠNH exe (xem ui.rs: nút Lưu tự ghi ra đó), không phải
    // thư mục làm việc hiện tại — lúc Windows tự chạy app, thư mục làm việc là
    // C:\Windows\system32, tìm config.ini ở đó thì không thấy.
    let cfg = exe.with_file_name("config.ini");
    Ok(format!("\"{}\" \"{}\"", exe.display(), cfg.display()))
}

/// Có đang bật tự khởi động không.
///
/// Trả `false` khi không đọc được registry — không đoán bừa là "đang bật", vì
/// hiển thị sai chiều khiến người dùng tưởng đã bật rồi và không bấm nữa.
#[cfg(windows)]
pub fn dang_bat() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(run) = hkcu.open_subkey(KHOA_RUN) else {
        return false;
    };
    let Ok(gia_tri) = run.get_value::<String, _>(TEN_GIA_TRI) else {
        return false;
    };
    // Chỉ coi là "đang bật" khi registry trỏ đúng exe ĐANG CHẠY. Người dùng có
    // thể đã copy exe sang thư mục mới; mục cũ vẫn còn nhưng trỏ file đã xoá —
    // báo "đang bật" lúc đó là nói dối, app sẽ không lên sau reboot.
    match std::env::current_exe() {
        Ok(exe) => gia_tri.contains(&exe.display().to_string()),
        Err(_) => false,
    }
}

/// Bật/tắt tự khởi động. Trả lỗi kèm lý do để UI hiện được cho người dùng —
/// KHÔNG nuốt lặng: người bấm nút mà không thấy gì đổi sẽ tưởng đã xong.
#[cfg(windows)]
pub fn dat(bat: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = hkcu
        .open_subkey_with_flags(KHOA_RUN, KEY_SET_VALUE)
        .context("mở khoá registry Run")?;

    if bat {
        let lenh = dong_lenh().context("dựng dòng lệnh khởi động")?;
        run.set_value(TEN_GIA_TRI, &lenh).context("ghi registry Run")?;
    } else {
        // Xoá mục không tồn tại KHÔNG phải lỗi — người dùng bấm tắt khi vốn
        // đã tắt thì kết quả mong muốn (không tự chạy) đã đạt được.
        match run.delete_value(TEN_GIA_TRI) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow::Error::new(e).context("xoá registry Run")),
        }
    }
    Ok(())
}

// --- Bản giả cho không-Windows: để build/test được trên Mac/CI ---

#[cfg(not(windows))]
pub fn dang_bat() -> bool {
    false
}

#[cfg(not(windows))]
pub fn dat(_bat: bool) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Dòng lệnh PHẢI có ngoặc kép quanh cả exe lẫn config.
    /// Đường dẫn Windows hay chứa khoảng trắng ("Program Files", tên người dùng
    /// có dấu cách). Thiếu ngoặc kép thì Windows cắt sai ở khoảng trắng đầu và
    /// app KHÔNG BAO GIỜ chạy sau reboot — hỏng âm thầm, đúng kiểu lỗi đã làm
    /// máy HCM im 3 ngày mà không ai biết.
    #[test]
    fn dong_lenh_co_ngoac_kep_de_chiu_duoc_duong_dan_co_khoang_trang() {
        let lenh = dong_lenh().expect("dựng được dòng lệnh");
        assert!(lenh.starts_with('"'), "exe phải trong ngoặc kép: {lenh}");
        assert_eq!(lenh.matches('"').count(), 4, "đúng 2 cặp ngoặc kép: {lenh}");
        assert!(lenh.contains("config.ini"), "phải truyền config.ini: {lenh}");
    }

    /// config.ini phải CẠNH exe, không phải thư mục làm việc hiện tại.
    /// Lúc Windows tự chạy app, thư mục làm việc là C:\Windows\system32 —
    /// tìm config.ini ở đó thì không thấy và app mở ra rỗng.
    #[test]
    fn config_lay_canh_exe_khong_phai_thu_muc_lam_viec() {
        let lenh = dong_lenh().expect("dựng được dòng lệnh");
        let exe = std::env::current_exe().expect("có đường dẫn exe");
        let thu_muc = exe.parent().expect("exe có thư mục cha");
        assert!(
            lenh.contains(&thu_muc.display().to_string()),
            "config.ini phải nằm cạnh exe ({}), lệnh: {lenh}",
            thu_muc.display()
        );
    }
}
