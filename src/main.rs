// SPDX-License-Identifier: AGPL-3.0-or-later
// Tray-only: KHÔNG mở cửa sổ console đen khi chạy trên Windows (NV thấy console
// tưởng lỗi/tò mò tắt → hỏng in). windows_subsystem=windows bỏ console; log vẫn
// ghi được nhưng không hiện terminal. Chỉ áp release Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Print agent: nối ZaloCRM qua socket.io (namespace /print-agent), nhận event
//! "job", in qua driver Windows, emit "ket-qua". Tự reconnect khi mất kết nối.
//! Có UI Slint + tray icon (kiểu Tailscale: chạy ẩn ở khay hệ thống).
//!
//! Giao thức: xem đầu net.rs (hợp đồng v2 — `cau-hinh`, `su-co`,
//! `trang-thai-may-in`, `thong-tin-app`, `ket-qua` có `khong_ro`/`loai`).

mod bao_cao;
mod config;
mod hop_thu_di;
mod job;
mod mot_ban;
mod net;
mod nhat_ky;
mod printing;
mod spooler;
mod state;
mod su_co;
mod theo_doi_tiep;
mod thoi_gian;
mod ui;
mod taskbar_win;
mod tu_khoi_dong;
mod usb_may_in;
mod view_model;

use anyhow::Result;
use state::TrangThaiChung;
use std::sync::{Arc, Mutex};

/// Config rỗng dùng khi CHƯA CÓ config.ini hợp lệ — buộc mở UI ở tab Cấu hình
/// để người dùng tự nhập, thay vì agent chết ngay lúc khởi động (bản CLI cũ).
fn config_rong() -> config::Config {
    config::Config {
        server_url: String::new(),
        token: String::new(),
        printer_name: String::new(),
        tray: "tray-1".to_string(),
        paper_size: "A5".to_string(),
    }
}

/// Đường dẫn config.ini: ưu tiên tham số dòng lệnh; nếu không có thì tìm
/// config.ini CẠNH exe (không phải thư mục làm việc — double-click từ Explorer
/// có cwd khác nơi để exe, khiến "config.ini" tương đối không thấy file). Fallback
/// "config.ini" tương đối nếu không lấy được đường dẫn exe.
fn duong_dan_config() -> String {
    if let Some(arg) = std::env::args().nth(1) {
        return arg;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("config.ini").to_string_lossy().into_owned();
        }
    }
    "config.ini".to_string()
}

fn main() -> Result<()> {
    // Một máy một bản app (R7c): bản thứ hai (tự khởi động + NV bấm đúp) báo
    // ngắn rồi thoát, không mở kết nối thứ hai cùng token.
    let _khoa_mot_ban = match mot_ban::giu_mot_ban() {
        Ok(k) => k,
        Err(o_dau) => {
            mot_ban::bao_da_chay(o_dau);
            return Ok(());
        }
    };

    let config_path = duong_dan_config();

    // Đọc config nếu có; KHÔNG bail như bản CLI cũ — nếu thiếu/lỗi, mở UI với
    // config rỗng ở tab Cấu hình để người dùng tự nhập rồi bấm Lưu.
    let (cfg, hop_le) = match std::fs::read_to_string(&config_path) {
        Ok(text) => match config::parse_config(&text) {
            Ok(c) => (c, true),
            Err(e) => {
                eprintln!("[print-agent] config.ini lỗi ({}) — mở cửa sổ để sửa", e);
                (config_rong(), false)
            }
        },
        Err(_) => {
            eprintln!("[print-agent] chưa có {} — mở cửa sổ để nhập cấu hình", config_path);
            (config_rong(), false)
        }
    };

    let cfg = Arc::new(cfg);
    let trang_thai = Arc::new(Mutex::new(TrangThaiChung::default()));
    // Đường gửi lên backend dùng chung cho mọi lần chạy thread net (R4/R7):
    // kết quả job in dở lúc bấm Lưu vẫn đi qua kết nối mới.
    let duong_gui = Arc::new(hop_thu_di::DuongGui::default());

    // Chỉ spawn thread net nếu config hợp lệ — tránh chay_net cố nối server
    // rỗng (server_url="") ngay từ đầu, gây log lỗi rối mắt trước khi người
    // dùng kịp nhập gì.
    let net_dang_chay = hop_le.then(|| net::khoi_chay(cfg.clone(), trang_thai.clone(), duong_gui.clone(), None));

    // Mô hình MỚI (kiểu Tailscale): cửa sổ LUÔN ẨN lúc khởi động, kể cả khi
    // CHƯA có config hợp lệ — icon tray là điểm vào duy nhất, người dùng tự
    // bấm "Cấu hình..." trong menu tray để mở cửa sổ nhập. Khác bản trước
    // (hiện ngay nếu thiếu config): giữ đúng "khởi động chỉ có icon khay,
    // cửa sổ ẩn" theo yêu cầu, kể cả lần chạy đầu tiên chưa có config.ini.
    //
    // chay_ui() (Slint) trả Result<(), slint::PlatformError> — main() trả
    // anyhow::Result<()>. slint::PlatformError impl std::error::Error (feature
    // "std", đã bật mặc định) nên anyhow's blanket `impl From<E: Error+Send+
    // Sync+'static> for anyhow::Error` cho `?` chuyển thẳng, không cần map tay.
    ui::chay_ui(cfg, trang_thai, duong_gui, net_dang_chay)?;
    Ok(())
}
