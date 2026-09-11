// SPDX-License-Identifier: AGPL-3.0-or-later
// Tray-only: KHÔNG mở cửa sổ console đen khi chạy trên Windows (NV thấy console
// tưởng lỗi/tò mò tắt → hỏng in). windows_subsystem=windows bỏ console; log vẫn
// ghi được nhưng không hiện terminal. Chỉ áp release Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Print agent: nối ZaloCRM qua socket.io (namespace /print-agent), nhận event
//! "job", in qua driver Windows, emit "ket-qua". Tự reconnect khi mất kết nối.
//! Có UI Slint + tray icon (kiểu Tailscale: chạy ẩn ở khay hệ thống).
//!
//! Giao thức CHỐT (khớp backend/src/modules/ai/may-in/agent-ws.ts):
//!   - namespace "/print-agent", auth {token} (server tra token → máy + chi nhánh)
//!   - server→agent event "job": {loai:"in", job:{id,pdfBase64,paperSize,tray,copies}}
//!   - agent→server event "ket-qua": {jobId, trangThai:"da_in"|"loi", loiCuoi?}

mod config;
mod job;
mod net;
mod printing;
mod spooler;
mod state;
mod ui;
mod taskbar_win;
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

    // Chỉ spawn thread net nếu config hợp lệ — tránh chay_net cố nối server
    // rỗng (server_url="") ngay từ đầu, gây log lỗi rối mắt trước khi người
    // dùng kịp nhập gì.
    if hop_le {
        let cfg_net = cfg.clone();
        let trang_thai_net = trang_thai.clone();
        std::thread::spawn(move || net::chay_net(cfg_net, trang_thai_net));
    }

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
    ui::chay_ui(cfg, trang_thai)?;
    Ok(())
}
