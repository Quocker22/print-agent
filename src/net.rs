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

/// Chu kỳ kiểm tra `trang_thai.da_noi` trong lúc giữ client sống.
/// 5s đủ nhanh để phát hiện treo mà không tốn CPU (so với sleep(3600) mù trước đây).
const CHU_KY_KIEM_TRA: std::time::Duration = std::time::Duration::from_secs(5);

/// Ngưỡng coi client là CHẾT HẲN khi `da_noi=false` liên tục quá lâu.
/// VÌ SAO 60s: rust_socketio 0.6 (xem client/client.rs::poll_callback) tự
/// phát hiện lỗi transport (EngineIO Error → Error::IncompleteResponseFromEngineIo)
/// và tự gọi reconnect() nội bộ với backoff mặc định (1s→5s, thử vô hạn lần vì
/// max_reconnect_attempts=None), rồi gắn lại đúng các callback on("open")/on("error")/
/// on("job") vào client mới — nên "im lặng" vài giây/chục giây là chuyện BÌNH THƯỜNG,
/// đang trong lúc thư viện tự nối lại, KHÔNG phải zombie. Nếu ta drop+connect() lại
/// ngay ở lần error đầu tiên sẽ đá văng đúng lúc thư viện đang tự phục hồi (double-
/// reconnect, tranh nhau). Ngưỡng 60s đủ rộng để qua nhiều vòng backoff của thư viện,
/// nhưng vẫn đủ hẹp để không để khách chờ hoá đơn quá lâu khi:
///
/// - thread poll_callback nội bộ của thư viện CHẾT HẲN (panic/treo — lúc đó không gì
///   tự nối lại nữa, chờ mãi cũng vô ích), hoặc
/// - server không phản hồi "open" dù transport đã sống lại (kẹt nửa chừng).
///
/// Đây là lớp watchdog NGOÀI, bổ sung cho reconnect nội bộ của thư viện — không thay thế.
const NGUONG_CHET_HAN: std::time::Duration = std::time::Duration::from_secs(60);

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

    // Vòng NGOÀI: build + connect() lại từ đầu mỗi khi client cũ bị coi là chết.
    // rust_socketio tự reconnect ở TẦNG TRONG của nó (xem NGUONG_CHET_HAN ở trên);
    // vòng ngoài này là watchdog dự phòng khi tầng trong không tự cứu được nữa.
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
                // connect() trả client sống. VÌ SAO _client khai báo TRONG vòng
                // trong (không đẩy ra ngoài): khi vòng trong `break` để reconnect,
                // _client ra khỏi scope và DROP ngay tại đây — đóng socket cũ
                // trước khi ClientBuilder::connect() mới được gọi ở vòng ngoài,
                // tránh rò socket (2 kết nối cùng auth token chồng nhau).
                //
                // Health-check thay cho sleep(3600) mù: kiểm trang_thai.da_noi
                // mỗi CHU_KY_KIEM_TRA (5s). Đếm thời gian da_noi=false LIÊN TỤC;
                // hễ đủ NGUONG_CHET_HAN (60s) thì coi client chết hẳn, thoát
                // vòng trong để vòng ngoài connect() lại từ đầu. Mỗi lần thấy
                // da_noi=true thì reset bộ đếm — chỉ tính CHUỖI mất kết nối
                // liên tục, không cộng dồn qua nhiều lần rớt-nối ngắt quãng.
                let mut mat_ket_noi_tu: Option<std::time::Instant> = None;
                loop {
                    std::thread::sleep(CHU_KY_KIEM_TRA);

                    let da_noi = match trang_thai.lock() {
                        Ok(t) => t.da_noi,
                        // Mutex poisoned (thread khác panic khi đang giữ khoá) —
                        // coi như không rõ trạng thái, thà kiểm tiếp còn hơn
                        // đoán bừa; vòng sau lock lại vẫn poisoned nên rơi vào
                        // nhánh mất-kết-nối bên dưới qua giá trị mặc định false.
                        Err(poisoned) => poisoned.into_inner().da_noi,
                    };

                    if da_noi {
                        mat_ket_noi_tu = None;
                        continue;
                    }

                    let luc_bat_dau_mat = *mat_ket_noi_tu.get_or_insert_with(std::time::Instant::now);
                    let da_mat_bao_lau = luc_bat_dau_mat.elapsed();

                    if da_mat_bao_lau >= NGUONG_CHET_HAN {
                        eprintln!(
                            "[print-agent] mất kết nối >{}s liên tục — coi client chết, nối lại từ đầu...",
                            NGUONG_CHET_HAN.as_secs()
                        );
                        if let Ok(mut t) = trang_thai.lock() {
                            t.thong_bao_cuoi = Some(format!(
                                "mất kết nối >{}s, đang nối lại...",
                                NGUONG_CHET_HAN.as_secs()
                            ));
                        }
                        break; // thoát vòng trong → _client drop → vòng ngoài connect() lại
                    }
                }
                // _client drop ở đây (cuối scope Ok(_client)).
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
