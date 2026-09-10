// SPDX-License-Identifier: AGPL-3.0-or-later
//! Print agent: nối ZaloCRM qua socket.io (namespace /print-agent), nhận event
//! "job", in qua driver Windows, emit "ket-qua". Tự reconnect khi mất kết nối.
//! Có UI egui + tray icon (kiểu Tailscale: chạy ẩn ở khay hệ thống).
//!
//! Giao thức CHỐT (khớp backend/src/modules/ai/may-in/agent-ws.ts):
//!   - namespace "/print-agent", auth {token, orgId}
//!   - server→agent event "job": {loai:"in", job:{id,pdfBase64,paperSize,tray,copies}}
//!   - agent→server event "ket-qua": {jobId, trangThai:"da_in"|"loi", loiCuoi?}

mod config;
mod job;
mod net;
mod printing;
mod state;
// TODO(task-5 ui-slint): ui.rs vẫn là egui/eframe cũ — không compile được
// sau khi Task 2 đổi deps sang Slint (Cargo.toml đã bỏ egui/eframe). Việc
// viết lại ui.rs bằng Slint (dùng MainWindow sinh từ ui/print-agent.slint)
// thuộc phạm vi Task 5, không phải Task 2. Tạm bỏ `mod ui;` + lời gọi
// ui::chay_ui() (thay bằng no-op) để `cargo check` pass cho Task 2 mà
// KHÔNG đụng nội dung ui.rs — Task 5 bật lại module này khi viết lại.
// mod ui;
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
        org_id: String::new(),
        printer_name: String::new(),
        tray: "tray-1".to_string(),
        paper_size: "A5".to_string(),
    }
}

fn main() -> Result<()> {
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.ini".to_string());

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
    // TODO(task-5 ui-slint): ui::chay_ui() (egui) tạm vô hiệu hoá — xem ghi
    // chú ở `mod ui;` phía trên. Task 5 nối lại bằng UI Slint (MainWindow).
    let _ = (cfg, trang_thai);
    Ok(())
}
