// SPDX-License-Identifier: AGPL-3.0-or-later
//! Nối ZaloCRM qua socket.io (namespace /print-agent), nhận event "job",
//! in qua driver Windows, emit "ket-qua". Tự reconnect khi mất kết nối.
//! TÁCH từ main.rs (giữ nguyên logic) để main.rs chỉ còn việc khởi động
//! UI + spawn thread này — UI đọc trạng thái qua `trang_thai` (Arc<Mutex>).
//!
//! Giao thức CHỐT (khớp backend/src/modules/ai/may-in/agent-ws.ts):
//!   - namespace "/print-agent", auth {token, orgId}
//!   - server→agent event "job": {loai:"in", job:{id,pdfBase64,paperSize,tray,copies}}
//!   - agent→server event "ket-qua": {jobId, trangThai:"da_in"|"loi", loiCuoi?}

use crate::config::Config;
use crate::job;
use crate::printing;
use crate::state::{JobLog, TrangThaiChung};
use rust_socketio::{ClientBuilder, Payload, RawClient};
use std::sync::{Arc, Mutex};

const NAMESPACE: &str = "/print-agent";

/// Giờ:phút:giây hiện tại — đủ cho UI, không cần chính xác ms.
/// VÌ SAO không dùng crate chrono: chỉ cần giờ địa phương dạng chuỗi ngắn,
/// std::time đủ dùng, tránh thêm dependency chỉ cho 1 chỗ hiển thị.
fn gio_hien_tai() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let trong_ngay = secs % 86400;
    format!("{:02}:{:02}:{:02}", trong_ngay / 3600, (trong_ngay % 3600) / 60, trong_ngay % 60)
}

/// Chạy vòng đời kết nối socket.io — GỌI TỪ THREAD RIÊNG (chặn/lặp vô hạn).
/// Mỗi lần đổi trạng thái (nối/mất/job xong/lỗi) đều cập nhật `trang_thai`
/// để UI (thread khác) đọc thấy ngay ở frame kế tiếp.
pub fn chay_net(cfg: Arc<Config>, trang_thai: Arc<Mutex<TrangThaiChung>>) {
    eprintln!(
        "[print-agent] khởi động — server={} org={} printer={:?} tray={} paper={}",
        cfg.server_url, cfg.org_id, cfg.printer_name, cfg.tray, cfg.paper_size
    );

    let cfg_job = cfg.clone();
    let trang_thai_job = trang_thai.clone();

    // Handler event "job": xử lý thuần (job::xu_ly_job) rồi emit "ket-qua",
    // đồng thời ghi vào trang_thai để UI hiện "In gần đây".
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

        if let Ok(mut t) = trang_thai_job.lock() {
            t.them_job(JobLog {
                so_hoa_don: kq.job_id.clone(),
                khach: None, // server hiện không gửi tên khách — xem state.rs, không bịa
                trang_thai: kq.trang_thai.clone(),
                luc: gio_hien_tai(),
            });
        }

        // emit kết quả; lỗi emit (mất kết nối lúc gửi) để server tự dọn qua disconnect.
        if let Ok(v) = serde_json::to_value(&kq) {
            let _ = socket.emit("ket-qua", v);
        }
    };

    // auth {token, orgId} — khớp handshake server đọc socket.handshake.auth.
    let auth = serde_json::json!({ "token": cfg.token, "orgId": cfg.org_id });

    let trang_thai_open = trang_thai.clone();
    let trang_thai_err = trang_thai.clone();

    // reconnect tự động do ClientBuilder bật sẵn; nối vòng lặp lại nếu build lỗi.
    loop {
        eprintln!("[print-agent] đang nối {} ...", cfg.server_url);
        let trang_thai_open2 = trang_thai_open.clone();
        let trang_thai_err2 = trang_thai_err.clone();
        let ket_noi = ClientBuilder::new(&cfg.server_url)
            .namespace(NAMESPACE)
            .auth(auth.clone())
            .reconnect(true)
            .on("job", on_job.clone())
            .on("error", move |err, _| {
                eprintln!("[print-agent] lỗi socket: {:?}", err);
                if let Ok(mut t) = trang_thai_err2.lock() {
                    t.da_noi = false;
                    t.thong_bao_cuoi = Some(format!("lỗi socket: {:?}", err));
                }
            })
            .on("open", move |_, _| {
                eprintln!("[print-agent] đã nối server");
                if let Ok(mut t) = trang_thai_open2.lock() {
                    t.da_noi = true;
                    t.thong_bao_cuoi = None;
                }
            })
            .connect();

        match ket_noi {
            Ok(_client) => {
                // connect() trả client sống; giữ thread sống, để callback chạy.
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            Err(e) => {
                eprintln!("[print-agent] nối thất bại: {} — thử lại sau 10s", e);
                if let Ok(mut t) = trang_thai.lock() {
                    t.da_noi = false;
                    t.thong_bao_cuoi = Some(format!("nối thất bại: {}", e));
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        }
    }
}
