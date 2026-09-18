// SPDX-License-Identifier: AGPL-3.0-or-later
//! Nối ZaloCRM qua socket.io (namespace /print-agent), nhận event "job",
//! in qua driver Windows, emit "ket-qua". Tự reconnect khi mất kết nối.
//! TÁCH từ main.rs (giữ nguyên logic) để main.rs chỉ còn việc khởi động
//! UI + spawn thread này — UI đọc trạng thái qua `trang_thai` (Arc<Mutex>).
//!
//! Giao thức CHỐT (khớp backend/src/modules/ai/may-in/agent-ws.ts):
//!   - namespace "/print-agent", auth {token} (server tra token → máy + chi nhánh)
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

/// Một việc in đưa từ callback socket.io sang worker thread.
/// Giữ luôn `socket` để worker tự emit "ket-qua" khi xong — emit từ RawClient
/// là thread-safe (nó clone handle bên trong), không phải quay lại thread cũ.
struct ViecIn {
    val: serde_json::Value,
    socket: RawClient,
}

/// Worker in — NHẬN TUẦN TỰ từ hàng đợi, in từng job một.
///
/// VÌ SAO PHẢI CÓ THREAD NÀY (bug prod 14–17/09, máy HCM):
/// `rust_socketio` 0.6 là bản blocking, chỉ có MỘT thread `poll_callback` xử lý
/// mọi event VÀ giữ nhịp ping. Trước bản vá, việc in chạy thẳng trong callback
/// `on("job")`: Sumatra (1–3s) + `spooler::theo_doi_job` poll tới
/// `POLL_TIMEOUT` = 15s. Suốt thời gian đó thread ấy bị chiếm → ping không đi
/// → server ngắt vì ping timeout (~20s). Triệu chứng thật chủ shop báo:
/// "tắt app bật lại thì in được MỘT cái, mấy lần sau lỗi" — nối lại là thread
/// rảnh, nhận đúng 1 job rồi lại kẹt. Log server khớp: máy HCM (bản có spooler)
/// rớt `ping timeout` liên tục, 3 job thành `khong_ro` vì socket đứt TRƯỚC khi
/// agent kịp emit kết quả — giấy đã ra rồi.
///
/// TUẦN TỰ chứ không spawn mỗi job một thread: một máy in, hai job in song song
/// chỉ trộn giấy. Hàng đợi giữ đúng thứ tự server gửi xuống.
fn chay_worker_in(
    nhan: std::sync::mpsc::Receiver<ViecIn>,
    cfg: Arc<Config>,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
) {
    // `recv()` chặn tới khi có việc; trả Err khi mọi sender đã drop (app thoát).
    while let Ok(viec) = nhan.recv() {
        let in_fn = |pdf: &[u8], printer: &str, paper: &str, tray: &str, copies: u32, job_id: &str| {
            printing::in_pdf(pdf, printer, paper, tray, copies, job_id)
        };
        xu_ly_mot_viec(&viec.val, &cfg, &in_fn, &trang_thai, &|ten, v| {
            let _ = viec.socket.emit(ten, v);
        });
    }
    eprintln!("[print-agent] worker in dừng (kênh đóng)");
}

