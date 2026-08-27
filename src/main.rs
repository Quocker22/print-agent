// SPDX-License-Identifier: AGPL-3.0-or-later
//! Print agent: nối ZaloCRM qua socket.io (namespace /print-agent), nhận event
//! "job", in qua driver Windows, emit "ket-qua". Tự reconnect khi mất kết nối.
//!
//! Giao thức CHỐT (khớp backend/src/modules/ai/may-in/agent-ws.ts):
//!   - namespace "/print-agent", auth {token, orgId}
//!   - server→agent event "job": {loai:"in", job:{id,pdfBase64,paperSize,tray,copies}}
//!   - agent→server event "ket-qua": {jobId, trangThai:"da_in"|"loi", loiCuoi?}

mod config;
mod job;
mod printing;

use anyhow::{Context, Result};
use rust_socketio::{ClientBuilder, Payload, RawClient};
use std::sync::Arc;

const NAMESPACE: &str = "/print-agent";

fn main() -> Result<()> {
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.ini".to_string());
    let text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("không đọc được {}", config_path))?;
    let cfg = config::parse_config(&text)?;
    eprintln!(
        "[print-agent] khởi động — server={} org={} printer={:?} tray={} paper={}",
        cfg.server_url, cfg.org_id, cfg.printer_name, cfg.tray, cfg.paper_size
    );

    let cfg = Arc::new(cfg);
    let cfg_job = cfg.clone();

    // Handler event "job": xử lý thuần (job::xu_ly_job) rồi emit "ket-qua".
    let on_job = move |payload: Payload, socket: RawClient| {
        let cfg = cfg_job.clone();
        let val: serde_json::Value = match payload {
            Payload::Text(vals) => vals.into_iter().next().unwrap_or(serde_json::Value::Null),
            Payload::Binary(_) => serde_json::Value::Null,
            #[allow(deprecated)]
            Payload::String(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        };
        let in_fn = |pdf: &[u8], printer: &str, paper: &str, tray: &str, copies: u32| {
            printing::in_pdf(pdf, printer, paper, tray, copies)
        };
        let kq = job::xu_ly_job(&val, &cfg, &in_fn);
        eprintln!("[print-agent] job {} → {}", kq.job_id, kq.trang_thai);
        // emit kết quả; lỗi emit (mất kết nối lúc gửi) để server tự dọn qua disconnect.
        if let Ok(v) = serde_json::to_value(&kq) {
            let _ = socket.emit("ket-qua", v);
        }
    };

    // auth {token, orgId} — khớp handshake server đọc socket.handshake.auth.
    let auth = serde_json::json!({ "token": cfg.token, "orgId": cfg.org_id });

    // reconnect tự động do ClientBuilder bật sẵn; nối vòng lặp lại nếu build lỗi.
    loop {
        eprintln!("[print-agent] đang nối {} ...", cfg.server_url);
        let ket_noi = ClientBuilder::new(&cfg.server_url)
            .namespace(NAMESPACE)
            .auth(auth.clone())
            .reconnect(true)
            .on("job", on_job.clone())
            .on("error", |err, _| eprintln!("[print-agent] lỗi socket: {:?}", err))
            .on("open", |_, _| eprintln!("[print-agent] đã nối server"))
            .connect();

        match ket_noi {
            Ok(_client) => {
                // connect() trả client sống; giữ tiến trình sống, để callback chạy.
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            Err(e) => {
                eprintln!("[print-agent] nối thất bại: {} — thử lại sau 10s", e);
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        }
    }
}
