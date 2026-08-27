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
mod ui;

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

    // Cửa sổ hiện ngay nếu chưa có config hợp lệ (bắt buộc người dùng nhập);
    // ẩn ngay từ đầu nếu đã có config — chạy nền kiểu Tailscale, người dùng
    // tự mở qua tray icon khi cần xem trạng thái.
    let hien_ngay = !hop_le;

    ui::chay_ui(cfg, trang_thai, hien_ngay).map_err(|e| anyhow::anyhow!("eframe lỗi: {}", e))
}