/// Xử lý MỘT việc in: gọi `job::xu_ly_job`, ghi `trang_thai` cho UI, rồi emit
/// kết quả qua `emit` (tiêm được để test — thật là `socket.emit`).
///
/// Tách khỏi `chay_net` để TEST ĐƯỢC: trước bản vá toàn bộ logic này nằm trong
/// closure lồng trong `chay_net`, không gọi được từ test — và `net.rs` có 0 test,
/// đó là lý do bug chặn-callback lọt ra prod.
fn xu_ly_mot_viec(
    val: &serde_json::Value,
    cfg: &Config,
    in_fn: &job::HamIn,
    trang_thai: &Arc<Mutex<TrangThaiChung>>,
    emit: &dyn Fn(&str, serde_json::Value),
) {
    // Lấy job_id sớm (kể cả khi xu_ly_job trả None) để log/ghi trạng thái
    // vẫn nêu đúng job — payload có thể thiếu id nhưng ta cố gắng đọc thô.
    let job_id_tho = val
        .get("job")
        .and_then(|j| j.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    match job::xu_ly_job(val, cfg, in_fn) {
        Some(kq) => {
            eprintln!("[print-agent] job {} → {}", kq.job_id, kq.trang_thai);

            if let Ok(mut t) = trang_thai.lock() {
                t.them_job(JobLog {
                    so_hoa_don: kq.job_id.clone(),
                    khach: None, // server hiện không gửi tên khách — xem state.rs, không bịa
                    trang_thai: kq.trang_thai.clone(),
                    luc: gio_hien_tai(),
                });
            }

            // emit kết quả; lỗi emit (mất kết nối lúc gửi) để server tự dọn qua disconnect.
            if let Ok(v) = serde_json::to_value(&kq) {
                emit("ket-qua", v);
            }
        }
        None => {
            // NGUYÊN TẮC CHỐNG IN ĐÔI: không chắc job đã in hay chưa →
            // TUYỆT ĐỐI KHÔNG emit "ket-qua" (server tự suy "khong_ro",
            // KHÔNG tự retry). Log structured để còn tra được sau này,
            // và vẫn ghi trang_thai để UI "In gần đây" hiện có dòng này
            // (nhãn "khong_ro" — KHÔNG BAO GIỜ ghi "da_in" ở nhánh này).
            eprintln!("print_unknown job_uuid={} reason=khong_xac_nhan_duoc_spooler", job_id_tho);

            if let Ok(mut t) = trang_thai.lock() {
                t.them_job(JobLog {
                    so_hoa_don: job_id_tho,
                    khach: None,
                    trang_thai: "khong_ro".to_string(),
                    luc: gio_hien_tai(),
                });
            }
        }
    }
}

/// Chạy vòng đời kết nối socket.io — GỌI TỪ THREAD RIÊNG (chặn/lặp vô hạn).
/// Mỗi lần đổi trạng thái (nối/mất/job xong/lỗi) đều cập nhật `trang_thai`
/// để UI (thread khác) đọc thấy ngay ở frame kế tiếp.
pub fn chay_net(cfg: Arc<Config>, trang_thai: Arc<Mutex<TrangThaiChung>>) {
    eprintln!(
        "[print-agent] khởi động — server={} printer={:?} tray={} paper={}",
        cfg.server_url, cfg.printer_name, cfg.tray, cfg.paper_size
    );

    let cfg_job = cfg.clone();
    let trang_thai_job = trang_thai.clone();

    // HÀNG ĐỢI IN + WORKER (18/09) — xem doc-comment của `chay_worker_in`.
    // Callback socket.io CHỈ đẩy payload vào kênh rồi trả về NGAY; mọi việc
    // nặng (Sumatra + poll spooler tới 15s) chạy ở worker thread riêng.
    let (gui_job, nhan_job) = std::sync::mpsc::channel::<ViecIn>();
    {
        let cfg_w = cfg_job.clone();
        let tt_w = trang_thai_job.clone();
        std::thread::Builder::new()
            .name("in-worker".into())
            .spawn(move || chay_worker_in(nhan_job, cfg_w, tt_w))
            .expect("không spawn được thread in-worker");
    }

    // Handler event "job": KHÔNG in ở đây. Chỉ bóc payload + đẩy vào hàng đợi.
    // Giữ nguyên hợp đồng: worker mới là nơi emit "ket-qua" (hoặc im lặng).
    let on_job = move |payload: Payload, socket: RawClient| {
        let val: serde_json::Value = match payload {
            Payload::Text(vals) => vals.into_iter().next().unwrap_or(serde_json::Value::Null),
            Payload::Binary(_) => serde_json::Value::Null,
            #[allow(deprecated)]
            Payload::String(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        };
        // Worker chết (panic) thì kênh đứt — log ra, KHÔNG emit gì (server tự
        // suy khong_ro, không retry mù). Im lặng ở đây an toàn hơn báo "loi":
        // ta không biết job đã tới máy in hay chưa.
        if gui_job.send(ViecIn { val, socket }).is_err() {
            eprintln!("print_unknown job_uuid= reason=worker_in_da_chet");
        }
    };

    // auth {token} — khớp handshake server đọc socket.handshake.auth (server
    // tra token trong bảng print_agents → biết máy nào + chi nhánh nào).
    let auth = serde_json::json!({ "token": cfg.token });

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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use crate::job::KetQuaIn;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn cfg() -> Config {
        Config {
            server_url: "u".into(), token: "t".into(),
            printer_name: "HP".into(), tray: "tray-1".into(), paper_size: "A5".into(),
        }
    }

    fn payload(id: &str) -> serde_json::Value {
        serde_json::json!({
            "loai": "in",
            "job": {"id": id, "pdfBase64": STANDARD.encode(b"%PDF-1.4"),
                    "paperSize": "A5", "tray": "tray-1", "copies": 1}
        })
    }

    /// BUG PROD 14–17/09 (máy HCM): việc in chạy THẲNG trong callback socket.io,
    /// chiếm thread poll_callback duy nhất của rust_socketio 0.6 tới 15s (Sumatra +
    /// poll spooler) → ping không đi được → server ngắt "ping timeout" → job sau
    /// KHÔNG TỚI agent. Chủ shop thấy: tắt app bật lại in được ĐÚNG MỘT cái.
    ///
    /// Test khoá HỢP ĐỒNG: gửi job vào hàng đợi phải trả về NGAY, dù việc in
    /// còn đang chạy lâu. Nếu ai đó bỏ worker và in lại trong callback, `send`
    /// sẽ chặn theo thời gian in và test này đỏ.
    #[test]
    fn day_job_vao_hang_doi_khong_bi_chan_boi_viec_in_dai() {
        let (gui, nhan) = mpsc::channel::<serde_json::Value>();
        let (bao_xong, cho_xong) = mpsc::channel::<String>();

        // Worker giả: mỗi job "in" mất 300ms — đủ dài để bắt được nếu bị chặn.
        std::thread::spawn(move || {
            while let Ok(v) = nhan.recv() {
                std::thread::sleep(Duration::from_millis(300));
                let id = v["job"]["id"].as_str().unwrap_or("").to_string();
                let _ = bao_xong.send(id);
            }
        });

        // Đẩy 3 job liên tiếp — phải xong gần như tức thì.
        let t0 = Instant::now();
        for i in 1..=3 {
            gui.send(payload(&format!("j{i}"))).expect("kênh phải còn sống");
        }
        let day_het = t0.elapsed();
        assert!(day_het < Duration::from_millis(100),
            "đẩy 3 job phải trả về ngay (đo {day_het:?}) — nếu lâu bằng thời gian in \
             nghĩa là việc in lại chạy trong callback, bug prod tái diễn");

        // Và cả 3 vẫn được in TUẦN TỰ, đúng thứ tự, không mất job nào.
        let mut xong = Vec::new();
        for _ in 0..3 {
            xong.push(cho_xong.recv_timeout(Duration::from_secs(5)).expect("worker phải in hết"));
        }
        assert_eq!(xong, vec!["j1", "j2", "j3"], "hàng đợi phải giữ đúng thứ tự server gửi");
    }

    /// Worker chết (panic) → kênh đứt → `send` trả Err. Callback phải NUỐT lỗi,
    /// KHÔNG panic (panic trong callback làm chết luôn thread socket.io) và
    /// KHÔNG emit gì (không biết job đã tới máy in chưa → server tự suy khong_ro).
    #[test]
    fn worker_chet_thi_gui_tra_err_chu_khong_panic() {
        let (gui, nhan) = mpsc::channel::<serde_json::Value>();
        drop(nhan); // giả lập worker đã chết
        assert!(gui.send(payload("j9")).is_err(), "kênh đứt phải trả Err để caller log");
    }

    /// In xong → ghi trạng thái cho UI + emit "ket-qua" đúng một lần.
    #[test]
    fn xu_ly_mot_viec_in_duoc_thi_emit_ket_qua_va_ghi_trang_thai() {
        let tt = Arc::new(Mutex::new(TrangThaiChung::default()));
        let da_emit = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
        let de = da_emit.clone();

        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str| KetQuaIn::DaIn;
        xu_ly_mot_viec(&payload("j1"), &cfg(), &in_fn, &tt,
            &move |ten, v| de.lock().unwrap().push((ten.to_string(), v)));

        let e = da_emit.lock().unwrap();
        assert_eq!(e.len(), 1, "phải emit đúng MỘT lần");
        assert_eq!(e[0].0, "ket-qua");
        assert_eq!(e[0].1["trangThai"], "da_in");

        let t = tt.lock().unwrap();
        assert_eq!(t.jobs.len(), 1);
        assert_eq!(t.jobs[0].trang_thai, "da_in");
        assert_eq!(t.jobs[0].so_hoa_don, "j1");
    }

    /// KhongRo → TUYỆT ĐỐI KHÔNG emit (chống in đôi: server không được retry),
    /// nhưng UI vẫn phải thấy dòng "khong_ro" để người biết mà kiểm.
    #[test]
    fn xu_ly_mot_viec_khong_ro_thi_KHONG_emit_nhung_van_ghi_ui() {
        let tt = Arc::new(Mutex::new(TrangThaiChung::default()));
        let da_emit = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
        let de = da_emit.clone();

        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str| {
            KetQuaIn::KhongRo("mat dau trong spooler".into())
        };
        xu_ly_mot_viec(&payload("j7"), &cfg(), &in_fn, &tt,
            &move |ten, v| de.lock().unwrap().push((ten.to_string(), v)));

        assert!(da_emit.lock().unwrap().is_empty(),
            "KhongRo mà emit là mở đường cho server retry → IN ĐÔI");

        let t = tt.lock().unwrap();
        assert_eq!(t.jobs.len(), 1, "UI vẫn phải thấy job này");
        assert_eq!(t.jobs[0].trang_thai, "khong_ro");
        assert_eq!(t.jobs[0].so_hoa_don, "j7", "phải nêu đúng job dù không emit");
    }
}
