// SPDX-License-Identifier: AGPL-3.0-or-later
//! Nối ZaloCRM qua socket.io (namespace /print-agent), nhận event "job",
//! in qua driver Windows, emit "ket-qua". Tự reconnect khi mất kết nối.
//! TÁCH từ main.rs (giữ nguyên logic) để main.rs chỉ còn việc khởi động
//! UI + spawn thread này — UI đọc trạng thái qua `trang_thai` (Arc<Mutex>).
//!
//! Giao thức (khớp backend agent-ws.ts; hợp đồng v2 `HOP-DONG-NHAT-KY-MAY-IN.md` §2):
//!   - namespace "/print-agent", auth {token} (server tra token → máy + chi nhánh)
//!   - server→agent "job": {loai:"in", job:{id,name?,pdfBase64,paperSize,tray,copies}}
//!   - server→agent "cau-hinh": {hoTro:[...]} — nhớ theo TỪNG kết nối
//!   - agent→server "ket-qua": {jobId, trangThai:"da_in"|"loi"|"khong_ro", loiCuoi?, loai?}
//!     (`khong_ro` chỉ khi hoTro có "khong_ro" — không thì im lặng như cũ)
//!   - agent→server "su-co", "trang-thai-may-in" (theo hoTro), "thong-tin-app" (luôn)
//!
//! Năm luồng: poll của rust_socketio (callback — chỉ nhận/đẩy, không bao giờ
//! chặn), worker in (tuần tự), theo dõi máy in (một mình quyết "đổi trạng
//! thái" nên không tranh chấp), theo dõi tiếp job `khong_ro` (R3), watchdog
//! (vòng lặp của `chay_net`). Mọi event lên backend đi qua `DuongGui` — lấy
//! kết nối HIỆN TẠI lúc gửi, không giữ socket của kết nối đã giao job (R4).

use crate::bao_cao::{self, BoLocSuCo};
use crate::config::Config;
use crate::hang_doi;
use crate::hop_thu_di::{CanHoTro, CongGui, DuongGui, KetQuaGui, ThuDi};
use crate::job;
use crate::nhat_ky;
use crate::printing;
use crate::spooler::{self, BangChungJob, KiemTruoc, QuanSat};
use crate::state::{JobLog, TrangThaiChung};
use crate::su_co::{MaSuCo, TapMa};
use crate::theo_doi_tiep::{self, JobTheoDoiTiep, KetLuanTiep, KhoTheoDoiTiep};
use rust_socketio::client::Client;
use rust_socketio::{ClientBuilder, Payload, RawClient, TransportType};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const NAMESPACE: &str = "/print-agent";

/// Nhịp của vòng canh client (watchdog). Ngắn để bấm Lưu (R7) dừng được kết
/// nối cũ trong chưa tới nửa giây; mỗi nhịp chỉ đọc vài biến trong bộ nhớ.
const NHIP_CANH: Duration = Duration::from_millis(250);

/// Ngưỡng coi client là CHẾT HẲN khi `da_noi=false` LIÊN TỤC quá lâu mà không
/// callback nào báo (lưới cuối của vòng canh — `canh_client`).
///
/// Tự nối lại của thư viện đã TẮT (`.reconnect(false)`, R-H): mọi lỗi/đóng kết
/// nối đi qua callback "error"/"close" → `client_chet` → vòng ngoài nối lại
/// NGAY với backoff 1→30 s. Ngưỡng này chỉ còn cho ca KHÔNG callback nào tới:
/// - `connect()` trả Ok nhưng server không bao giờ gửi CONNECT ack của
///   namespace (không có "open" — kẹt nửa chừng ở middleware/proxy);
/// - luồng poll của thư viện chết/treo theo cách không gọi callback nào.
///
/// Kết nối ĐANG mở (`da_noi=true`) mà chết lặng (TCP nửa mở) thì transport
/// websocket tự phát hiện: `rust_engineio` chờ mỗi gói tối đa pingInterval +
/// pingTimeout rồi trả `PingTimeout` → callback "error" → nối lại. Polling đã
/// bị loại (T1) vì chính nó là đường chết lặng không callback.
const NGUONG_CHET_HAN: Duration = Duration::from_secs(60);

// Luồng poll của rust_socketio được KẾT THÚC bằng `resume_unwind` từ trong
// callback (`thoat_luong_poll`, R-H/R-I). Với `panic = "abort"` lệnh đó giết
// CẢ TIẾN TRÌNH mỗi lần mất mạng — chặn ngay lúc biên dịch (T10).
#[cfg(panic = "abort")]
compile_error!("print-agent cần panic=unwind (net.rs kết thúc luồng poll bằng resume_unwind)");

/// Transport engine.io DUY NHẤT app dùng (T1, giám sát vòng 3): websocket.
///
/// VÌ SAO không để mặc định (`Any` — thử websocket, hỏng thì LẶNG LẼ rơi về
/// polling; đo: 1/23 lần nối lại rơi polling): trên POLLING, khi engine đóng
/// (server gửi gói close "1", hoặc chính ta `disconnect()`), `RawClient::poll`
/// trả `Ok(None)` mà KHÔNG gọi callback nào → vòng lặp của thư viện thành
/// `Err(StoppedEngineIoSocket)` quay rỗng 100% một nhân, `da_noi` kẹt `true`,
/// `DuongGui` giữ cổng chết, watchdog không nổ (nó chỉ đếm `da_noi=false`).
/// Websocket: đóng/đứt kết nối hiện ra thành LỖI → callback "error" → nối lại.
///
/// Đánh đổi: mạng/proxy CHẶN websocket thì app KHÔNG nối được (nhật ký
/// `noi_that_bai` ghi rõ lý do) — trước đây nó có thể rơi về polling.
const TRANSPORT: TransportType = TransportType::Websocket;

/// Ghi vào nhật ký khi nối thất bại — nói rõ app chỉ dùng websocket (T1).
const CHU_CHI_WEBSOCKET: &str =
    "app chi dung websocket (khong dung polling) — mang/proxy/firewall chan websocket thi khong noi duoc";

/// Chu kỳ đọc trạng thái máy in lúc RẢNH (hợp đồng §2: mỗi 20 giây). Lúc đang
/// in, worker đã đọc máy in mỗi 500 ms và chuyển sang — không đọc chồng.
const CHU_KY_DOC_MAY_IN: Duration = Duration::from_secs(20);

/// Worker in một job quá chừng này (Sumatra treo, spooler treo…) thì luồng
/// theo dõi máy in vẫn đọc máy in như lúc rảnh (R8) — không để giao diện và
/// backend "mù" trạng thái máy in đúng lúc có chuyện.
const DANG_IN_QUA_LAU: Duration = Duration::from_secs(20);

/// Chờ tối đa chừng này cho `disconnect()` client cũ (R7).
const CHO_NGAT_CLIENT_CU: Duration = Duration::from_secs(3);
/// Bấm Lưu: chờ tối đa chừng này cho `chay_net` cũ dừng hẳn rồi mới nối (R7a).
const CHO_NET_CU_DUNG: Duration = Duration::from_secs(10);

/// Giờ:phút:giây hiện tại theo GIỜ MÁY — hiện ở cột giờ của "In gần đây".
/// VÌ SAO không dùng crate chrono: chỉ cần giờ địa phương dạng chuỗi ngắn.
/// Bản trước tính từ UNIX epoch = giờ UTC (lệch 7 tiếng ở VN) — không ai thấy
/// vì giao diện chưa hiện cột này; nay hiện thì phải đúng giờ shop: Windows
/// lấy bằng GetLocalTime, máy dev giữ UTC.
#[cfg(windows)]
fn gio_hien_tai() -> String {
    // SAFETY: GetLocalTime chỉ ghi vào SYSTEMTIME trả về, không có điều kiện trước.
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond)
}

#[cfg(not(windows))]
fn gio_hien_tai() -> String {
    use std::time::UNIX_EPOCH;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let trong_ngay = secs % 86400;
    format!("{:02}:{:02}:{:02}", trong_ngay / 3600, (trong_ngay % 3600) / 60, trong_ngay % 60)
}

/// Đối số đầu tiên của một event socket.io (payload là mảng đối số).
fn payload_dau(payload: Payload) -> serde_json::Value {
    match payload {
        Payload::Text(vals) => vals.into_iter().next().unwrap_or(serde_json::Value::Null),
        Payload::Binary(_) => serde_json::Value::Null,
        #[allow(deprecated)]
        Payload::String(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
    }
}

fn job_id_tho(val: &serde_json::Value) -> String {
    val.get("job")
        .and_then(|j| j.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn khoa<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Cổng emit thật của MỘT kết nối: `RawClient` mà callback "open" nhận được.
struct CongSocket(Mutex<RawClient>);

impl CongGui for CongSocket {
    fn emit(&self, su_kien: &str, gia_tri: serde_json::Value) -> Result<(), String> {
        khoa(&self.0).emit(su_kien, gia_tri).map_err(|e| e.to_string())
    }

    /// Ack về qua callback trên luồng poll — callback chỉ đẩy vào kênh (không
    /// chặn luồng poll: chặn callback = mất ping, bài học 14–17/09); luồng gọi
    /// chờ kênh SAU KHI đã nhả khoá client.
    fn emit_ack(&self, su_kien: &str, gia_tri: serde_json::Value, cho: Duration) -> Result<serde_json::Value, String> {
        let (gui, nhan) = mpsc::channel::<serde_json::Value>();
        khoa(&self.0)
            .emit_with_ack(su_kien, gia_tri, cho, move |p: Payload, _c: RawClient| {
                let _ = gui.send(gia_tri_ack(p));
            })
            .map_err(|e| e.to_string())?;
        nhan.recv_timeout(cho).map_err(|_| "het gio cho ack".to_string())
    }
}

/// Một việc in đưa từ callback socket.io sang worker thread. KHÔNG giữ socket
/// của kết nối đã giao job (R4): kết quả đi qua kết nối hiện tại lúc gửi.
struct ViecIn {
    val: serde_json::Value,
}

/// Lệnh cho luồng theo dõi máy in. MỘT luồng duy nhất ghi trạng thái máy in +
/// quyết gửi `trang-thai-may-in`, nên hai nguồn đọc (lúc rảnh / lúc in) không
/// bao giờ gửi chéo thứ tự nhau.
enum LenhMayIn {
    /// Worker đọc được trạng thái máy in (đã đổi) trong lúc theo dõi job.
    QuanSat(MaSuCo, Option<String>),
    /// Vừa nhận `cau-hinh`: xả hộp thư đi, gửi trạng thái hiện tại ngay (§2).
    CoCauHinh,
    /// Job vừa xong có sự cố: lần đọc lúc rảnh kế tiếp PHẢI gửi dù không đổi (R6).
    EpGuiLanRanhToi,
    /// `chay_net` dừng (bấm Lưu, R7a).
    Dung,
}

/// "Worker đang in từ lúc nào" — `None` = rảnh.
type DangInTu = Mutex<Option<Instant>>;

/// Đặt mốc "đang in" suốt một job, tự xoá khi ra khỏi scope — kể cả khi panic,
/// để luồng theo dõi máy in không bị khoá "đang in" mãi.
struct CoDangIn<'a>(&'a DangInTu);

impl<'a> CoDangIn<'a> {
    fn bat(co: &'a DangInTu) -> Self {
        *khoa(co) = Some(Instant::now());
        Self(co)
    }
}

impl Drop for CoDangIn<'_> {
    fn drop(&mut self) {
        *khoa(self.0) = None;
    }
}

/// Luồng theo dõi máy in có được đọc máy in lúc này không (R8): rảnh, hoặc
/// worker đã in một job quá `DANG_IN_QUA_LAU`.
fn nen_doc_may_in(dang_in_tu: Option<Instant>, bay_gio: Instant) -> bool {
    dang_in_tu.is_none_or(|t| bay_gio.saturating_duration_since(t) >= DANG_IN_QUA_LAU)
}

/// Hàm gửi event lên backend (thật = `DuongGui::gui`).
type HamGui<'a> = dyn Fn(&'static str, serde_json::Value, CanHoTro) -> KetQuaGui + 'a;

/// Hàm in thật có kênh báo `QuanSat` (thật = `printing::in_pdf`). Đối số áp
/// cuối = cờ nền chụp TRƯỚC Sumatra (T3).
type HamInCoBao<'a> =
    dyn Fn(&[u8], &str, &str, &str, u32, &str, Option<&str>, Option<TapMa>, &dyn Fn(QuanSat)) -> job::KetQuaIn + 'a;

/// Kiểm TRƯỚC KHI IN (thật = một vòng đọc spooler + danh sách theo dõi tiếp:
/// R-A/R-J, T2, T3, T5). `TuChoi` = không in, trả `loi` ngay.
type HamKiemTruoc<'a> = dyn Fn() -> KiemTruoc + 'a;

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
///
/// Dừng khi `dung` bật (bấm Lưu) và hàng đợi đã hết — job đã nhận thì in xong
/// rồi mới dừng; kết quả của nó vẫn đi qua `DuongGui` (kết nối mới).
#[allow(clippy::too_many_arguments)]
fn chay_worker_in(
    nhan: mpsc::Receiver<ViecIn>,
    cfg: Arc<Config>,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
    dang_in: Arc<DangInTu>,
    gui_may_in: Sender<LenhMayIn>,
    duong_gui: Arc<DuongGui>,
    kho_theo_doi: Arc<KhoTheoDoiTiep>,
    dung: Arc<AtomicBool>,
) {
    loop {
        let viec = match nhan.recv_timeout(Duration::from_millis(500)) {
            Ok(v) => v,
            Err(RecvTimeoutError::Timeout) if dung.load(Ordering::SeqCst) => break,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let _dang_in = CoDangIn::bat(&dang_in);
        let gui = |su_kien: &'static str, v: serde_json::Value, can: CanHoTro| duong_gui.gui(su_kien, v, can);
        let chuyen_may_in = |ma: MaSuCo, chi_tiet: Option<String>| {
            let _ = gui_may_in.send(LenhMayIn::QuanSat(ma, chi_tiet));
        };
        let kiem_truoc = || kiem_truoc_khi_in_that(&cfg.printer_name, &kho_theo_doi, &duong_gui);
        let xong = xu_ly_viec_co_bao_cao(&viec.val, &cfg, &printing::in_pdf, &kiem_truoc, &trang_thai, &gui, &chuyen_may_in);
        if xong.da_thay_su_co {
            let _ = gui_may_in.send(LenhMayIn::EpGuiLanRanhToi);
        }
        if let Some(j) = xong.theo_doi_tiep {
            dua_vao_theo_doi_tiep(&kho_theo_doi, j);
        }
    }
    eprintln!("[print-agent] worker in dừng");
}

/// Đưa một job vào danh sách theo dõi tiếp (dùng chung qua lần bấm Lưu) + ghi
/// nhật ký; job CŨ NHẤT bị bỏ vì vượt trần cũng được ghi (không mất lặng).
fn dua_vao_theo_doi_tiep(kho: &KhoTheoDoiTiep, j: JobTheoDoiTiep) {
    nhat_ky::ghi(
        "theo_doi_tiep_them",
        &format!(
            "job={} hoa_don={} may_in={} theo={}",
            job::rut_gon_job_id(&j.job_id),
            j.so_hoa_don,
            j.may_in,
            if j.la_qua_usb() { "usb" } else { "hang_doi" }
        ),
    );
    if let Some(bo) = kho.them(j) {
        nhat_ky::ghi(
            "theo_doi_tiep_bo",
            &format!(
                "job={} hoa_don={} — vuot {} job dang theo doi",
                job::rut_gon_job_id(&bo.job_id),
                bo.so_hoa_don,
                theo_doi_tiep::SO_JOB_TOI_DA
            ),
        );
    }
}

/// Bước kiểm TRƯỚC KHI gọi Sumatra — MỘT vòng đọc spooler (cờ máy in + hàng
/// đợi) phục vụ cả ba việc (xem `spooler::kiem_truoc_khi_in`): máy in không
/// tồn tại (T5), hàng đợi kẹt (R-A/R-J; không đọc được thì dựa vào lần đọc
/// chưa quá 60 s của luồng theo dõi tiếp — T2), chụp cờ nền (T3). Chỉ TỪ CHỐI
/// khi kết nối hiện tại đã nhận `cau-hinh` (backend mới có cầu dao — T2).
/// Dry-run (không gửi gì xuống máy in) thì bỏ qua.
fn kiem_truoc_khi_in_that(may_in: &str, kho: &KhoTheoDoiTiep, duong_gui: &DuongGui) -> KiemTruoc {
    if printing::dang_dry_run() {
        return KiemTruoc::In { nen: None };
    }
    let vong = spooler::mo_spooler(may_in).doc_vong();
    spooler::kiem_truoc_khi_in(&vong, may_in, kho.job_dang_ket(may_in, Instant::now()), duong_gui.backend_moi())
}

/// Nhận `QuanSat` từ spooler trong lúc in MỘT job:
/// - `SuCo` → gửi `su-co` NGAY, MỖI loai MỘT lần; bật dải cảnh báo; ghi nhật ký.
///   Vẫn để spooler theo dõi tiếp như cũ.
/// - `MayIn` → chuyển cho luồng theo dõi máy in, chỉ khi mã đổi (spooler báo
///   mỗi 500 ms, không cần dội 30 lệnh giống nhau vào kênh).
/// - `ConTrongHangDoi` → nhớ để đưa job vào theo dõi tiếp nếu kết quả là `khong_ro`.
struct BaoCaoTrongLuc<'a> {
    job_id: &'a str,
    so_hoa_don: &'a str,
    may_in: &'a str,
    trang_thai: &'a Mutex<TrangThaiChung>,
    gui: &'a HamGui<'a>,
    chuyen_may_in: &'a dyn Fn(MaSuCo, Option<String>),
    // Chỉ worker gọi (spooler chạy trên chính luồng worker) → RefCell/Cell đủ.
    bo_loc: RefCell<BoLocSuCo>,
    may_in_da_chuyen: Cell<Option<MaSuCo>>,
    da_thay_su_co: Cell<bool>,
    con_trong_hang_doi: Cell<Option<BangChungJob>>,
    /// `Some((da_thay_loi, da_thay_in))` = job nằm trong BỘ NHỚ máy in USB (U2) — theo dõi tiếp qua USB.
    trong_may_in_usb: Cell<Option<(bool, bool)>>,
}

impl BaoCaoTrongLuc<'_> {
    fn nhan(&self, qs: QuanSat) {
        match qs {
            QuanSat::SuCo { loai, chi_tiet } => {
                self.da_thay_su_co.set(true);
                if !self.bo_loc.borrow_mut().lan_dau(loai) {
                    return;
                }
                eprintln!("[print-agent] sự cố khi in {}: {} ({})", job::rut_gon_job_id(self.job_id), loai.ma(), chi_tiet);
                khoa(self.trang_thai).ghi_su_co_dang_in(loai, self.so_hoa_don, self.job_id);
                let ct = (!chi_tiet.is_empty()).then_some(chi_tiet.as_str());
                let kq = (self.gui)("su-co", bao_cao::su_co(self.job_id, loai, ct, self.may_in, SystemTime::now()), CanHoTro::SuCo);
                nhat_ky::ghi(
                    "su_co",
                    &format!("job={} loai={} gui_server={} {}", job::rut_gon_job_id(self.job_id), loai.ma(), kq.chu(), chi_tiet),
                );
            }
            QuanSat::MayIn { ma, chi_tiet } => {
                if self.may_in_da_chuyen.get() != Some(ma) {
                    self.may_in_da_chuyen.set(Some(ma));
                    (self.chuyen_may_in)(ma, chi_tiet);
                }
            }
            QuanSat::ConTrongHangDoi(bc) => self.con_trong_hang_doi.set(Some(bc)),
            // Không còn trong hàng đợi Windows nhưng CÒN chờ trong máy in — với
            // backend nghĩa y hệt (`conTrongHangDoi:true`: tự in, KHÔNG in lại).
            QuanSat::TrongMayInUsb { bang_chung, da_thay_loi, da_thay_in } => {
                self.con_trong_hang_doi.set(Some(bang_chung));
                self.trong_may_in_usb.set(Some((da_thay_loi, da_thay_in)));
            }
            QuanSat::DaRoiHangDoi => khoa(self.trang_thai).doi_trang_thai_job(self.job_id, job::CHO_MAY_IN),
        }
    }
}

/// Điều worker cần làm sau MỘT job.
#[derive(Debug, Default)]
struct KetThucViec {
    /// Có sự cố trong lúc in → ép gửi trạng thái máy in lần rảnh tới (R6).
    da_thay_su_co: bool,
    /// Job `khong_ro` còn nằm trong hàng đợi Windows (R3) hoặc trong bộ nhớ máy
    /// in USB (U2) → theo dõi tiếp.
    theo_doi_tiep: Option<JobTheoDoiTiep>,
}

/// Một job, kèm kênh báo sự cố/trạng thái máy in trong lúc in. Tách khỏi
/// `chay_worker_in` để test được cả chuỗi "su-co gửi NGAY, trước ket-qua".
///
/// `kiem_truoc` (R-A/R-J, T5): hàng đợi đang kẹt / máy in không tồn tại →
/// KHÔNG gọi hàm in (không spool, không Sumatra), trả `loi` ngay — chưa byte
/// nào rời máy, backend giữ hoá đơn và gửi lại khi máy hết lỗi. Được in thì
/// cờ nền chụp ở bước kiểm (TRƯỚC Sumatra, T3) đi xuống hàm in.
fn xu_ly_viec_co_bao_cao(
    val: &serde_json::Value,
    cfg: &Config,
    in_that: &HamInCoBao<'_>,
    kiem_truoc: &HamKiemTruoc<'_>,
    trang_thai: &Mutex<TrangThaiChung>,
    gui: &HamGui<'_>,
    chuyen_may_in: &dyn Fn(MaSuCo, Option<String>),
) -> KetThucViec {
    let job_id = job_id_tho(val);
    let name = val.get("job").and_then(|j| j.get("name")).and_then(|v| v.as_str());
    let (so_hoa_don, _) = job::nhan_hien_thi(&job_id, name);
    let bao_cao = BaoCaoTrongLuc {
        job_id: &job_id,
        so_hoa_don: &so_hoa_don,
        may_in: &cfg.printer_name,
        trang_thai,
        gui,
        chuyen_may_in,
        bo_loc: RefCell::new(BoLocSuCo::default()),
        may_in_da_chuyen: Cell::new(None),
        da_thay_su_co: Cell::new(false),
        con_trong_hang_doi: Cell::new(None),
        trong_may_in_usb: Cell::new(None),
    };
    let bao = |qs: QuanSat| bao_cao.nhan(qs);
    let in_fn = |pdf: &[u8], printer: &str, paper: &str, tray: &str, copies: u32, id: &str, ten: Option<&str>| {
        match kiem_truoc() {
            KiemTruoc::TuChoi { ly_do, su_kien } => {
                nhat_ky::ghi(
                    su_kien,
                    &format!("job={} loai={} {}", job::rut_gon_job_id(id), ly_do.loai.map_or("-", MaSuCo::ma), ly_do.chu),
                );
                job::KetQuaIn::Loi(ly_do)
            }
            KiemTruoc::In { nen } => in_that(pdf, printer, paper, tray, copies, id, ten, nen, &bao),
        }
    };
    let con_trong = || bao_cao.con_trong_hang_doi.get().is_some();
    let (kq, _) = xu_ly_mot_viec(val, cfg, &in_fn, trang_thai, gui, &con_trong);
    // Theo dõi tiếp ĐÚNG khi đã báo backend `conTrongHangDoi:true` (T9: in
    // thiếu bản thì không — bản còn lại đã gỡ khỏi hàng đợi).
    let theo_doi_tiep = match (kq.trang_thai.as_str(), kq.con_trong_hang_doi, bao_cao.con_trong_hang_doi.get()) {
        (job::KHONG_RO, Some(true), Some(bc)) => {
            let j = JobTheoDoiTiep::moi(job_id.clone(), so_hoa_don.clone(), kq.loai, bc, Instant::now())
                .tren_may_in(&cfg.printer_name);
            Some(match bao_cao.trong_may_in_usb.get() {
                Some((da_thay_loi, da_thay_in)) => j.qua_usb(da_thay_loi, da_thay_in),
                None => j,
            })
        }
        _ => None,
    };
    KetThucViec { da_thay_su_co: bao_cao.da_thay_su_co.get(), theo_doi_tiep }
}

/// Xử lý MỘT việc in: gọi `job::xu_ly_job`, gửi `ket-qua` qua `gui`, ghi
/// `trang_thai` cho UI + nhật ký ĐÚNG kết quả gửi (R4).
///
/// Tách khỏi `chay_net` để TEST ĐƯỢC: trước bản vá toàn bộ logic này nằm trong
/// closure lồng trong `chay_net`, không gọi được từ test — và `net.rs` có 0 test,
/// đó là lý do bug chặn-callback lọt ra prod.
///
/// `con_trong_hang_doi` (R-D): hỏi SAU khi in xong — `khong_ro` mà job còn
/// trong hàng đợi (đã giao theo dõi tiếp) → `conTrongHangDoi:true`.
fn xu_ly_mot_viec(
    val: &serde_json::Value,
    cfg: &Config,
    in_fn: &job::HamIn<'_>,
    trang_thai: &Mutex<TrangThaiChung>,
    gui: &HamGui<'_>,
    con_trong_hang_doi: &dyn Fn() -> bool,
) -> (job::KetQua, KetQuaGui) {
    // Lấy job_id/name sớm (thô, từ payload) để log/giao diện nêu đúng job kể cả
    // khi payload hỏng.
    let job_id_tho = job_id_tho(val);
    let name = val.get("job").and_then(|j| j.get("name")).and_then(|v| v.as_str());
    let (so_hoa_don, khach) = job::nhan_hien_thi(&job_id_tho, name);
    let id_ngan = job::rut_gon_job_id(&job_id_tho);
    nhat_ky::ghi(
        "nhan_job",
        &format!("job={} hoa_don={} khach={}", id_ngan, so_hoa_don, khach.as_deref().unwrap_or("-")),
    );
    // Hiện NGAY trên app (0.2.5): trước bản này "In gần đây" im lặng tới khi có
    // kết quả cuối (tới 30 s với máy USB) — NV tưởng app không nhận lệnh.
    khoa(trang_thai).bat_dau_job(&job_id_tho, &so_hoa_don, khach.clone(), gio_hien_tai());

    let mut kq = job::xu_ly_job(val, cfg, in_fn);
    if kq.trang_thai == job::KHONG_RO {
        // T9: in thiếu bản — bản còn lại đã gỡ khỏi hàng đợi, không có gì để
        // theo dõi tiếp và nó KHÔNG nằm trong máy in.
        kq.con_trong_hang_doi = Some(kq.ban_da_in.is_none() && con_trong_hang_doi());
    }
    eprintln!("[print-agent] job {} → {}", kq.job_id, kq.trang_thai);

    // Gửi qua kết nối HIỆN TẠI. `khong_ro` + backend không báo hỗ trợ → `bo`:
    // NGUYÊN TẮC CHỐNG IN ĐÔI — backend cũ hiểu mọi trangThai khác "loi" là đã
    // in; im lặng thì nó tự suy khong_ro, KHÔNG tự retry.
    let ket_qua_gui = match serde_json::to_value(&kq) {
        Ok(v) => gui("ket-qua", v, CanHoTro::cua_ket_qua(&kq)),
        Err(_) => KetQuaGui::Bo,
    };
    if ket_qua_gui == KetQuaGui::Bo {
        eprintln!("print_unknown job_uuid={} reason=khong_gui_ket_qua ({})", id_ngan, kq.trang_thai);
    }

    // UI "In gần đây" luôn có dòng này, kể cả khi không gửi.
    {
        let mut t = khoa(trang_thai);
        t.them_job(JobLog {
            job_id: job_id_tho.clone(),
            so_hoa_don: so_hoa_don.clone(),
            khach,
            trang_thai: kq.trang_thai.clone(),
            loai: kq.loai,
            sau_khac_phuc: false,
            ban_da_in: kq.ban_da_in,
            luc: gio_hien_tai(),
        });
        t.ghi_ket_qua(&job_id_tho, &so_hoa_don, &kq.trang_thai, kq.loai, kq.con_trong_hang_doi == Some(false), kq.ban_da_in);
    }
    nhat_ky::ghi(
        "ket_qua",
        &format!(
            "job={} hoa_don={} trang_thai={} loai={} con_trong_hang_doi={} gui_server={} {}",
            id_ngan,
            so_hoa_don,
            kq.trang_thai,
            kq.loai.map_or("-", MaSuCo::ma),
            kq.con_trong_hang_doi.map_or("-", |c| if c { "co" } else { "khong" }),
            ket_qua_gui.chu(),
            kq.loi_cuoi.as_deref().unwrap_or("")
        ),
    );
    (kq, ket_qua_gui)
}

/// Ghi một lần đọc trạng thái máy in vào `trang_thai`; trả payload
/// `trang-thai-may-in` khi PHẢI gửi: mã đổi, hoặc `ep_gui` (ngay sau
/// `cau-hinh`, hoặc lần rảnh đầu sau một job có sự cố — R6). Có gửi thật hay
/// không do `DuongGui` quyết theo hoTro; không hỗ trợ vẫn ghi (giao diện cần).
fn ghi_trang_thai_may_in(
    trang_thai: &Mutex<TrangThaiChung>,
    may_in: &str,
    ma: MaSuCo,
    chi_tiet: Option<String>,
    ep_gui: bool,
    luc_ranh: bool,
) -> Option<serde_json::Value> {
    let doi = khoa(trang_thai).ghi_may_in(ma, chi_tiet.clone(), luc_ranh);
    if doi {
        eprintln!("[print-agent] máy in → {}", ma.ma());
        nhat_ky::ghi("trang_thai_may_in", &format!("{} {}", ma.ma(), chi_tiet.as_deref().unwrap_or("")));
    }
    (doi || ep_gui).then(|| bao_cao::trang_thai_may_in(ma, chi_tiet.as_deref(), may_in, SystemTime::now()))
}

/// Một bước của luồng theo dõi máy in.
#[derive(Debug, PartialEq, Eq)]
enum BuocMayIn {
    /// Ghi trạng thái worker đọc được giữa lúc in (không phải lúc rảnh).
    GhiTuWorker(MaSuCo, Option<String>),
    /// Đọc máy in lúc rảnh (hoặc worker in quá lâu, R8).
    DocRanh { ep_gui: bool },
    /// Vừa có `cau-hinh`: xả hộp thư đi, gửi trạng thái hiện tại (ép).
    SauCauHinh,
    KhongLamGi,
    Thoat,
}

/// Bộ quyết của luồng theo dõi máy in — THUẦN để test R6/R8.
#[derive(Debug, Default)]
struct BoTheoDoiMayIn {
    /// Có job vừa xong kèm sự cố: lần đọc lúc rảnh kế tiếp PHẢI gửi dù
    /// không đổi — backend xoá chip "Hết giấy" kẹt khi máy in không bật cờ
    /// cấp máy (driver chỉ bật cờ trên job, trạng thái máy luôn "bình thường").
    ep_gui_lan_ranh_toi: bool,
}

impl BoTheoDoiMayIn {
    fn buoc(&mut self, su_kien: Result<LenhMayIn, RecvTimeoutError>, co_the_doc: bool) -> BuocMayIn {
        match su_kien {
            Ok(LenhMayIn::QuanSat(ma, ct)) => BuocMayIn::GhiTuWorker(ma, ct),
            Ok(LenhMayIn::CoCauHinh) => BuocMayIn::SauCauHinh,
            Ok(LenhMayIn::EpGuiLanRanhToi) => {
                self.ep_gui_lan_ranh_toi = true;
                BuocMayIn::KhongLamGi
            }
            Ok(LenhMayIn::Dung) | Err(RecvTimeoutError::Disconnected) => BuocMayIn::Thoat,
            Err(RecvTimeoutError::Timeout) if co_the_doc => {
                BuocMayIn::DocRanh { ep_gui: std::mem::take(&mut self.ep_gui_lan_ranh_toi) }
            }
            Err(RecvTimeoutError::Timeout) => BuocMayIn::KhongLamGi,
        }
    }
}

/// Luồng theo dõi máy in: đọc `GetPrinterW` mỗi CHU_KY_DOC_MAY_IN khi RẢNH,
/// nhận trạng thái worker đọc được lúc in, gửi `trang-thai-may-in` khi đổi.
///
/// `dung` (T7): trạng thái giao diện dùng chung qua lần bấm Lưu — luồng của
/// lần chạy CŨ (máy in cũ) thôi ghi/gửi ngay khi cờ dừng bật, không đè trạng
/// thái máy in mới.
fn chay_theo_doi_may_in(
    nhan: mpsc::Receiver<LenhMayIn>,
    may_in: String,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
    duong_gui: Arc<DuongGui>,
    dang_in: Arc<DangInTu>,
    dung: Arc<AtomicBool>,
) {
    let gui = |v: Option<serde_json::Value>| {
        if let Some(v) = v {
            let ma = v.get("trangThai").and_then(|m| m.as_str()).unwrap_or("-").to_string();
            let kq = duong_gui.gui("trang-thai-may-in", v, CanHoTro::TrangThaiMayIn);
            nhat_ky::ghi("gui_trang_thai_may_in", &format!("{} gui_server={}", ma, kq.chu()));
        }
    };
    let mut bo = BoTheoDoiMayIn::default();
    // Lần đầu đọc NGAY: mở app lúc máy in đang hết giấy thì dải cảnh báo phải
    // hiện luôn, không đợi 20 giây (hay đợi backend gửi `cau-hinh`).
    let mut cho = Duration::ZERO;
    loop {
        let su_kien = nhan.recv_timeout(cho);
        if dung.load(Ordering::SeqCst) {
            return;
        }
        cho = CHU_KY_DOC_MAY_IN;
        let co_the_doc = nen_doc_may_in(*khoa(&dang_in), Instant::now());
        match bo.buoc(su_kien, co_the_doc) {
            BuocMayIn::Thoat => return,
            BuocMayIn::KhongLamGi => {}
            BuocMayIn::GhiTuWorker(ma, ct) => gui(ghi_trang_thai_may_in(&trang_thai, &may_in, ma, ct, false, false)),
            BuocMayIn::DocRanh { ep_gui } => match spooler::doc_tinh_trang_may_in(&may_in) {
                Some((ma, ct)) => gui(ghi_trang_thai_may_in(&trang_thai, &may_in, ma, ct, ep_gui, true)),
                // Không đọc được: giữ lời hứa ép gửi cho lần sau.
                None => bo.ep_gui_lan_ranh_toi |= ep_gui,
            },
            BuocMayIn::SauCauHinh => {
                ghi_nhat_ky_xa_hop_thu(&duong_gui.xa_hop_thu());
                // Không đọc được lúc này (đang in / spooler lỗi) thì gửi trạng
                // thái đã biết gần nhất — backend cần có trạng thái ngay khi nối.
                let doc = if co_the_doc { spooler::doc_tinh_trang_may_in(&may_in) } else { None };
                let (hien_tai, luc_ranh) = match doc {
                    Some(x) => (Some(x), true),
                    None => (khoa(&trang_thai).may_in.clone(), false),
                };
                if let Some((ma, ct)) = hien_tai {
                    gui(ghi_trang_thai_may_in(&trang_thai, &may_in, ma, ct, true, luc_ranh));
                }
            }
        }
    }
}

fn ghi_nhat_ky_xa_hop_thu(ds: &[(ThuDi, KetQuaGui)]) {
    for (thu, kq) in ds {
        let job = thu.gia_tri.get("jobId").and_then(|v| v.as_str()).map(job::rut_gon_job_id).unwrap_or_default();
        nhat_ky::ghi("gui_lai", &format!("{} job={} gui_server={}", thu.su_kien, job, kq.chu()));
    }
}

/// `chiTiet` của `su-co khong_xac_nhan` khi theo dõi tiếp mất dấu (R-C).
pub fn chu_mat_dau(so_hoa_don: &str) -> String {
    format!(
        "Hoá đơn {} không còn trong hàng đợi Windows mà app không thấy in (bị xoá/huỷ?) — kiểm khay giấy, in lại nếu chưa có",
        so_hoa_don
    )
}

/// Như `chu_mat_dau` cho hoá đơn đã xuống máy in USB mà máy không in (U3).
pub fn chu_mat_usb(so_hoa_don: &str) -> String {
    format!(
        "Hoá đơn {} đã gửi xuống máy in USB nhưng app không thấy máy in nó (máy bị tắt / lệnh bị huỷ trên máy?) — kiểm khay giấy, in lại nếu chưa có",
        so_hoa_don
    )
}

/// `chiTiet` khi job rời hàng đợi (đã có bằng chứng in) đúng lúc máy in báo
/// sự cố `ma` — có thể nằm trong bộ nhớ máy in, tự ra khi khắc phục. KHÔNG bảo
/// in lại ngay: in lại lúc này là hai tờ khi NV nạp giấy.
pub fn chu_co_the_trong_may_in(so_hoa_don: &str, ma: MaSuCo) -> String {
    format!(
        "Hoá đơn {} đã rời hàng đợi Windows đúng lúc máy in báo {} — có thể đang nằm trong bộ nhớ máy in; khắc phục xong đợi vài phút, chỉ in lại nếu vẫn không thấy ra",
        so_hoa_don,
        ma.nhan()
    )
}

/// `chiTiet` của `su-co khong_xac_nhan` khi theo dõi tiếp quá 12 giờ (R-C).
pub fn chu_het_han(so_hoa_don: &str) -> String {
    format!(
        "Hoá đơn {} theo dõi quá 12 giờ không thấy in (vẫn nằm trong hàng đợi Windows) — kiểm máy in và khay giấy, in lại nếu chưa có",
        so_hoa_don
    )
}

/// Như `chu_het_han` cho hoá đơn nằm trong BỘ NHỚ máy in USB (U3).
pub fn chu_het_han_usb(so_hoa_don: &str) -> String {
    format!(
        "Hoá đơn {} theo dõi quá 12 giờ, máy in USB vẫn chưa in xong (hoá đơn nằm trong bộ nhớ máy in) — kiểm máy in và khay giấy, in lại nếu chưa có",
        so_hoa_don
    )
}

/// Kết luận của luồng theo dõi tiếp (R3) → gửi `da_in` muộn / cập nhật giao
/// diện / ghi nhật ký. `da_in` chỉ gửi khi kết nối HIỆN TẠI có hoTro
/// `khong_ro` (`DuongGui` lọc) — backend cũ thì chỉ ghi nhật ký.
///
/// `Mat`/`HetHan` (R-C, giám sát vòng 2): gửi `su-co khong_xac_nhan` — trước
/// đây chỉ ghi file, ZaloCRM vẫn ghi "sẽ tự in ra — KHÔNG in lại" trong khi
/// NV đã xoá tay hàng đợi (đúng việc 24/09) → mất hoá đơn mà trạng thái nói
/// sai. `su-co` chỉ đi khi hoTro có `su_co` (`DuongGui` lọc).
fn xu_ly_ket_luan_tiep(
    j: JobTheoDoiTiep,
    kl: KetLuanTiep,
    may_in_mac_dinh: &str,
    trang_thai: &Mutex<TrangThaiChung>,
    gui: &HamGui<'_>,
) {
    let id_ngan = job::rut_gon_job_id(&j.job_id);
    let loai = j.loai.map_or("-", MaSuCo::ma);
    let may_in = if j.may_in.is_empty() { may_in_mac_dinh } else { j.may_in.as_str() };
    let co_the_trong_may_in = matches!(kl, KetLuanTiep::Mat(_)).then(|| j.co_the_trong_may_in()).flatten();
    let bao_mat = |chi_tiet: String| {
        khoa(trang_thai).mat_dau_job_theo(&j.job_id, &j.so_hoa_don, co_the_trong_may_in);
        let v = bao_cao::su_co(&j.job_id, MaSuCo::KhongXacNhan, Some(&chi_tiet), may_in, SystemTime::now());
        gui("su-co", v, CanHoTro::SuCo)
    };
    match kl {
        KetLuanTiep::DaIn => {
            let mut ket_qua = job::KetQua::da_in(j.job_id.clone());
            ket_qua.loi_cuoi = j.ghi_chu_da_in();
            let kq = match serde_json::to_value(ket_qua) {
                Ok(v) => gui("ket-qua", v, CanHoTro::KhongRo),
                Err(_) => KetQuaGui::Bo,
            };
            khoa(trang_thai).xac_nhan_in_sau(&j.job_id);
            nhat_ky::ghi(
                "theo_doi_tiep_da_in",
                &format!("job={} hoa_don={} loai_truoc={} gui_server={}", id_ngan, j.so_hoa_don, loai, kq.chu()),
            );
        }
        KetLuanTiep::Mat(ly_do) => {
            let kq = bao_mat(match co_the_trong_may_in {
                Some(ma) => chu_co_the_trong_may_in(&j.so_hoa_don, ma),
                None if j.la_qua_usb() => chu_mat_usb(&j.so_hoa_don),
                None => chu_mat_dau(&j.so_hoa_don),
            });
            nhat_ky::ghi(
                "theo_doi_tiep_mat",
                &format!("job={} hoa_don={} loai_truoc={} gui_server={} {}", id_ngan, j.so_hoa_don, loai, kq.chu(), ly_do),
            );
        }
        KetLuanTiep::HetHan => {
            let kq = bao_mat(if j.la_qua_usb() { chu_het_han_usb(&j.so_hoa_don) } else { chu_het_han(&j.so_hoa_don) });
            nhat_ky::ghi(
                "theo_doi_tiep_het_han",
                &format!("job={} hoa_don={} loai_truoc={} gui_server={}", id_ngan, j.so_hoa_don, loai, kq.chu()),
            );
        }
    }
}

/// Luồng theo dõi tiếp của MỘT lần chạy mạng. Danh sách (`kho`) sống qua lần
/// bấm Lưu (R-E(1)): luồng mới nhận quyền, luồng này tự thoát khi mất quyền
/// hoặc cờ dừng bật — job đang theo dõi KHÔNG bị bỏ.
fn chay_theo_doi_tiep(
    kho: Arc<KhoTheoDoiTiep>,
    may_in: String,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
    duong_gui: Arc<DuongGui>,
    dung: Arc<AtomicBool>,
) {
    let gui = |su_kien: &'static str, v: serde_json::Value, can: CanHoTro| duong_gui.gui(su_kien, v, can);
    theo_doi_tiep::chay_vong_lap(
        &kho,
        &may_in,
        &dung,
        &mut |may| spooler::mo_spooler(may).doc_vong(),
        &mut |d| std::thread::sleep(d),
        &Instant::now,
        &mut |j, kl| xu_ly_ket_luan_tiep(j, kl, &may_in, &trang_thai, &gui),
    );
}

// ===================== Một bản app, một kết nối (R7, R-H) =====================

thread_local! {
    /// Luồng phụ ĐANG gọi `disconnect()` cho client đã nghỉ: callback "close"
    /// chạy trên luồng này thì cứ để nó trả về (không kết thúc luồng phụ).
    static LA_LUONG_NGAT: Cell<bool> = const { Cell::new(false) };
}

/// Cờ của MỘT client socket.io (R-H, giám sát vòng 2).
#[derive(Debug, Default)]
struct CoClient {
    /// Vòng canh đã cho client này nghỉ — mọi callback về sau của nó là thừa.
    nghi: AtomicBool,
    /// Luồng poll của client gặp lỗi/đóng và đã tự kết thúc — vòng ngoài nối
    /// lại NGAY (không chờ 60 s).
    chet: AtomicBool,
    /// Đã từng "open" — lần nối lại kế tiếp bắt đầu backoff từ đầu.
    da_mo: AtomicBool,
    /// Cờ dừng của LẦN CHẠY MẠNG sở hữu client này (bấm Lưu, T7). Bật = client
    /// đã bị thay thế: coi như đã nghỉ. Ca thật: `connect()` treo >10 s, bấm
    /// Lưu, lần chạy mới đã nối; client cũ nối xong SAU ĐÓ — nếu nó còn "open"
    /// bình thường thì nó giành cổng gửi của `DuongGui`, nhận job trong khi
    /// worker cũ đã thoát (`mat_job` — hoá đơn mất), và `cho_nghi` của nó xoá
    /// luôn cổng của kết nối mới.
    dung_net: Arc<AtomicBool>,
}

/// Payload riêng của cú "thoát luồng poll" — để nhận ra trong test.
struct ThoatLuongPoll;

/// Kết thúc LUỒNG POLL của client đang chạy callback này (R-H, R-I).
///
/// VÌ SAO (đọc nguồn rust_socketio 0.6 `client.rs::poll_callback`): luồng poll
/// lặp `for packet in iter()` và `Iter::next` KHÔNG BAO GIỜ trả `None` khi
/// transport chết — `poll()` lỗi liền tức thì. Với `.reconnect(false)` luồng
/// đó quay rỗng 100% một nhân; với `.reconnect(true)` thư viện tự nối lại
/// (backoff hết sau ~15 phút rồi cũng quay rỗng), và client ĐÃ NGHỈ vẫn tự nối
/// lại thành kết nối ma — đè đăng ký của kết nối sống ở backend (R-H). Bản
/// trước "đỗ" luồng bằng `park()` mãi mãi: token sai → CONNECT_ERROR mỗi phút
/// rò 2 luồng + 1 RawClient (R-I, ≈2.900 luồng/ngày).
///
/// Nay: `resume_unwind` từ trong callback — gỡ ngăn xếp luồng poll tới đáy
/// rồi luồng KẾT THÚC (không gọi panic hook, không in gì; Cargo.toml không đặt
/// `panic = "abort"`). Khoá `on` của client bị nhiễm độc (poison) → mọi callback
/// về sau (vd "close" khi `disconnect()` ở luồng khác) trả `Err` ngay, không treo.
fn thoat_luong_poll() -> ! {
    std::panic::resume_unwind(Box::new(ThoatLuongPoll))
}

/// Gọi ĐẦU mọi callback: client đã nghỉ (hoặc lần chạy mạng của nó đã bị
/// thay — T7) thì (trên luồng poll của nó) kết thúc luồng poll; (trên luồng
/// ngắt của ta) báo callback thôi làm gì. `false` = client còn dùng, callback
/// chạy bình thường.
fn da_nghi(co: &CoClient) -> bool {
    if !co.nghi.load(Ordering::SeqCst) && !co.dung_net.load(Ordering::SeqCst) {
        return false;
    }
    if LA_LUONG_NGAT.with(|c| c.get()) {
        return true;
    }
    thoat_luong_poll()
}

/// Client mất kết nối (lỗi transport, CONNECT_ERROR, server đóng namespace):
/// bật cờ `chet` rồi kết thúc luồng poll — vòng ngoài dựng client MỚI với
/// backoff (R-H). Trên luồng ngắt của ta thì chỉ bật cờ.
fn client_chet(co: &CoClient) {
    co.chet.store(true, Ordering::SeqCst);
    if !LA_LUONG_NGAT.with(|c| c.get()) {
        thoat_luong_poll()
    }
}

/// Chữ của payload lỗi (thư viện đưa `err.to_string()` vào payload).
fn chu_payload(p: &Payload) -> String {
    match p {
        Payload::Text(vals) => vals
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
            .collect::<Vec<_>>()
            .join(" "),
        Payload::Binary(_) => String::new(),
        #[allow(deprecated)]
        Payload::String(s) => s.clone(),
    }
}

/// CONNECT_ERROR (server từ chối ở middleware — token sai/thu hồi, DB lỗi) →
/// lý do server gửi (`message` nếu là JSON). `None` = lỗi khác.
fn ly_do_tu_choi(chu: &str) -> Option<String> {
    let (_, sau) = chu.split_once("ConnectError frame:")?;
    let sau = sau.trim();
    let ly_do = serde_json::from_str::<serde_json::Value>(sau)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| sau.trim_matches('"').to_string());
    Some(ly_do)
}

/// Chờ trước lần nối lại thứ `lan` (0 = lần đầu sau khi mất / sau lần "open"
/// gần nhất): 1 s, 2 s, 4 s … trần 30 s, cộng jitter 0–25 % (`ngau_nhien`) để
/// nhiều máy không cùng nối dồn vào backend vừa khởi động lại (R-H).
fn cho_noi_lai(lan: u32, ngau_nhien: u64) -> Duration {
    let goc_ms = 1_000u64.saturating_mul(1u64 << lan.min(5)).min(30_000);
    Duration::from_millis(goc_ms + ngau_nhien % (goc_ms / 4 + 1))
}

/// Số ngẫu nhiên cho jitter — không cần crate `rand`: `RandomState` của std
/// mang khoá ngẫu nhiên mỗi tiến trình.
fn so_ngau_nhien() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(Instant::now())
}

/// Cho một client nghỉ HẲN trước khi dựng client mới (R7).
///
/// `Client` của rust_socketio 0.6 không có Drop, luồng poll của thư viện giữ
/// bản clone. Thứ tự: (1) bật cờ nghỉ — callback nào của client này còn chạy
/// trên luồng poll của nó thì kết thúc luồng đó (`da_nghi`); (2) bỏ cổng gửi
/// của nó khỏi `DuongGui`; (3) `disconnect()` trên luồng phụ, chờ tối đa
/// `CHO_NGAT_CLIENT_CU` (transport treo thì luồng phụ tự xong khi mạng hết
/// hạn — không bao giờ kẹt mãi: khoá callback không còn ai giữ).
fn cho_nghi(client: Client, co: &CoClient, the_he: u64, duong_gui: &DuongGui) {
    co.nghi.store(true, Ordering::SeqCst);
    duong_gui.dong_ket_noi(the_he);
    let (xong, cho_xong) = mpsc::channel::<()>();
    let da_spawn = std::thread::Builder::new().name("ngat-client-cu".into()).spawn(move || {
        LA_LUONG_NGAT.with(|c| c.set(true));
        let _ = client.disconnect();
        let _ = xong.send(());
    });
    if da_spawn.is_err() || cho_xong.recv_timeout(CHO_NGAT_CLIENT_CU).is_err() {
        nhat_ky::ghi("ngat_client_cham", "disconnect() client cu khong xong trong 3 s");
    }
}

/// Lý do vòng canh client thoát.
#[derive(Debug, PartialEq, Eq)]
enum LyDoThoat {
    /// Cờ dừng bật (bấm Lưu).
    Dung,
    /// Luồng poll của client đã kết thúc vì lỗi/đóng (R-H) — nối lại ngay.
    ClientChet,
    /// `da_noi=false` liên tục quá `NGUONG_CHET_HAN` mà không callback nào báo.
    ChetHan,
}

/// Vòng canh MỘT client đang sống: mỗi `NHIP_CANH` xem cờ dừng, chạy
/// `moi_nhip` (kiểm server bản cũ, R12), xem client đã chết chưa (R-H), đếm
/// thời gian `da_noi=false` LIÊN TỤC — lưới cho ca không có callback nào.
/// Tách hàm với đồng hồ/ngủ tiêm được để test.
fn canh_client(
    dung: &AtomicBool,
    da_noi: &dyn Fn() -> bool,
    chet: &dyn Fn() -> bool,
    moi_nhip: &mut dyn FnMut(),
    ngu: &mut dyn FnMut(Duration),
    bay_gio: &dyn Fn() -> Instant,
) -> LyDoThoat {
    let mut mat_tu: Option<Instant> = None;
    loop {
        if dung.load(Ordering::SeqCst) {
            return LyDoThoat::Dung;
        }
        ngu(NHIP_CANH);
        if dung.load(Ordering::SeqCst) {
            return LyDoThoat::Dung;
        }
        moi_nhip();
        if chet() {
            return LyDoThoat::ClientChet;
        }
        if da_noi() {
            mat_tu = None;
            continue;
        }
        let tu = *mat_tu.get_or_insert_with(bay_gio);
        if bay_gio().saturating_duration_since(tu) >= NGUONG_CHET_HAN {
            return LyDoThoat::ChetHan;
        }
    }
}

/// Ngủ `tong` nhưng thức dậy sớm khi cờ dừng bật.
fn ngu_co_the_dung(tong: Duration, dung: &AtomicBool) {
    let mut con = tong;
    while !con.is_zero() && !dung.load(Ordering::SeqCst) {
        let buoc = con.min(NHIP_CANH);
        std::thread::sleep(buoc);
        con -= buoc;
    }
}

/// R12: kết nối mở 10 s mà không có `cau-hinh` → backend bản cũ.
fn kiem_server_ban_cu(duong_gui: &DuongGui, trang_thai: &Mutex<TrangThaiChung>) {
    if duong_gui.kiem_ban_cu(Instant::now()) {
        eprintln!("[print-agent] server không gửi cau-hinh — bản cũ");
        nhat_ky::ghi(
            "server_ban_cu",
            "10 s sau khi noi khong nhan cau-hinh — giu hanh vi cu: khong gui khong_ro/su-co/trang-thai-may-in",
        );
        khoa(trang_thai).server_ban_cu = true;
        ghi_nhat_ky_xa_hop_thu(&duong_gui.xa_hop_thu());
    }
}

/// Việc lúc khởi động — CHỈ lần chạy đầu của tiến trình, chạy trên luồng net
/// TRƯỚC khi có worker in:
/// 1. R11b: job CỦA APP kẹt "Paused" (app bị tắt giữa lúc tạm dừng và xoá) →
///    cho chạy tiếp, ghi nhật ký.
/// 2. R-E(2): job của app còn trong hàng đợi (không Paused) → theo dõi tiếp,
///    in xong thì báo `da_in` trễ.
///
/// VÌ SAO chỉ lần đầu: lúc bấm Lưu, worker của bản cũ có thể đang ở giữa "tạm
/// dừng → đọc lại → xoá" của chính nó; cho job đó chạy tiếp đúng khe đó là có
/// byte tới máy in rồi mới bị xoá + báo `loi` → in đôi. (Danh sách theo dõi
/// tiếp thì đã sống qua lần Lưu, không cần nhận lại.)
///
/// Cả hai CHỈ đụng job do CHÍNH máy này nộp (T8, `spooler::cung_may`): hàng
/// đợi chia sẻ `\\PC\may` có job của máy khác.
fn viec_khi_khoi_dong(may_in: &str, kho: &KhoTheoDoiTiep) {
    let ten_may = bao_cao::ten_may_tinh();
    let mut sp = spooler::mo_spooler(may_in);
    for (id, document, kq) in spooler::tiep_tuc_job_bi_dung_cua_app(&mut *sp, &ten_may) {
        nhat_ky::ghi(
            "tiep_tuc_job_khi_khoi_dong",
            &format!("job_windows={} ten={} ket_qua={}", id, document, kq.err().unwrap_or_else(|| "ok".into())),
        );
    }
    // Đọc lại SAU bước resume: job vừa được cho chạy tiếp giờ không còn Paused.
    let vong = sp.doc_vong();
    let bay_gio_epoch =
        SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let nl = theo_doi_tiep::nhan_lai_khi_khoi_dong(&vong, may_in, &ten_may, Instant::now(), bay_gio_epoch);
    for ten in nl.bo_qua {
        nhat_ky::ghi("theo_doi_tiep_bo_qua", &format!("khong tach duoc jobId dung dang backend tu ten tai lieu: {}", ten));
    }
    if nl.may_khac > 0 {
        nhat_ky::ghi(
            "theo_doi_tiep_bo_qua",
            &format!("{} job cua app do MAY KHAC nop (hang doi chia se) — khong nhan lai; may nay={}", nl.may_khac, ten_may),
        );
    }
    for j in nl.nhan {
        nhat_ky::ghi("theo_doi_tiep_nhan_lai", &format!("job={} hoa_don={}", job::rut_gon_job_id(&j.job_id), j.so_hoa_don));
        dua_vao_theo_doi_tiep(kho, j);
    }
}

/// Điều khiển MỘT lần chạy `chay_net` — để bấm Lưu dừng HẲN bản cũ (R7a).
pub struct DieuKhienNet {
    dung: Arc<AtomicBool>,
    /// Đóng (Disconnected) khi luồng `chay_net` đã thoát.
    da_dung: mpsc::Receiver<()>,
    /// Danh sách theo dõi tiếp — CHUYỂN sang lần chạy mới khi bấm Lưu (R-E(1)).
    theo_doi: Arc<KhoTheoDoiTiep>,
}

impl DieuKhienNet {
    /// Bật cờ dừng, chờ tối đa `cho` cho luồng cũ dọn xong. `true` = đã dừng.
    fn dung_va_cho(self, cho: Duration) -> bool {
        self.dung.store(true, Ordering::SeqCst);
        !matches!(self.da_dung.recv_timeout(cho), Err(RecvTimeoutError::Timeout))
    }
}

/// Chạy `chay_net` trên luồng riêng. Có `cu` (bấm Lưu) thì luồng mới DỪNG HẲN
/// bản cũ trước — cờ dừng + disconnect client cũ + dừng các luồng theo dõi —
/// rồi mới nối (R7a); danh sách theo dõi tiếp của bản cũ được giữ nguyên và
/// chuyển sang bản mới (R-E(1)). Việc chờ nằm trên luồng mới, giao diện không
/// bị chặn.
pub fn khoi_chay(
    cfg: Arc<Config>,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
    duong_gui: Arc<DuongGui>,
    cu: Option<DieuKhienNet>,
) -> DieuKhienNet {
    let dung = Arc::new(AtomicBool::new(false));
    let (bao_da_dung, da_dung) = mpsc::channel::<()>();
    let theo_doi = cu.as_ref().map_or_else(|| Arc::new(KhoTheoDoiTiep::default()), |c| c.theo_doi.clone());
    // Bật cờ dừng của bản cũ NGAY (đồng bộ, trước khi giao diện đặt lại trạng
    // thái dùng chung — T7): từ đây mọi luồng/callback của bản cũ thôi ghi
    // trạng thái kết nối. Việc CHỜ nó dừng hẳn vẫn nằm trên luồng mới.
    if let Some(c) = &cu {
        c.dung.store(true, Ordering::SeqCst);
    }
    let (d, kho) = (dung.clone(), theo_doi.clone());
    std::thread::Builder::new()
        .name("net".into())
        .spawn(move || {
            // Giữ Sender tới hết luồng: drop (thoát hay panic) = báo "đã dừng".
            let _bao = bao_da_dung;
            // Chỉ lần chạy ĐẦU (không có worker nào khác trong tiến trình) mới
            // được cho chạy tiếp job bị dừng (R11b) — xem `viec_khi_khoi_dong`.
            let lan_dau = cu.is_none();
            if let Some(cu) = cu {
                if !cu.dung_va_cho(CHO_NET_CU_DUNG) {
                    nhat_ky::ghi("net_cu_cham_dung", "chay_net cu chua dung sau 10 s — van noi ket noi moi");
                }
            }
            if lan_dau {
                // Tên file job (backend cũ) chứa token — che TRƯỚC khi ghi nhật ký.
                nhat_ky::che_bi_mat(&cfg.token);
                viec_khi_khoi_dong(&cfg.printer_name, &kho);
            }
            chay_net(cfg, trang_thai, duong_gui, kho, d);
        })
        .expect("không spawn được luồng net");
    DieuKhienNet { dung, da_dung, theo_doi }
}

/// Chạy vòng đời kết nối socket.io — GỌI TỪ THREAD RIÊNG (qua `khoi_chay`).
/// Mỗi lần đổi trạng thái (nối/mất/job xong/lỗi) đều cập nhật `trang_thai`
/// để UI (thread khác) đọc thấy ngay ở frame kế tiếp. Thoát khi `dung` bật.
/// Giá trị ack ĐẦU TIÊN server trả (`ack({ok:true})` → `{ok:true}`).
///
/// rust_socketio 0.6 (`handle_ack`) dựng `Payload::from(packet.data)` — data của
/// gói ack là chuỗi MẢNG các đối số (`[{"ok":true}]`), nên payload thành
/// `Text([ Array([{ok:true}]) ])`: phải bóc thêm một lớp mảng. Lấy thẳng phần tử
/// đầu (bản đầu 0.2.4) là thấy `[...]`, không có `ok` → coi như hỏng → gửi lại
/// cùng lô mãi (soát trước khi lên prod 25/09).
fn gia_tri_ack(p: Payload) -> serde_json::Value {
    let dau = match p {
        Payload::Text(mut ds) if !ds.is_empty() => ds.swap_remove(0),
        #[allow(deprecated)]
        Payload::String(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    };
    match dau {
        serde_json::Value::Array(mut ds) if !ds.is_empty() => ds.swap_remove(0),
        v => v,
    }
}

/// Số dòng tối đa một lô `nhat-ky-app` (hợp đồng: 1..500).
const LO_NHAT_KY: usize = 500;
/// Nhịp gửi nhật ký khi rảnh; còn nhiều dòng thì gửi lô kế ngay.
const NHIP_NHAT_KY: Duration = Duration::from_secs(3);
/// Chờ ack của backend (ghi DB xong mới ack).
const CHO_ACK_NHAT_KY: Duration = Duration::from_secs(15);

/// Payload `nhat-ky-app` của một lô.
fn payload_nhat_ky(lo: &[nhat_ky::DongGui], bo_qua: u64) -> serde_json::Value {
    let dong: Vec<serde_json::Value> = lo
        .iter()
        .map(|d| serde_json::json!({ "luc": crate::thoi_gian::iso_utc(d.luc), "suKien": d.su_kien, "noiDung": d.noi_dung }))
        .collect();
    serde_json::json!({ "dong": dong, "boQua": bo_qua, "phienBan": env!("CARGO_PKG_VERSION") })
}

/// Luồng gửi nhật ký: lấy một lô từ bộ đệm (nhat_ky.rs), gửi kèm ack; không
/// có ack `ok:true` (chưa kết nối, backend cũ, quá tải, chưa migrate…) thì TRẢ
/// LẠI lô vào đầu bộ đệm và nghỉ lâu dần (3 → 60 s). Chỉ ghi nhật ký khi tình
/// trạng gửi ĐỔI — không một dòng mỗi lần thử (dòng đó cũng sẽ được gửi lên).
fn chay_gui_nhat_ky(duong_gui: Arc<DuongGui>, dung: Arc<AtomicBool>) {
    let mut nghi = NHIP_NHAT_KY;
    let mut loi_truoc: Option<String> = None;
    while !dung.load(Ordering::SeqCst) {
        let (lo, bo_qua) = nhat_ky::lay_lo_gui(LO_NHAT_KY);
        if lo.is_empty() && bo_qua == 0 {
            ngu_tung_khuc(&dung, NHIP_NHAT_KY);
            continue;
        }
        let ket_qua = duong_gui
            .gui_ack("nhat-ky-app", payload_nhat_ky(&lo, bo_qua), CanHoTro::NhatKyApp, CHO_ACK_NHAT_KY)
            .and_then(|v| {
                if v.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                    Ok(())
                } else {
                    Err(v.get("loi").and_then(serde_json::Value::as_str).unwrap_or("ack khong ok").to_string())
                }
            });
        match ket_qua {
            Ok(()) => {
                if loi_truoc.take().is_some() {
                    nhat_ky::ghi("gui_nhat_ky", "gui nhat ky len server tro lai binh thuong");
                }
                nghi = NHIP_NHAT_KY;
                if lo.len() < LO_NHAT_KY {
                    ngu_tung_khuc(&dung, NHIP_NHAT_KY);
                }
            }
            Err(loi) => {
                nhat_ky::tra_lai_gui(lo, bo_qua);
                if loi_truoc.as_deref() != Some(loi.as_str()) {
                    nhat_ky::ghi("gui_nhat_ky_loi", &loi);
                    loi_truoc = Some(loi);
                }
                ngu_tung_khuc(&dung, nghi);
                nghi = (nghi * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// Hỏi `lay-hang-doi` NGAY khi kết nối mới báo hỗ trợ `hang_doi`, rồi mỗi
/// `NHIP_LAM_MOI`. Gửi thẳng (không hộp thư đi) — mất kết nối thì thôi.
fn chay_lam_moi_hang_doi(duong_gui: Arc<DuongGui>, dung: Arc<AtomicBool>) {
    let mut the_he_da_hoi: Option<u64> = None;
    let mut luc_hoi = Instant::now();
    while !dung.load(Ordering::SeqCst) {
        if let Some(the_he) = duong_gui.the_he_ho_tro(CanHoTro::HangDoi) {
            if the_he_da_hoi != Some(the_he) || luc_hoi.elapsed() >= hang_doi::NHIP_LAM_MOI {
                let _ = duong_gui.gui_ngay("lay-hang-doi", serde_json::json!({}), CanHoTro::HangDoi);
                the_he_da_hoi = Some(the_he);
                luc_hoi = Instant::now();
            }
        }
        ngu_tung_khuc(&dung, Duration::from_secs(1));
    }
}

/// Gửi yêu cầu huỷ / bỏ theo dõi cho `muc` (TUẦN TỰ, một luồng riêng — không
/// bao giờ trên luồng giao diện hay callback socket). Mỗi hoá đơn: nhật ký
/// `huy_yeu_cau` → hỏi server (hỏi lại khi hết giờ) → kết cục vào trạng thái
/// + nhật ký `huy_ket_qua`/`bo_theo_doi` (`ok=true`/`ok=false` đầu dòng, §8.9).
///
/// Xong hết thì hỏi ảnh chụp mới.
pub fn khoi_chay_yeu_cau_hang_doi(
    duong_gui: Arc<DuongGui>,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
    loai: hang_doi::LoaiViec,
    muc: Vec<bao_cao::MucHangDoi>,
) {
    if muc.is_empty() {
        return;
    }
    let ids: Vec<String> = muc.iter().map(|m| m.id.clone()).collect();
    let (dg, tt) = (duong_gui.clone(), trang_thai.clone());
    let da_spawn = std::thread::Builder::new().name("yeu-cau-hang-doi".into()).spawn(move || {
        for m in &muc {
            let (su_kien_yc, su_kien_kq) = match loai {
                hang_doi::LoaiViec::Huy => ("huy_yeu_cau", "huy_ket_qua"),
                hang_doi::LoaiViec::BoTheoDoi => ("bo_theo_doi_yeu_cau", "bo_theo_doi"),
            };
            nhat_ky::ghi(su_kien_yc, &format!("so={} id={}", m.so_hoa_don, m.id));
            let kc = hang_doi::gui_yeu_cau(
                &dg,
                loai,
                &m.id,
                &mut |lan| khoa(&tt).hang_doi.bao_lan(&m.id, lan),
                &mut |d| std::thread::sleep(d),
                &Instant::now,
            );
            nhat_ky::ghi(su_kien_kq, &kc.dong_nhat_ky(m));
            let mut t = khoa(&tt);
            t.hang_doi.ket_thuc(&m.id, loai, &kc, Instant::now());
            if loai == hang_doi::LoaiViec::Huy && matches!(kc, hang_doi::KetCuc::Duoc { .. }) {
                t.ghi_da_huy(&m.id, &m.so_hoa_don, m.ten_khach.clone(), gio_hien_tai());
            }
        }
        let _ = dg.gui_ngay("lay-hang-doi", serde_json::json!({}), CanHoTro::HangDoi);
    });
    if da_spawn.is_err() {
        // Không có luồng thì không gửi gì — nói thẳng là CHƯA làm.
        let mut t = khoa(&trang_thai);
        for id in &ids {
            t.hang_doi.ket_thuc(id, loai, &hang_doi::KetCuc::ChuaGui { ly_do: "khong tao duoc luong".into() }, Instant::now());
        }
    }
}

/// Ngủ `tong` theo từng khúc 500 ms — cờ dừng (bấm Lưu) có hiệu lực nhanh.
fn ngu_tung_khuc(dung: &AtomicBool, tong: Duration) {
    let mut con = tong;
    while !con.is_zero() && !dung.load(Ordering::SeqCst) {
        let khuc = con.min(Duration::from_millis(500));
        std::thread::sleep(khuc);
        con = con.saturating_sub(khuc);
    }
}

fn chay_net(
    cfg: Arc<Config>,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
    duong_gui: Arc<DuongGui>,
    kho_theo_doi: Arc<KhoTheoDoiTiep>,
    dung: Arc<AtomicBool>,
) {
    eprintln!(
        "[print-agent] khởi động — server={} printer={:?} tray={} paper={}",
        cfg.server_url, cfg.printer_name, cfg.tray, cfg.paper_size
    );
    // Token không bao giờ được vào file nhật ký (§0.3) — đăng ký để luồng ghi che.
    nhat_ky::che_bi_mat(&cfg.token);
    nhat_ky::ghi(
        "khoi_dong",
        &format!(
            "phien_ban={} may_in={} khay={} kho_giay={} dang_theo_doi_tiep={}",
            env!("CARGO_PKG_VERSION"),
            cfg.printer_name,
            cfg.tray,
            cfg.paper_size,
            kho_theo_doi.so_job()
        ),
    );

    let ten_may = bao_cao::ten_may_tinh();
    let dang_in: Arc<DangInTu> = Arc::new(Mutex::new(None));

    // Luồng theo dõi máy in (hợp đồng §2 "trang-thai-may-in").
    let (gui_may_in, nhan_may_in) = mpsc::channel::<LenhMayIn>();
    {
        let may_in = cfg.printer_name.clone();
        let (tt, dg, di, d) = (trang_thai.clone(), duong_gui.clone(), dang_in.clone(), dung.clone());
        if let Err(e) = std::thread::Builder::new()
            .name("theo-doi-may-in".into())
            .spawn(move || chay_theo_doi_may_in(nhan_may_in, may_in, tt, dg, di, d))
        {
            // Không có luồng này thì chỉ mất báo trạng thái lúc rảnh — việc in vẫn chạy.
            eprintln!("[print-agent] không spawn được luồng theo dõi máy in: {}", e);
        }
    }

    // Luồng theo dõi tiếp job `khong_ro` còn trong hàng đợi (R3) — MỘT luồng,
    // nhận quyền chủ danh sách dùng chung (R-E(1)).
    {
        let may_in = cfg.printer_name.clone();
        let (k, tt, dg, d) = (kho_theo_doi.clone(), trang_thai.clone(), duong_gui.clone(), dung.clone());
        if let Err(e) = std::thread::Builder::new()
            .name("theo-doi-tiep".into())
            .spawn(move || chay_theo_doi_tiep(k, may_in, tt, dg, d))
        {
            eprintln!("[print-agent] không spawn được luồng theo dõi tiếp: {}", e);
        }
    }

    // Luồng gửi nhật ký cục bộ lên ZaloCRM (0.2.4, chủ yêu cầu 25/09).
    {
        let (dg, d) = (duong_gui.clone(), dung.clone());
        if let Err(e) = std::thread::Builder::new().name("gui-nhat-ky".into()).spawn(move || chay_gui_nhat_ky(dg, d)) {
            eprintln!("[print-agent] không spawn được luồng gửi nhật ký: {}", e);
        }
    }

    // Luồng hỏi ảnh chụp hàng đợi (v5.1 §8.7): ngay khi kết nối mới báo hỗ trợ,
    // rồi mỗi `NHIP_LAM_MOI` — server chỉ đẩy khi đổi, app tự biết ảnh còn tươi.
    {
        let (dg, d) = (duong_gui.clone(), dung.clone());
        if let Err(e) = std::thread::Builder::new().name("hang-doi".into()).spawn(move || chay_lam_moi_hang_doi(dg, d)) {
            eprintln!("[print-agent] không spawn được luồng hàng đợi: {}", e);
        }
    }

    // HÀNG ĐỢI IN + WORKER (18/09) — xem doc-comment của `chay_worker_in`.
    // Callback socket.io CHỈ đẩy payload vào kênh rồi trả về NGAY; mọi việc
    // nặng (Sumatra + poll spooler tới 15s) chạy ở worker thread riêng.
    let (gui_job, nhan_job) = mpsc::channel::<ViecIn>();
    {
        let (c, tt, di, gm, dg, k, d) = (
            cfg.clone(),
            trang_thai.clone(),
            dang_in.clone(),
            gui_may_in.clone(),
            duong_gui.clone(),
            kho_theo_doi.clone(),
            dung.clone(),
        );
        std::thread::Builder::new()
            .name("in-worker".into())
            .spawn(move || chay_worker_in(nhan_job, c, tt, di, gm, dg, k, d))
            .expect("không spawn được thread in-worker");
    }

    // auth {token} — khớp handshake server đọc socket.handshake.auth (server
    // tra token trong bảng print_agents → biết máy nào + chi nhánh nào).
    let auth = serde_json::json!({ "token": cfg.token });

    // Vòng NGOÀI: dựng client + connect() mỗi khi client trước chết. Tự nối
    // lại của thư viện TẮT (`.reconnect(false)`, R-H) — vòng này là đường nối
    // lại DUY NHẤT, backoff 1→30 s + jitter, đặt lại sau mỗi lần "open".
    let mut lan_noi_lai: u32 = 0;
    // Lỗi nối gần nhất đã ghi nhật ký — mất mạng cả ngày chỉ ra một dòng mỗi
    // loại lỗi, không một dòng mỗi lần thử.
    let mut loi_noi_da_ghi: Option<String> = None;
    while !dung.load(Ordering::SeqCst) {
        eprintln!("[print-agent] đang nối {} ...", cfg.server_url);
        // Mỗi client một bộ cờ + một ô thế hệ kết nối (xem `cho_nghi`).
        let co = Arc::new(CoClient { dung_net: dung.clone(), ..CoClient::default() });
        let the_he = Arc::new(AtomicU64::new(0));

        let on_job = {
            let (gj, co) = (gui_job.clone(), co.clone());
            move |payload: Payload, _socket: RawClient| {
                let val = payload_dau(payload);
                // Job đã tới thì in — kể cả trên client vừa nghỉ (server tưởng đã
                // giao). Worker chết (panic) thì kênh đứt — log ra, KHÔNG emit gì
                // (server tự suy khong_ro, không retry mù).
                if gj.send(ViecIn { val }).is_err() {
                    eprintln!("print_unknown job_uuid= reason=worker_in_da_dung");
                    nhat_ky::ghi("mat_job", "worker in da dung — job khong duoc in (backend se het gio cho)");
                }
                let _ = da_nghi(&co);
            }
        };
        let on_error = {
            let (tt, co) = (trang_thai.clone(), co.clone());
            move |err: Payload, _socket: RawClient| {
                if da_nghi(&co) {
                    return;
                }
                let chu = chu_payload(&err);
                eprintln!("[print-agent] lỗi socket: {}", chu);
                {
                    let mut t = khoa(&tt);
                    // Chỉ ghi nhật ký lúc CHUYỂN từ nối sang mất — mất mạng một
                    // ngày không được ra hàng vạn dòng giống nhau.
                    if t.da_noi {
                        nhat_ky::ghi("mat_ket_noi", &chu);
                    }
                    t.da_noi = false;
                    t.thong_bao_cuoi = Some(format!("lỗi socket: {}", chu));
                    // R-I: server từ chối (token sai/thu hồi, DB lỗi) — nói MỘT
                    // lần (nhật ký + dòng nhỏ trên giao diện), không mỗi lần thử.
                    if let Some(ly_do) = ly_do_tu_choi(&chu) {
                        if t.tu_choi_ket_noi.as_deref() != Some(ly_do.as_str()) {
                            nhat_ky::ghi("tu_choi_ket_noi", &ly_do);
                            t.tu_choi_ket_noi = Some(ly_do);
                        }
                    }
                }
                client_chet(&co);
            }
        };
        let on_open = {
            let (tt, dg, co, th, c, tm) =
                (trang_thai.clone(), duong_gui.clone(), co.clone(), the_he.clone(), cfg.clone(), ten_may.clone());
            move |_: Payload, socket: RawClient| {
                // Client đã bị thay (bấm Lưu lúc `connect()` còn treo — T7):
                // KHÔNG đăng ký gì (không giành cổng gửi, không `da_noi`), kết
                // thúc luồng poll ngay — không nhận job nào; vòng ngoài thấy cờ
                // dừng và `disconnect()` nó.
                if da_nghi(&co) {
                    return;
                }
                co.da_mo.store(true, Ordering::SeqCst);
                eprintln!("[print-agent] đã nối server");
                {
                    let mut t = khoa(&tt);
                    t.da_noi = true;
                    t.thong_bao_cuoi = None;
                    t.tu_choi_ket_noi = None;
                    // Ảnh chụp hàng đợi cũ chỉ để xem tới khi kết nối này gửi ảnh mới.
                    t.hang_doi.ket_noi_moi();
                }
                // Kết nối MỚI: quên hoTro của kết nối trước (§2); mọi event sau
                // đây đi qua socket này (R4).
                let cong: Arc<dyn CongGui> = Arc::new(CongSocket(Mutex::new(socket.clone())));
                th.store(dg.mo_ket_noi(cong, Instant::now()), Ordering::SeqCst);
                nhat_ky::ghi("ket_noi", &format!("server={}", c.server_url));
                // Backend cũ bỏ qua event lạ — gửi không cần hoTro (§2).
                let _ = socket.emit("thong-tin-app", bao_cao::thong_tin_app(&c, &tm));
            }
        };
        let on_close = {
            let (tt, co) = (trang_thai.clone(), co.clone());
            move |_: Payload, _socket: RawClient| {
                if da_nghi(&co) {
                    return;
                }
                // Server ngắt namespace: kết nối này xong — vòng ngoài nối lại.
                // KHÔNG đụng `DuongGui` ở đây: `cho_nghi` bỏ cổng đúng thế hệ.
                eprintln!("[print-agent] server đóng kết nối");
                nhat_ky::ghi("mat_ket_noi", "server dong ket noi");
                khoa(&tt).da_noi = false;
                client_chet(&co);
            }
        };
        let on_cau_hinh = {
            let (tt, dg, co, gm) = (trang_thai.clone(), duong_gui.clone(), co.clone(), gui_may_in.clone());
            move |payload: Payload, _socket: RawClient| {
                if da_nghi(&co) {
                    return;
                }
                let ho_tro = bao_cao::doc_cau_hinh(&payload_dau(payload));
                dg.nhan_cau_hinh(ho_tro);
                {
                    let mut t = khoa(&tt);
                    t.server_ban_cu = false;
                    t.hang_doi.nhan_cau_hinh(ho_tro.hang_doi);
                }
                eprintln!("[print-agent] cau-hinh: {:?}", ho_tro);
                nhat_ky::ghi(
                    "cau_hinh",
                    &format!(
                        "khong_ro={} su_co={} trang_thai_may_in={} nhat_ky_app={} hang_doi={}",
                        ho_tro.khong_ro, ho_tro.su_co, ho_tro.trang_thai_may_in, ho_tro.nhat_ky_app, ho_tro.hang_doi
                    ),
                );
                // Xả hộp thư đi + đọc máy in là I/O — KHÔNG làm trong callback
                // (bài học 14–17/09: chặn callback là mất ping).
                let _ = gm.send(LenhMayIn::CoCauHinh);
            }
        };

        // Ảnh chụp hàng đợi server (v5.1 §8.7) — chỉ ghi trạng thái (khoá ngắn),
        // nhật ký một dòng khi SỐ LƯỢNG đổi.
        let on_hang_doi = {
            let (tt, co) = (trang_thai.clone(), co.clone());
            move |payload: Payload, _socket: RawClient| {
                if da_nghi(&co) {
                    return;
                }
                let anh = bao_cao::doc_hang_doi(&payload_dau(payload));
                let doi = khoa(&tt).hang_doi.nhan_anh(anh, Instant::now());
                if let Some(tom_tat) = doi {
                    nhat_ky::ghi("hang_doi", &tom_tat);
                }
            }
        };

        let ket_noi = ClientBuilder::new(&cfg.server_url)
            .namespace(NAMESPACE)
            .auth(auth.clone())
            .transport_type(TRANSPORT)
            .reconnect(false)
            .on("job", on_job)
            .on("error", on_error)
            .on("open", on_open)
            .on("close", on_close)
            .on("cau-hinh", on_cau_hinh)
            .on("hang-doi", on_hang_doi)
            .connect();

        match ket_noi {
            Ok(client) => {
                let tt = trang_thai.clone();
                let ly_do = canh_client(
                    &dung,
                    &|| khoa(&tt).da_noi,
                    &|| co.chet.load(Ordering::SeqCst),
                    &mut || kiem_server_ban_cu(&duong_gui, &trang_thai),
                    &mut |d| std::thread::sleep(d),
                    &Instant::now,
                );
                if ly_do == LyDoThoat::ChetHan {
                    eprintln!(
                        "[print-agent] mất kết nối >{}s liên tục — coi client chết, nối lại từ đầu...",
                        NGUONG_CHET_HAN.as_secs()
                    );
                    nhat_ky::ghi("noi_lai_tu_dau", &format!("mat ket noi >{}s", NGUONG_CHET_HAN.as_secs()));
                    khoa(&trang_thai).thong_bao_cuoi =
                        Some(format!("mất kết nối >{}s, đang nối lại...", NGUONG_CHET_HAN.as_secs()));
                }
                // Client cũ phải nghỉ HẲN trước khi vòng ngoài dựng client mới.
                cho_nghi(client, &co, the_he.load(Ordering::SeqCst), &duong_gui);
                // Đã bị thay (Lưu) thì trạng thái dùng chung là của lần chạy
                // mới — không ghi đè (T7).
                if !dung.load(Ordering::SeqCst) {
                    khoa(&trang_thai).da_noi = false;
                }
                if co.da_mo.load(Ordering::SeqCst) {
                    lan_noi_lai = 0;
                    loi_noi_da_ghi = None;
                }
            }
            Err(e) => {
                let chu = e.to_string();
                eprintln!("[print-agent] nối thất bại: {}", chu);
                if !dung.load(Ordering::SeqCst) {
                    let mut t = khoa(&trang_thai);
                    t.da_noi = false;
                    t.thong_bao_cuoi = Some(format!("nối thất bại (chỉ websocket): {}", chu));
                }
                if loi_noi_da_ghi.as_deref() != Some(chu.as_str()) {
                    nhat_ky::ghi("noi_that_bai", &format!("{} — {}", chu, CHU_CHI_WEBSOCKET));
                    loi_noi_da_ghi = Some(chu);
                }
            }
        }
        if dung.load(Ordering::SeqCst) {
            break;
        }
        ngu_co_the_dung(cho_noi_lai(lan_noi_lai, so_ngau_nhien()), &dung);
        lan_noi_lai = lan_noi_lai.saturating_add(1);
    }

    // Dừng hẳn (R7a): luồng theo dõi máy in; luồng theo dõi tiếp + worker tự
    // thấy cờ dừng.
    let _ = gui_may_in.send(LenhMayIn::Dung);
    nhat_ky::ghi("net_dung", &format!("server={}", cfg.server_url));
    eprintln!("[print-agent] chay_net dừng");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bao_cao::HoTro;
    use crate::job::{KetQuaIn, LyDo};
    use base64::{engine::general_purpose::STANDARD, Engine as _};

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

    type DaEmit = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// Hàm gửi giả: kết nối có `ho_tro` — gửi được thì ghi lại + `co`, không thì `bo`.
    fn gui_gia(ho_tro: HoTro) -> (DaEmit, impl Fn(&'static str, serde_json::Value, CanHoTro) -> KetQuaGui) {
        let da = DaEmit::default();
        let d = da.clone();
        (da, move |ten: &'static str, v: serde_json::Value, can: CanHoTro| {
            if can.duoc_gui(ho_tro) {
                d.lock().unwrap().push((ten.to_string(), v));
                KetQuaGui::Co
            } else {
                KetQuaGui::Bo
            }
        })
    }

    /// Bước kiểm trước khi in "cho in, không chụp nền" (test không có spooler).
    fn kiem_in() -> KiemTruoc {
        KiemTruoc::In { nen: None }
    }

    fn du_ho_tro() -> HoTro {
        HoTro { khong_ro: true, su_co: true, trang_thai_may_in: true, nhat_ky_app: false, hang_doi: false }
    }

    /// In xong → ghi trạng thái cho UI + emit "ket-qua" đúng một lần.
    #[test]
    fn xu_ly_mot_viec_in_duoc_thi_emit_ket_qua_va_ghi_trang_thai() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (da_emit, gui) = gui_gia(HoTro::default());
        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>| KetQuaIn::DaIn;
        let (_, kq_gui) = xu_ly_mot_viec(&payload("j1"), &cfg(), &in_fn, &tt, &gui, &|| false);
        assert_eq!(kq_gui, KetQuaGui::Co);

        let e = da_emit.lock().unwrap();
        assert_eq!(e.len(), 1, "phải emit đúng MỘT lần");
        assert_eq!(e[0].0, "ket-qua");
        assert_eq!(e[0].1["trangThai"], "da_in");

        let t = tt.lock().unwrap();
        assert_eq!(t.jobs.len(), 1);
        assert_eq!(t.jobs[0].trang_thai, "da_in");
        assert_eq!(t.jobs[0].so_hoa_don, "j1");
        assert_eq!(t.jobs[0].job_id, "j1");
    }

    /// KhongRo + backend KHÔNG báo hỗ trợ `khong_ro` → TUYỆT ĐỐI KHÔNG emit
    /// (chống in đôi: backend cũ hiểu mọi trangThai khác "loi" là đã in), nhưng
    /// UI vẫn phải thấy dòng "khong_ro" để người biết mà kiểm.
    #[test]
    fn xu_ly_mot_viec_khong_ro_thi_KHONG_emit_nhung_van_ghi_ui() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (da_emit, gui) = gui_gia(HoTro::default());
        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>| {
            KetQuaIn::KhongRo("mat dau trong spooler".into())
        };
        let (_, kq_gui) = xu_ly_mot_viec(&payload("j7"), &cfg(), &in_fn, &tt, &gui, &|| false);
        assert_eq!(kq_gui, KetQuaGui::Bo, "nhật ký phải ghi gui_server=bo, không phải co");
        assert!(da_emit.lock().unwrap().is_empty(), "KhongRo mà emit là mở đường cho server retry → IN ĐÔI");

        let t = tt.lock().unwrap();
        assert_eq!(t.jobs.len(), 1, "UI vẫn phải thấy job này");
        assert_eq!(t.jobs[0].trang_thai, "khong_ro");
        assert_eq!(t.jobs[0].so_hoa_don, "j7", "phải nêu đúng job dù không emit");
    }

    fn payload_co_name(id: &str) -> serde_json::Value {
        let mut p = payload(id);
        p["job"]["name"] = serde_json::json!(format!("AI-INV_2026_030045-Anh_Loc-{}.pdf", id));
        p
    }

    #[test]
    fn khong_ro_gui_khi_backend_ho_tro_kem_loai() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (da_emit, gui) = gui_gia(du_ho_tro());
        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>| {
            KetQuaIn::KhongRo(LyDo::co_loai("loi sau khi da bat dau in: Kẹt giấy", MaSuCo::KetGiay))
        };
        xu_ly_mot_viec(&payload("1790251200000-8"), &cfg(), &in_fn, &tt, &gui, &|| false);
        let e = da_emit.lock().unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, "ket-qua");
        assert_eq!(e[0].1["trangThai"], "khong_ro");
        assert_eq!(e[0].1["loai"], "ket_giay");
        assert_eq!(e[0].1["jobId"], "1790251200000-8", "gửi server thì id ĐẦY ĐỦ (backend tra theo nó)");
    }

    #[test]
    fn giao_dien_hien_so_hoa_don_nhan_loi_va_bat_canh_bao() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (_da_emit, gui) = gui_gia(HoTro::default());
        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>| {
            KetQuaIn::Loi(LyDo::co_loai("loi truoc khi in: Hết giấy", MaSuCo::HetGiay))
        };
        xu_ly_mot_viec(&payload_co_name("tokHN-1727170000000-3"), &cfg(), &in_fn, &tt, &gui, &|| false);
        let t = tt.lock().unwrap();
        assert_eq!(t.jobs[0].so_hoa_don, "INV_2026_030045");
        assert_eq!(t.jobs[0].khach.as_deref(), Some("Anh_Loc"));
        assert_eq!(t.jobs[0].trang_thai, "loi");
        assert_eq!(t.jobs[0].loai, Some(MaSuCo::HetGiay));
        let dai = t.dai_moi_nhat().expect("dải cảnh báo phải bật");
        assert_eq!((dai.loai_dai, dai.ma, dai.so_hoa_don.as_str()),
            (crate::state::LoaiDai::Loi, Some(MaSuCo::HetGiay), "INV_2026_030045"));
    }

    #[test]
    fn khong_co_name_thi_hien_id_da_cat_token() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (_da_emit, gui) = gui_gia(HoTro::default());
        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>| KetQuaIn::DaIn;
        xu_ly_mot_viec(&payload("tokHNbimat-1727170000000-3"), &cfg(), &in_fn, &tt, &gui, &|| false);
        let t = tt.lock().unwrap();
        assert_eq!(t.jobs[0].so_hoa_don, "1727170000000-3");
        assert!(!t.jobs[0].so_hoa_don.contains("tokHNbimat"), "§0.3: token không lên giao diện");
    }

    /// Hàm in giả: trong lúc "in" báo sự cố hết giấy 3 lần (spooler poll 3 vòng)
    /// + kẹt giấy 1 lần + trạng thái máy in, rồi trả Loi.
    #[allow(clippy::too_many_arguments)]
    fn in_gia_co_su_co(
        _p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, bao: &dyn Fn(QuanSat),
    ) -> KetQuaIn {
        for _ in 0..3 {
            bao(QuanSat::MayIn { ma: MaSuCo::BinhThuong, chi_tiet: None });
            bao(QuanSat::SuCo { loai: MaSuCo::HetGiay, chi_tiet: "JOB_STATUS PAPEROUT (0x00000040)".into() });
        }
        bao(QuanSat::MayIn { ma: MaSuCo::KetGiay, chi_tiet: Some("x".into()) });
        bao(QuanSat::SuCo { loai: MaSuCo::KetGiay, chi_tiet: String::new() });
        KetQuaIn::Loi(LyDo::co_loai("loi truoc khi in: Hết giấy (da xoa job khoi hang doi Windows)", MaSuCo::HetGiay))
    }

    #[test]
    fn su_co_gui_ngay_moi_loai_mot_lan_truoc_ket_qua() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (da_emit, gui) = gui_gia(du_ho_tro());
        let da_chuyen = RefCell::new(Vec::<MaSuCo>::new());
        let chuyen = |ma: MaSuCo, _ct: Option<String>| da_chuyen.borrow_mut().push(ma);
        let xong = xu_ly_viec_co_bao_cao(&payload("1790251200000-9"), &cfg(), &in_gia_co_su_co, &kiem_in, &tt, &gui, &chuyen);

        let e = da_emit.lock().unwrap();
        let ten: Vec<&str> = e.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(ten, vec!["su-co", "su-co", "ket-qua"], "mỗi (job, loai) một lần, và TRƯỚC ket-qua");
        assert_eq!(e[0].1["loai"], "het_giay");
        assert_eq!(e[0].1["jobId"], "1790251200000-9");
        assert_eq!(e[0].1["mayIn"], "HP");
        assert_eq!(e[0].1["chiTiet"], "JOB_STATUS PAPEROUT (0x00000040)");
        assert!(e[0].1["luc"].as_str().unwrap().ends_with('Z'), "luc ISO-8601 UTC");
        assert_eq!(e[1].1["loai"], "ket_giay");
        assert!(e[1].1.get("chiTiet").is_none(), "chiTiet rỗng thì bỏ hẳn");
        assert_eq!(e[2].1["trangThai"], "loi");
        assert_eq!(e[2].1["loai"], "het_giay");
        assert_eq!(*da_chuyen.borrow(), vec![MaSuCo::BinhThuong, MaSuCo::KetGiay], "chỉ chuyển trạng thái máy in khi đổi");
        assert!(xong.da_thay_su_co, "R6: job có sự cố → ép gửi trạng thái lần rảnh tới");
        assert!(xong.theo_doi_tiep.is_none(), "Loi thì không theo dõi tiếp");
    }

    #[test]
    fn su_co_khong_gui_khi_backend_khong_ho_tro_nhung_van_bao_tren_may() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (da_emit, gui) = gui_gia(HoTro::default());
        let chuyen = |_ma: MaSuCo, _ct: Option<String>| {};
        xu_ly_viec_co_bao_cao(&payload("j9"), &cfg(), &in_gia_co_su_co, &kiem_in, &tt, &gui, &chuyen);
        let e = da_emit.lock().unwrap();
        let ten: Vec<&str> = e.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(ten, vec!["ket-qua"], "backend cũ: không su-co, chỉ ket-qua như trước");
        assert!(tt.lock().unwrap().dai_moi_nhat().is_some(), "máy shop vẫn phải báo");
    }

    /// R3: kết quả `khong_ro` mà spooler báo job còn trong hàng đợi → đưa vào
    /// theo dõi tiếp, kèm bằng chứng + số hoá đơn.
    #[allow(clippy::too_many_arguments)]
    fn in_gia_khong_ro_con_trong_hang_doi(
        _p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, bao: &dyn Fn(QuanSat),
    ) -> KetQuaIn {
        bao(QuanSat::SuCo { loai: MaSuCo::KetGiay, chi_tiet: String::new() });
        bao(QuanSat::ConTrongHangDoi(BangChungJob { da_thay_in: true, ..Default::default() }));
        KetQuaIn::KhongRo(LyDo::co_loai("loi sau khi da bat dau in: Kẹt giấy", MaSuCo::KetGiay))
    }

    #[test]
    fn khong_ro_con_trong_hang_doi_thi_dua_vao_theo_doi_tiep() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (_e, gui) = gui_gia(du_ho_tro());
        let id = "1790251200000-7";
        let xong = xu_ly_viec_co_bao_cao(&payload_co_name(id), &cfg(), &in_gia_khong_ro_con_trong_hang_doi, &kiem_in, &tt, &gui, &|_, _| {});
        let j = xong.theo_doi_tiep.expect("phải theo dõi tiếp");
        assert_eq!(j.job_id, id);
        assert_eq!(j.so_hoa_don, "INV_2026_030045");
        assert_eq!(j.loai, Some(MaSuCo::KetGiay));
        assert_eq!(j.bang_chung, BangChungJob { da_thay_in: true, ..Default::default() });
        // Không báo còn trong hàng đợi → không theo dõi tiếp.
        let in_khong_bao = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, _b: &dyn Fn(QuanSat)| {
            KetQuaIn::KhongRo(LyDo::co_loai("x", MaSuCo::HetGiay))
        };
        assert!(xu_ly_viec_co_bao_cao(&payload(id), &cfg(), &in_khong_bao, &kiem_in, &tt, &gui, &|_, _| {}).theo_doi_tiep.is_none());
    }

    /// R3: da_in muộn chỉ gửi khi kết nối HIỆN TẠI hỗ trợ khong_ro; giao diện
    /// luôn cập nhật "Đã in (sau khi khắc phục)" và tắt dải của job đó.
    #[test]
    fn theo_doi_tiep_da_in_gui_muon_theo_ho_tro_va_cap_nhat_giao_dien() {
        let id = "1790251200000-7";
        let lam = |ho_tro: HoTro| {
            let tt = Mutex::new(TrangThaiChung::default());
            {
                let mut t = tt.lock().unwrap();
                t.them_job(JobLog { job_id: id.into(), trang_thai: "khong_ro".into(), loai: Some(MaSuCo::KetGiay), ..Default::default() });
                t.ghi_ket_qua_job(id, "INV_1", "khong_ro", Some(MaSuCo::KetGiay));
            }
            let (e, gui) = gui_gia(ho_tro);
            let j = JobTheoDoiTiep::moi(id.into(), "INV_1".into(), Some(MaSuCo::KetGiay), BangChungJob::default(), Instant::now());
            xu_ly_ket_luan_tiep(j, KetLuanTiep::DaIn, "HP", &tt, &gui);
            let t = tt.lock().unwrap();
            assert!(t.jobs[0].sau_khac_phuc && t.jobs[0].trang_thai == "da_in");
            assert!(t.dai_moi_nhat().is_none());
            let v = e.lock().unwrap().clone();
            v
        };
        let da_gui = lam(du_ho_tro());
        assert_eq!(da_gui.len(), 1);
        assert_eq!(da_gui[0].1, serde_json::json!({"jobId": id, "trangThai": "da_in"}));
        assert!(lam(HoTro::default()).is_empty(), "backend cũ: chỉ ghi nhật ký, không gửi");
    }

    /// R3 đầu-cuối (ví dụ của giám sát): luồng theo dõi tiếp thật + xử lý kết
    /// luận thật trên spooler giả. Kẹt giấy 40 vòng → PRINTING → vắng 3 vòng →
    /// đúng MỘT `da_in` gửi đi; NV huỷ job (DELETING) → không gửi gì.
    #[test]
    fn r3_dau_cuoi_ket_giay_roi_in_thi_dung_mot_da_in_huy_thi_khong_gui() {
        use crate::spooler::gia::{job, vong, SpoolerGia, ID};
        use crate::spooler::Spooler;
        use crate::su_co::co::*;
        let chay = |vongs: Vec<spooler::VongDoc>| {
            let tt = Mutex::new(TrangThaiChung::default());
            let (e, gui) = gui_gia(du_ho_tro());
            let kho = KhoTheoDoiTiep::default();
            dua_vao_theo_doi_tiep(
                &kho,
                JobTheoDoiTiep::moi(ID.into(), "INV_1".into(), Some(MaSuCo::KetGiay), BangChungJob::default(), Instant::now())
                    .tren_may_in("HP"),
            );
            let sp = RefCell::new(SpoolerGia { vong: vongs, ..Default::default() });
            let dung = AtomicBool::new(false);
            theo_doi_tiep::chay_vong_lap(
                &kho,
                "HP",
                &dung,
                &mut |_| sp.borrow_mut().doc_vong(),
                // hết việc → dừng (luồng thật chạy tới khi bấm Lưu/tắt app)
                &mut |_| {
                    if kho.so_job() == 0 {
                        dung.store(true, Ordering::SeqCst);
                    }
                },
                &Instant::now,
                &mut |j, kl| xu_ly_ket_luan_tiep(j, kl, "HP", &tt, &gui),
            );
            assert!(sp.borrow().lenh.is_empty(), "theo dõi tiếp không bao giờ dừng/xoá job");
            let v = e.lock().unwrap().clone();
            v
        };
        let mut vongs = vec![vong(0, vec![job(7, JOB_STATUS_ERROR | JOB_STATUS_PAPEROUT)]); 40];
        vongs.push(vong(0, vec![job(7, JOB_STATUS_PRINTING)]));
        vongs.extend([vong(0, vec![]), vong(0, vec![]), vong(0, vec![])]);
        let da_gui = chay(vongs);
        assert_eq!(da_gui.len(), 1, "đúng MỘT da_in: {:?}", da_gui);
        assert_eq!(da_gui[0].1, serde_json::json!({"jobId": ID, "trangThai": "da_in"}));

        // NV huỷ job (DELETING) → KHÔNG da_in; R-C: báo su-co khong_xac_nhan.
        let da_gui = chay(vec![
            vong(0, vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT)]),
            vong(0, vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_DELETING)]),
            vong(0, vec![]),
        ]);
        assert_eq!(da_gui.len(), 1, "{:?}", da_gui);
        assert_eq!((da_gui[0].0.as_str(), &da_gui[0].1["loai"]), ("su-co", &serde_json::json!("khong_xac_nhan")));
    }

    /// R-C: theo dõi tiếp mất dấu / quá 12 giờ → `su-co khong_xac_nhan` (chỉ
    /// khi hoTro có su_co) + "In gần đây" và dải chuyển sang câu "chưa xác nhận".
    #[test]
    fn r_c_theo_doi_tiep_mat_dau_het_han_bao_su_co() {
        for (kl, chu) in [
            (KetLuanTiep::Mat("job roi hang doi ma chua tung thay in".into()), chu_mat_dau("INV_1")),
            (KetLuanTiep::HetHan, chu_het_han("INV_1")),
        ] {
            let tt = Mutex::new(TrangThaiChung::default());
            {
                let mut t = tt.lock().unwrap();
                t.them_job(JobLog { job_id: "j".into(), trang_thai: "khong_ro".into(), loai: Some(MaSuCo::HetGiay), ..Default::default() });
                t.ghi_ket_qua_job("j", "INV_1", "khong_ro", Some(MaSuCo::HetGiay));
                t.da_hieu();
            }
            let (e, gui) = gui_gia(du_ho_tro());
            let j = JobTheoDoiTiep::moi("j".into(), "INV_1".into(), Some(MaSuCo::HetGiay), BangChungJob::default(), Instant::now())
                .tren_may_in("HP cu");
            xu_ly_ket_luan_tiep(j, kl.clone(), "HP", &tt, &gui);
            let v = e.lock().unwrap().clone();
            assert_eq!(v.len(), 1, "{:?}", kl);
            assert_eq!(v[0].0, "su-co");
            assert_eq!(v[0].1["jobId"], "j");
            assert_eq!(v[0].1["loai"], "khong_xac_nhan");
            assert_eq!(v[0].1["chiTiet"], chu.as_str());
            assert_eq!(v[0].1["mayIn"], "HP cu", "máy in CỦA JOB");
            let t = tt.lock().unwrap();
            assert_eq!(t.jobs[0].loai, Some(MaSuCo::KhongXacNhan));
            assert_eq!(t.dai_moi_nhat().map(|d| d.ma), Some(Some(MaSuCo::KhongXacNhan)), "dải bật lại dù đã bấm Đã hiểu");
        }
        assert!(chu_mat_dau("INV_1").starts_with("Hoá đơn INV_1 không còn trong hàng đợi Windows mà app không thấy in (bị xoá/huỷ?)"));
        // Rời hàng đợi (đã thấy in) đúng lúc máy in báo hết giấy → KHÔNG bảo in lại ngay.
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(du_ho_tro());
        let mut kho = theo_doi_tiep::DanhSachTheoDoiTiep::default();
        kho.them(JobTheoDoiTiep::moi("j".into(), "INV_1".into(), None, BangChungJob { da_thay_in: true, ..Default::default() }, Instant::now()));
        let mut kl = Vec::new();
        for v in [
            crate::spooler::gia::vong(crate::su_co::co::PRINTER_STATUS_PAPER_OUT, vec![]),
            crate::spooler::gia::vong(crate::su_co::co::PRINTER_STATUS_PAPER_OUT, vec![]),
        ] {
            kl.extend(kho.mot_vong(&v, Instant::now()));
        }
        let (j, k) = kl.pop().expect("phải có kết luận");
        xu_ly_ket_luan_tiep(j, k, "HP", &tt, &gui);
        let v = e.lock().unwrap().clone();
        assert_eq!(v[0].1["loai"], "khong_xac_nhan");
        assert!(v[0].1["chiTiet"].as_str().unwrap().contains("có thể đang nằm trong bộ nhớ máy in"), "{}", v[0].1);
        let cb = crate::view_model::canh_bao(&tt.lock().unwrap(), "HP").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_1 có thể đang nằm trong máy in");
        assert!(chu_het_han("INV_1").contains("theo dõi quá 12 giờ không thấy in"));
        // backend không hỗ trợ su_co → không gửi, nhưng giao diện vẫn đổi
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(HoTro { khong_ro: true, ..HoTro::default() });
        let j = JobTheoDoiTiep::moi("j".into(), "INV_1".into(), None, BangChungJob::default(), Instant::now());
        xu_ly_ket_luan_tiep(j, KetLuanTiep::Mat("x".into()), "HP", &tt, &gui);
        assert!(e.lock().unwrap().is_empty());
        assert!(tt.lock().unwrap().dai_moi_nhat().is_some());
    }

    #[test]
    fn trang_thai_may_in_tra_payload_khi_doi_hoac_ep() {
        let tt = Mutex::new(TrangThaiChung::default());
        let v = ghi_trang_thai_may_in(&tt, "HP", MaSuCo::BinhThuong, None, false, true).expect("lần đầu = đổi");
        assert_eq!(v["trangThai"], "binh_thuong");
        assert_eq!(v["mayIn"], "HP");
        assert!(ghi_trang_thai_may_in(&tt, "HP", MaSuCo::BinhThuong, None, false, true).is_none(), "không đổi thì im");
        assert!(ghi_trang_thai_may_in(&tt, "HP", MaSuCo::BinhThuong, None, true, true).is_some(), "ép thì gửi dù không đổi");
        let v = ghi_trang_thai_may_in(&tt, "HP", MaSuCo::HetGiay, Some("PRINTER_STATUS PAPER_OUT (0x00000010)".into()), false, true).unwrap();
        assert_eq!(v["trangThai"], "het_giay");
        assert_eq!(v["chiTiet"], "PRINTER_STATUS PAPER_OUT (0x00000010)");
        assert_eq!(tt.lock().unwrap().may_in.as_ref().map(|(m, _)| *m), Some(MaSuCo::HetGiay));
    }

    /// R6: máy in không bật cờ cấp máy (luôn "bình thường") — backend chỉ biết
    /// hết sự cố khi app gửi lại trạng thái. Sau job có su-co, lần đọc lúc rảnh
    /// kế tiếp PHẢI gửi dù không đổi; lần sau nữa thì thôi.
    #[test]
    fn r6_sau_job_co_su_co_lan_ranh_toi_ep_gui_trang_thai() {
        let tt = Mutex::new(TrangThaiChung::default());
        ghi_trang_thai_may_in(&tt, "HP", MaSuCo::BinhThuong, None, false, true);
        let mut bo = BoTheoDoiMayIn::default();
        assert_eq!(bo.buoc(Ok(LenhMayIn::EpGuiLanRanhToi), true), BuocMayIn::KhongLamGi);
        // Worker đang in (chưa quá lâu) → chưa đọc, lời hứa ép gửi còn giữ.
        assert_eq!(bo.buoc(Err(RecvTimeoutError::Timeout), false), BuocMayIn::KhongLamGi);
        let BuocMayIn::DocRanh { ep_gui } = bo.buoc(Err(RecvTimeoutError::Timeout), true) else { panic!() };
        assert!(ep_gui);
        assert!(ghi_trang_thai_may_in(&tt, "HP", MaSuCo::BinhThuong, None, ep_gui, true).is_some(),
            "binh_thuong không đổi vẫn phải gửi");
        assert_eq!(bo.buoc(Err(RecvTimeoutError::Timeout), true), BuocMayIn::DocRanh { ep_gui: false });
        assert_eq!(bo.buoc(Ok(LenhMayIn::Dung), true), BuocMayIn::Thoat);
        assert_eq!(bo.buoc(Ok(LenhMayIn::CoCauHinh), false), BuocMayIn::SauCauHinh);
    }

    /// R8: worker in một job quá 20 s thì luồng theo dõi máy in vẫn đọc.
    #[test]
    fn r8_dang_in_qua_20_giay_van_doc_may_in() {
        let t0 = Instant::now();
        assert!(nen_doc_may_in(None, t0));
        assert!(!nen_doc_may_in(Some(t0), t0 + Duration::from_secs(19)));
        assert!(nen_doc_may_in(Some(t0), t0 + DANG_IN_QUA_LAU));
    }

    #[test]
    fn co_dang_in_tu_tat_ke_ca_khi_panic() {
        let co: DangInTu = Mutex::new(None);
        {
            let _g = CoDangIn::bat(&co);
            assert!(co.lock().unwrap().is_some());
        }
        assert!(co.lock().unwrap().is_none());
        let co = Arc::new(Mutex::new(None));
        let c = co.clone();
        let _ = std::thread::spawn(move || {
            let _g = CoDangIn::bat(&c);
            panic!("gia lap worker panic");
        })
        .join();
        assert!(khoa(&co).is_none(), "worker chết không được khoá luồng theo dõi máy in ở 'đang in'");
    }

    /// R7: cờ dừng (bấm Lưu) làm vòng canh client thoát ngay nhịp sau.
    #[test]
    fn r7_co_dung_lam_vong_canh_thoat() {
        let dung = AtomicBool::new(false);
        let so_nhip = Cell::new(0);
        let ly_do = canh_client(
            &dung,
            &|| true,
            &|| false,
            &mut || {
                so_nhip.set(so_nhip.get() + 1);
                if so_nhip.get() == 3 {
                    dung.store(true, Ordering::SeqCst);
                }
            },
            &mut |_| {},
            &Instant::now,
        );
        assert_eq!(ly_do, LyDoThoat::Dung);
        assert_eq!(so_nhip.get(), 3);
    }

    #[test]
    fn r7_mat_ket_noi_lien_tuc_60s_thi_chet_han_con_chap_chon_thi_khong() {
        let t0 = Instant::now();
        let dong_ho = Cell::new(t0);
        let dung = AtomicBool::new(false);
        let ly_do = canh_client(&dung, &|| false, &|| false, &mut || {}, &mut |d| dong_ho.set(dong_ho.get() + d), &|| dong_ho.get());
        assert_eq!(ly_do, LyDoThoat::ChetHan);
        assert!(dong_ho.get() - t0 >= NGUONG_CHET_HAN);
        // Chập chờn (cứ 20 s nối lại một nhịp) → chuỗi mất bị reset, không chết hẳn.
        let dong_ho = Cell::new(t0);
        let ly_do = canh_client(
            &dung,
            &|| (dong_ho.get() - t0).as_secs().is_multiple_of(20),
            &|| false,
            &mut || {
                if dong_ho.get() - t0 > Duration::from_secs(300) {
                    dung.store(true, Ordering::SeqCst);
                }
            },
            &mut |d| dong_ho.set(dong_ho.get() + d),
            &|| dong_ho.get(),
        );
        assert_eq!(ly_do, LyDoThoat::Dung);
    }

    #[test]
    fn r7_dung_va_cho_thay_luong_cu_da_thoat() {
        let dung = Arc::new(AtomicBool::new(false));
        let (bao, da_dung) = mpsc::channel::<()>();
        let d = dung.clone();
        let luong = std::thread::spawn(move || {
            let _bao = bao;
            while !d.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let dk = DieuKhienNet { dung, da_dung, theo_doi: Arc::default() };
        assert!(dk.dung_va_cho(Duration::from_secs(5)), "luồng cũ thấy cờ dừng và thoát");
        luong.join().unwrap();
    }

    /// R-A đầu-cuối (kịch bản S2 của giám sát vòng 2): J1 kẹt trên JOB
    /// (PAPEROUT|ERROR), cấp máy bình thường. J2 tới → KHÔNG gọi hàm in, gửi
    /// `ket-qua loi` mã của J1; dải + "In gần đây" hiện câu `loi` (R1).
    #[test]
    fn r_a_s2_hoa_don_moi_xep_sau_job_ket_bi_tu_choi_bang_loi() {
        use crate::spooler::gia::{job, vong, SpoolerGia};
        use crate::spooler::Spooler;
        use crate::su_co::co::*;
        let sp = RefCell::new(SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR)])],
            ..Default::default()
        });
        let kho = KhoTheoDoiTiep::default();
        let kiem = || spooler::kiem_truoc_khi_in(&sp.borrow_mut().doc_vong(), "HP", kho.job_dang_ket("HP", Instant::now()), true);
        let da_goi_in = Cell::new(false);
        let in_that = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, _b: &dyn Fn(QuanSat)| {
            da_goi_in.set(true);
            KetQuaIn::DaIn
        };
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(du_ho_tro());
        let id2 = "clx0abc12345678-1790000000000";
        let mut p = payload(id2);
        p["job"]["name"] = serde_json::json!(format!("AI-INV_2-Khach-{}.pdf", id2));
        let xong = xu_ly_viec_co_bao_cao(&p, &cfg(), &in_that, &kiem, &tt, &gui, &|_, _| {});
        assert!(!da_goi_in.get(), "KHÔNG được gửi hoá đơn xuống máy in (không Sumatra, không spool)");
        assert!(sp.borrow().lenh.is_empty());
        assert!(xong.theo_doi_tiep.is_none());
        let v = e.lock().unwrap().clone();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, "ket-qua");
        assert_eq!(v[0].1["trangThai"], "loi");
        assert_eq!(v[0].1["loai"], "het_giay");
        assert_eq!(v[0].1["loiCuoi"], "Máy in đang kẹt hoá đơn INV_2026_030045 — chưa gửi hoá đơn này xuống máy in");
        let t = tt.lock().unwrap();
        assert_eq!(t.jobs[0].trang_thai, "loi");
        let cb = crate::view_model::canh_bao(&t, "HP").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_2 chưa in");
        assert!(cb.chi_tiet.ends_with("Hệ thống sẽ TỰ gửi in lại khi máy in hết lỗi — KHÔNG in tay."));
    }

    /// Chạy MỘT hoá đơn qua `xu_ly_viec_co_bao_cao` với bước kiểm thật
    /// (`spooler::kiem_truoc_khi_in`) trên vòng đọc `v`; hàm in giả ghi lại
    /// cờ nền nó nhận. Trả (emit, đã gọi in?, nền hàm in nhận, trạng thái).
    fn mot_hoa_don_kiem(
        v: spooler::VongDoc,
        backend_moi: bool,
    ) -> (Vec<(String, serde_json::Value)>, bool, Option<TapMa>, TrangThaiChung) {
        let kho = KhoTheoDoiTiep::default();
        let kiem = || spooler::kiem_truoc_khi_in(&v, "HP", kho.job_dang_ket("HP", Instant::now()), backend_moi);
        let nen_nhan = Cell::new(None);
        let da_goi = Cell::new(false);
        let in_that = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, nen: Option<TapMa>, _b: &dyn Fn(QuanSat)| {
            da_goi.set(true);
            nen_nhan.set(nen);
            KetQuaIn::DaIn
        };
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(du_ho_tro());
        xu_ly_viec_co_bao_cao(&payload_co_name("clx0abc12345678-1790000000000"), &cfg(), &in_that, &kiem, &tt, &gui, &|_, _| {});
        let emit = e.lock().unwrap().clone();
        let t = tt.into_inner().unwrap();
        (emit, da_goi.get(), nen_nhan.get(), t)
    }

    /// T2: server BẢN CŨ (không `cau-hinh`) — KHÔNG từ chối in dù hàng đợi
    /// kẹt: backend cũ tiêu một lượt mỗi `loi`, vài phút là that_bai.
    #[test]
    fn t2_server_ban_cu_khong_tu_choi_in() {
        use crate::spooler::gia::{job_khac, vong};
        use crate::su_co::co::*;
        let ket = crate::spooler::JobHangDoi { status: JOB_STATUS_PAPEROUT, ..job_khac(9) };
        let (emit, da_goi, _, _) = mot_hoa_don_kiem(vong(0, vec![ket.clone()]), false);
        assert!(da_goi, "backend cũ: in như trước");
        assert_eq!(emit[0].1["trangThai"], "da_in");
        let (emit, da_goi, _, _) = mot_hoa_don_kiem(vong(0, vec![ket]), true);
        assert!(!da_goi);
        assert_eq!((emit[0].1["trangThai"].as_str(), emit[0].1["loai"].as_str()), (Some("loi"), Some("het_giay")));
        // NV tạm dừng job kẹt (PAUSED) → spooler in tiếp job sau → không từ chối
        let paused = crate::spooler::JobHangDoi { status: JOB_STATUS_PAPEROUT | JOB_STATUS_PAUSED, ..job_khac(9) };
        assert!(mot_hoa_don_kiem(vong(0, vec![paused]), true).1);
    }

    /// T3: cờ nền chụp ở bước kiểm TRƯỚC Sumatra được truyền xuống hàm in.
    #[test]
    fn t3_nen_chup_truoc_sumatra_di_xuong_ham_in() {
        use crate::spooler::gia::vong;
        use crate::su_co::co::*;
        let (_, da_goi, nen, _) = mot_hoa_don_kiem(vong(PRINTER_STATUS_ERROR, vec![]), true);
        assert!(da_goi);
        let nen = nen.expect("phải có nền chụp trước Sumatra");
        assert!(nen.co(MaSuCo::LoiMayIn), "HP 4003 ERROR suốt → ERROR là nền");
        let (_, _, nen, _) = mot_hoa_don_kiem(vong(0, vec![]), true);
        assert_eq!(nen, Some(TapMa::default()), "máy sạch lúc kiểm → nền rỗng: sự cố bắt đầu lúc Sumatra chạy là MỚI");
        // Không đọc được máy in → nền rỗng (thận trọng: mọi mã đều tính)
        let (_, _, nen, _) = mot_hoa_don_kiem(spooler::VongDoc { hang_doi: Some(vec![]), ..Default::default() }, true);
        assert_eq!(nen, Some(TapMa::default()));
    }

    /// T5: tên máy in không còn trong Windows → `loi(khong_tim_thay_may_in)`
    /// NGAY, không gọi Sumatra (chưa byte nào rời máy); dải nói chọn lại máy in.
    #[test]
    fn t5_may_in_khong_ton_tai_tu_choi_truoc_khi_in() {
        let v = spooler::VongDoc { khong_tim_thay_may_in: true, ..Default::default() };
        let (emit, da_goi, _, t) = mot_hoa_don_kiem(v.clone(), true);
        assert!(!da_goi, "không gọi Sumatra");
        assert_eq!(emit.len(), 1);
        assert_eq!(emit[0].1["trangThai"], "loi");
        assert_eq!(emit[0].1["loai"], "khong_tim_thay_may_in");
        assert_eq!(emit[0].1["loiCuoi"], spooler::chu_khong_tim_thay_may_in("HP").as_str());
        assert!(emit[0].1.get("conTrongHangDoi").is_none());
        let cb = crate::view_model::canh_bao(&t, "HP").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Không tìm thấy máy in trong Windows — hoá đơn INV_2026_030045 chưa in");
        // backend cũ: in như trước (T2 áp cho mọi lần từ chối)
        assert!(mot_hoa_don_kiem(v, false).1);
    }

    /// T9: copies > 1 in thiếu bản → `khong_ro` với `conTrongHangDoi:false`,
    /// không theo dõi tiếp, dải nói đúng số bản.
    #[test]
    fn t9_in_thieu_ban_khong_ro_ngoai_hang_doi() {
        let in_gia = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, bao: &dyn Fn(QuanSat)| {
            // (không thể xảy ra ở đường thật, nhưng dù có báo) còn trong hàng đợi
            bao(QuanSat::ConTrongHangDoi(BangChungJob::default()));
            KetQuaIn::KhongRo(LyDo { ban_da_in: Some((1, 2)), ..LyDo::co_loai(printing::chu_thieu_ban(1, 2), MaSuCo::HetGiay) })
        };
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(du_ho_tro());
        let xong = xu_ly_viec_co_bao_cao(&payload_co_name("1790251200000-7"), &cfg(), &in_gia, &kiem_in, &tt, &gui, &|_, _| {});
        let v = e.lock().unwrap()[0].1.clone();
        assert_eq!((v["trangThai"].as_str(), v["conTrongHangDoi"].as_bool()), (Some("khong_ro"), Some(false)));
        assert!(v["loiCuoi"].as_str().unwrap().starts_with("Đã in 1/2 bản — bản còn lại CHƯA in"));
        assert!(v.get("banDaIn").is_none() && v.get("ban_da_in").is_none(), "không thêm trường ngoài hợp đồng");
        assert!(xong.theo_doi_tiep.is_none());
        let t = tt.lock().unwrap();
        assert_eq!(t.jobs[0].ban_da_in, Some((1, 2)));
        let cb = crate::view_model::canh_bao(&t, "HP").unwrap();
        assert!(cb.tieu_de.contains("Đã in 1/2 bản — bản còn lại CHƯA in") && !cb.tieu_de.contains("có thể"), "{}", cb.tieu_de);
    }

    /// T1: chỉ websocket — polling là đường chết lặng không callback (quay 100% CPU).
    #[test]
    fn t1_chi_dung_websocket() {
        assert!(TRANSPORT == TransportType::Websocket);
        assert!(CHU_CHI_WEBSOCKET.contains("websocket"));
    }

    /// T7: client của lần chạy mạng đã bị thay (bấm Lưu) coi như đã nghỉ —
    /// "open" của nó không đăng ký gì, luồng poll của nó kết thúc.
    #[test]
    fn t7_client_cua_lan_chay_da_bi_thay_coi_nhu_nghi() {
        let dung = Arc::new(AtomicBool::new(false));
        let co = Arc::new(CoClient { dung_net: dung.clone(), ..CoClient::default() });
        assert!(!da_nghi(&co));
        dung.store(true, Ordering::SeqCst);
        let c = co.clone();
        let kq = std::thread::spawn(move || {
            // như callback "open" trên luồng poll của client
            if da_nghi(&c) {
                return "khong_dang_ky";
            }
            "dang_ky"
        })
        .join();
        assert!(kq.expect_err("luồng poll phải kết thúc").is::<ThoatLuongPoll>());
        LA_LUONG_NGAT.with(|c| c.set(true));
        assert!(da_nghi(&co), "trên luồng ngắt của ta: chỉ báo thôi làm gì");
        LA_LUONG_NGAT.with(|c| c.set(false));
    }

    /// R-D: `ket-qua khong_ro` mang `conTrongHangDoi`; `false` + mã máy → dải
    /// "có thể đang nằm trong máy in".
    #[test]
    fn r_d_khong_ro_mang_con_trong_hang_doi() {
        let chay = |bao_con_trong: bool| {
            let in_gia = move |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, bao: &dyn Fn(QuanSat)| {
                if bao_con_trong {
                    bao(QuanSat::ConTrongHangDoi(BangChungJob::default()));
                }
                KetQuaIn::KhongRo(LyDo::co_loai("x", MaSuCo::HetGiay))
            };
            let tt = Mutex::new(TrangThaiChung::default());
            let (e, gui) = gui_gia(du_ho_tro());
            let xong = xu_ly_viec_co_bao_cao(&payload_co_name("1790251200000-7"), &cfg(), &in_gia, &kiem_in, &tt, &gui, &|_, _| {});
            let v = e.lock().unwrap()[0].1.clone();
            let cb = crate::view_model::canh_bao(&tt.lock().unwrap(), "HP").unwrap();
            (v, cb, xong.theo_doi_tiep.is_some())
        };
        let (v, cb, theo_doi) = chay(true);
        assert_eq!(v["conTrongHangDoi"], true);
        assert!(theo_doi);
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_2026_030045 đang chờ trong máy in");
        let (v, cb, theo_doi) = chay(false);
        assert_eq!(v["conTrongHangDoi"], false);
        assert!(!theo_doi);
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_2026_030045 có thể đang nằm trong máy in");
        assert_eq!(cb.chi_tiet, "Nạp giấy vào khay. Khắc phục xong đợi vài phút — chỉ in lại nếu vẫn không thấy ra.");
        // da_in / loi không mang trường này
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(du_ho_tro());
        let in_fn = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>| KetQuaIn::DaIn;
        xu_ly_mot_viec(&payload("j1"), &cfg(), &in_fn, &tt, &gui, &|| true);
        assert!(e.lock().unwrap()[0].1.get("conTrongHangDoi").is_none());
    }

    /// U2: máy in USB giữ hoá đơn trong bộ nhớ (hết giấy, ca HCM 25/09) →
    /// `ket-qua khong_ro` mang `conTrongHangDoi:true` (backend: tự in, KHÔNG in
    /// lại; ngắt cầu dao), job vào theo dõi tiếp QUA USB, dải trên máy shop nói
    /// "đang chờ trong máy in".
    #[test]
    fn u2_trong_may_in_usb_thi_con_trong_hang_doi_va_theo_doi_usb() {
        let in_gia = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, bao: &dyn Fn(QuanSat)| {
            bao(QuanSat::SuCo { loai: MaSuCo::CanXuLy, chi_tiet: "USB 0x90 STATUS:BUSY".into() });
            bao(QuanSat::TrongMayInUsb {
                bang_chung: BangChungJob { da_thay_in: true, ..Default::default() },
                da_thay_loi: true,
                da_thay_in: false,
            });
            KetQuaIn::KhongRo(LyDo::co_loai("may in bao loi qua USB", MaSuCo::CanXuLy))
        };
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(du_ho_tro());
        let xong = xu_ly_viec_co_bao_cao(&payload_co_name("1790251200000-7"), &cfg(), &in_gia, &kiem_in, &tt, &gui, &|_, _| {});
        let e = e.lock().unwrap();
        let (ten, v) = e.iter().find(|(t, _)| t == "ket-qua").expect("phải gửi ket-qua");
        assert_eq!(ten, "ket-qua");
        assert_eq!(v["trangThai"], "khong_ro");
        assert_eq!(v["loai"], "can_xu_ly");
        assert_eq!(v["conTrongHangDoi"], true);
        let j = xong.theo_doi_tiep.expect("phải theo dõi tiếp");
        assert!(j.la_qua_usb(), "theo dõi QUA USB, không theo hàng đợi");
        let cb = crate::view_model::canh_bao(&tt.lock().unwrap(), "HP").unwrap();
        assert!(cb.tieu_de.contains("đang chờ trong máy in"), "{}", cb.tieu_de);
        assert!(cb.chi_tiet.contains("TỰ in ra") && cb.chi_tiet.contains("KHÔNG in lại"), "{}", cb.chi_tiet);
    }

    /// U3: hết hạn 12 giờ của hoá đơn trong bộ nhớ máy in USB — câu KHÔNG nói
    /// "hàng đợi Windows" (nó không nằm ở đó).
    #[test]
    fn u3_het_han_usb_noi_bo_nho_may_in() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (e, gui) = gui_gia(du_ho_tro());
        let j = JobTheoDoiTiep::moi("1790251200000-7".into(), "INV_1".into(), Some(MaSuCo::CanXuLy), BangChungJob::default(), Instant::now())
            .qua_usb(true, false);
        xu_ly_ket_luan_tiep(j, KetLuanTiep::HetHan, "HP", &tt, &gui);
        let v = e.lock().unwrap()[0].1.clone();
        let ct = v["chiTiet"].as_str().unwrap_or_default().to_string();
        assert!(ct.contains("bộ nhớ máy in") && !ct.contains("hàng đợi Windows"), "{}", ct);
    }

    /// Ack từ rust_socketio 0.6: data gói ack là CHUỖI MẢNG đối số → Text([Array[obj]]).
    #[test]
    fn gia_tri_ack_boc_lop_mang_cua_rust_socketio() {
        use serde_json::json;
        // Đúng như handle_ack dựng: Payload::from(String "[{...}]").
        let p = Payload::from(r#"[{"ok":true,"soDong":5}]"#.to_string());
        assert_eq!(gia_tri_ack(p), json!({"ok": true, "soDong": 5}));
        assert_eq!(gia_tri_ack(Payload::Text(vec![json!({"ok": false, "loi": "QUA_TAI"})])), json!({"ok": false, "loi": "QUA_TAI"}));
        assert_eq!(gia_tri_ack(Payload::Text(vec![])), serde_json::Value::Null);
        assert_eq!(gia_tri_ack(Payload::from(vec![1u8, 2])), serde_json::Value::Null);
    }

    /// ĐẦU-CUỐI (chạy tay): client rust_socketio THẬT gửi `nhat-ky-app` kèm ack tới một
    /// server socket.io 4.x thật trả `ack({ok:true,...})` như backend ZaloCRM.
    /// `ACK_URL=http://127.0.0.1:47811 cargo test -- --ignored ack_dau_cuoi`
    #[test]
    #[ignore]
    fn ack_dau_cuoi_voi_server_socketio_that() {
        let url = std::env::var("ACK_URL").expect("ACK_URL");
        let client = ClientBuilder::new(url)
            .namespace("/print-agent")
            .transport_type(TransportType::Websocket)
            .connect()
            .expect("connect");
        let (gui, nhan) = mpsc::channel::<serde_json::Value>();
        let lo = vec![nhat_ky::DongGui { luc: std::time::SystemTime::now(), su_kien: "thu".into(), noi_dung: "a".into() }];
        client
            .emit_with_ack("nhat-ky-app", payload_nhat_ky(&lo, 0), Duration::from_secs(5), move |p: Payload, _c: RawClient| {
                let _ = gui.send(gia_tri_ack(p));
            })
            .expect("emit");
        let v = nhan.recv_timeout(Duration::from_secs(5)).expect("ack");
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["soDong"], 1);
        let _ = client.disconnect();
    }

    /// ĐẦU-CUỐI hàng đợi v5.1 (chạy tay): NGUYÊN ngăn mạng thật của app
    /// (`khoi_chay` → callback `cau-hinh`/`hang-doi`, luồng hỏi lại, luồng gửi yêu
    /// cầu) nối vào server socket.io 4.x giả theo hợp đồng (scratch `mock-hd/server.js`).
    /// `HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_dau_cuoi --nocapture`
    #[test]
    #[ignore]
    fn hang_doi_dau_cuoi_voi_server_socketio_that() {
        use crate::hang_doi::{KetCuc, LoaiViec};
        let url = std::env::var("HD_URL").expect("HD_URL");
        let cfg = Arc::new(Config {
            server_url: url,
            token: "tok-e2e".into(),
            printer_name: "HP e2e".into(),
            tray: "Tự động".into(),
            paper_size: "A5".into(),
        });
        let tt = Arc::new(Mutex::new(TrangThaiChung::default()));
        let dg = Arc::new(DuongGui::default());
        let _dk = khoi_chay(cfg, tt.clone(), dg.clone(), None);
        let cho = |mo_ta: &str, dk: &dyn Fn(&TrangThaiChung) -> bool| {
            let het = Instant::now() + Duration::from_secs(15);
            while Instant::now() < het {
                if dk(&khoa(&tt)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("hết giờ chờ: {mo_ta}");
        };
        let khoi = |t: &TrangThaiChung| t.hang_doi.khoi(t.da_noi, None, Instant::now(), &|_| String::new());

        // 1. Nối + cau-hinh (hang_doi) + ảnh chụp đầu tiên: 3 đang chờ, 1 chưa xác nhận.
        cho("ảnh chụp đầu", &|t| khoi(t).dong.len() == 4 && khoi(t).dong[0].bat_nut);
        assert!(dg.the_he_ho_tro(CanHoTro::HangDoi).is_some(), "cau-hinh báo hang_doi");
        {
            let k = khoi(&khoa(&tt));
            assert_eq!(k.tieu_de, "HÀNG ĐỢI (3) · 1 chưa xác nhận");
            assert!(k.dai_tam_giu.starts_with("2 hoá đơn đang chờ"));
        }

        // 2. Huỷ a1 qua đúng đường giao diện: bấm → xác nhận → luồng gửi yêu cầu.
        let m = {
            let mut t = khoa(&tt);
            assert!(t.hang_doi.bam_huy("a1", true));
            t.hang_doi.bat_dau("a1", LoaiViec::Huy, true).expect("xác nhận")
        };
        khoi_chay_yeu_cau_hang_doi(dg.clone(), tt.clone(), LoaiViec::Huy, vec![m]);
        cho("Đã huỷ a1", &|t| khoi(t).dong.iter().any(|d| d.id == "a1" && d.che_do == hang_doi::CheDo::DaHuy));
        cho("server bỏ a1 khỏi ảnh chụp", &|t| khoi(t).tieu_de == "HÀNG ĐỢI (2) · 1 chưa xác nhận");
        assert_eq!(khoa(&tt).jobs[0].trang_thai, job::DA_HUY, "In gần đây ghi Đã huỷ");

        // 3. Huỷ lệnh đang gửi: giao diện không cho; hỏi thẳng server → KHÔNG huỷ được (DANG_IN).
        assert!(!khoa(&tt).hang_doi.clone().bam_huy("d1", true));
        let kc = hang_doi::gui_yeu_cau(&dg, LoaiViec::Huy, "d1", &mut |_| {}, &mut |d| std::thread::sleep(d), &Instant::now);
        assert!(matches!(&kc, KetCuc::KhongDuoc { loi, .. } if loi == "DANG_IN"), "{kc:?}");

        // 4. Bỏ theo dõi k1 (chưa xác nhận): Vì sao? → Bỏ khỏi hàng đợi → xác nhận.
        let m = {
            let mut t = khoa(&tt);
            assert!(t.hang_doi.bam_vi_sao("k1"));
            assert!(t.hang_doi.bam_bo("k1", true));
            t.hang_doi.bat_dau("k1", LoaiViec::BoTheoDoi, true).expect("xác nhận bỏ")
        };
        khoi_chay_yeu_cau_hang_doi(dg.clone(), tt.clone(), LoaiViec::BoTheoDoi, vec![m]);
        cho("Đã bỏ k1", &|t| khoi(t).dong.iter().any(|d| d.id == "k1" && d.che_do == hang_doi::CheDo::DaBo));
        cho("server bỏ k1", &|t| khoi(t).tieu_de == "HÀNG ĐỢI (2)");

        // 5. Huỷ cả loạt còn lại (a2) qua "Huỷ cả N".
        let ds = {
            let mut t = khoa(&tt);
            // a2 là lệnh tạm giữ duy nhất còn lại.
            assert!(t.hang_doi.bam_huy_ca(true));
            t.hang_doi.bat_dau_loat(true)
        };
        assert_eq!(ds.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a2"]);
        khoi_chay_yeu_cau_hang_doi(dg.clone(), tt.clone(), LoaiViec::Huy, ds);
        cho("tổng kết loạt", &|t| khoi(t).loat_chu == "Đã huỷ 1/1 lệnh in");

        // 6. Server có nhận `lay-hang-doi` (luồng hỏi lại + sau mỗi yêu cầu).
        let dem = dg.gui_ack("dem", serde_json::json!({}), CanHoTro::HangDoi, Duration::from_secs(5)).expect("dem");
        eprintln!("server đếm: {dem}");
        assert!(dem["lay"].as_u64().unwrap_or(0) >= 1, "{dem}");
        assert_eq!(dem["huy"], 3, "{dem}");
        assert_eq!(dem["bo"], 1, "{dem}");
    }

    /// ĐẦU-CUỐI (chạy tay, ~70 s): server nhận `yeu-cau-huy` mà KHÔNG ack → app hỏi
    /// lại đủ 3 lần rồi kết luận CHƯA RÕ (không bao giờ "Đã huỷ"), luồng không treo.
    /// `HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_khong_ack --nocapture`
    #[test]
    #[ignore]
    fn hang_doi_khong_ack_la_chua_ro() {
        use crate::hang_doi::{KetCuc, LoaiViec};
        let url = std::env::var("HD_URL").expect("HD_URL");
        let dg = DuongGui::default();
        // Cổng thật là RawClient của callback "open" — y như chay_net.
        let (gui_raw, nhan_raw) = mpsc::channel::<RawClient>();
        let gui_raw = Mutex::new(gui_raw);
        let client = ClientBuilder::new(url)
            .namespace("/print-agent")
            .auth(serde_json::json!({ "token": "tok-e2e" }))
            .transport_type(TransportType::Websocket)
            .on("open", move |_p: Payload, raw: RawClient| {
                let _ = khoa(&gui_raw).send(raw);
            })
            .connect()
            .expect("connect");
        let raw = nhan_raw.recv_timeout(Duration::from_secs(5)).expect("open");
        dg.mo_ket_noi(Arc::new(CongSocket(Mutex::new(raw))), Instant::now());
        dg.nhan_cau_hinh(HoTro { hang_doi: true, ..HoTro::default() });
        let bat_dau = Instant::now();
        let mut lan_cuoi = 0;
        let kc = hang_doi::gui_yeu_cau(&dg, LoaiViec::Huy, "im_lang", &mut |n| lan_cuoi = n, &mut |d| std::thread::sleep(d), &Instant::now);
        eprintln!("kết cục {:?} sau {:?}, {} lần", kc, bat_dau.elapsed(), lan_cuoi);
        assert!(matches!(kc, KetCuc::ChuaRo { .. }), "{kc:?}");
        assert_eq!(lan_cuoi, hang_doi::SO_LAN_GUI_TOI_DA);
        let _ = client.disconnect();
    }

    /// 0.2.4: payload `nhat-ky-app` đúng hợp đồng (luc ISO ms, suKien, noiDung, boQua, phienBan).
    #[test]
    fn nhat_ky_app_payload_dung_hop_dong() {
        let lo = vec![nhat_ky::DongGui {
            luc: std::time::UNIX_EPOCH + Duration::from_millis(1_790_331_966_328),
            su_kien: "vet_in".into(),
            noi_dung: "job=x t=1ms".into(),
        }];
        let v = payload_nhat_ky(&lo, 3);
        assert_eq!(v["dong"][0]["luc"], "2026-09-25T10:26:06.328Z");
        assert_eq!(v["dong"][0]["suKien"], "vet_in");
        assert_eq!(v["dong"][0]["noiDung"], "job=x t=1ms");
        assert_eq!(v["boQua"], 3);
        assert_eq!(v["phienBan"], env!("CARGO_PKG_VERSION"));
    }

    /// 0.2.5 (chủ 25/09: "lúc gửi xuống máy in không hiển thị"): dòng "In gần đây"
    /// hiện NGAY khi nhận lệnh ("Đang gửi…"), đổi "Đã gửi — đang chờ in ra…" khi
    /// job rời hàng đợi, rồi THAY bằng kết quả cuối (một dòng, không nhân đôi).
    #[test]
    fn in_gan_day_hien_tung_buoc_mot_dong() {
        let tt = Mutex::new(TrangThaiChung::default());
        let (_e, gui) = gui_gia(du_ho_tro());
        let da_thay: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let tt_ref = &tt;
        let in_gia = |_p: &[u8], _pr: &str, _pa: &str, _t: &str, _c: u32, _j: &str, _n: Option<&str>, _nen: Option<TapMa>, bao: &dyn Fn(QuanSat)| {
            da_thay.borrow_mut().push(tt_ref.lock().unwrap().jobs[0].trang_thai.clone());
            bao(QuanSat::DaRoiHangDoi);
            da_thay.borrow_mut().push(tt_ref.lock().unwrap().jobs[0].trang_thai.clone());
            KetQuaIn::DaIn
        };
        xu_ly_viec_co_bao_cao(&payload_co_name("1790251200000-7"), &cfg(), &in_gia, &kiem_in, &tt, &gui, &|_, _| {});
        assert_eq!(*da_thay.borrow(), vec![job::DANG_GUI.to_string(), job::CHO_MAY_IN.to_string()]);
        let t = tt.lock().unwrap();
        assert_eq!(t.jobs.len(), 1, "một dòng cho một job");
        assert_eq!(t.jobs[0].trang_thai, job::DA_IN);
    }

    // --- R-H / R-I: kết nối ---

    #[test]
    fn r_h_backoff_1_toi_30_giay_co_jitter() {
        assert_eq!(cho_noi_lai(0, 0), Duration::from_secs(1));
        assert_eq!(cho_noi_lai(1, 0), Duration::from_secs(2));
        assert_eq!(cho_noi_lai(4, 0), Duration::from_secs(16));
        assert_eq!(cho_noi_lai(5, 0), Duration::from_secs(30));
        assert_eq!(cho_noi_lai(u32::MAX, 0), Duration::from_secs(30), "trần 30 s, không tràn số");
        for lan in 0..10 {
            for r in [1u64, 12_345, u64::MAX] {
                let d = cho_noi_lai(lan, r);
                let goc = cho_noi_lai(lan, 0);
                assert!(d >= goc && d <= goc + goc / 4, "lan {} r {}: {:?}", lan, r, d);
            }
        }
        assert_ne!(so_ngau_nhien(), so_ngau_nhien(), "jitter phải thật sự ngẫu nhiên");
    }

    /// R-H: callback báo client chết → vòng canh thoát NGAY (không chờ 60 s).
    #[test]
    fn r_h_client_chet_thi_vong_canh_thoat_ngay() {
        let dung = AtomicBool::new(false);
        let so_nhip = Cell::new(0);
        let ly_do = canh_client(
            &dung,
            &|| false,
            &|| so_nhip.get() >= 2,
            &mut || so_nhip.set(so_nhip.get() + 1),
            &mut |_| {},
            &Instant::now,
        );
        assert_eq!(ly_do, LyDoThoat::ClientChet);
        assert_eq!(so_nhip.get(), 2);
    }

    /// R-H/R-I: `thoat_luong_poll` kết thúc luồng đang chạy callback — không
    /// `park()` mãi (rò luồng) — và làm nhiễm độc khoá callback đang giữ, để
    /// `disconnect()` ở luồng khác lấy khoá đó trả lỗi NGAY thay vì treo.
    #[test]
    fn r_h_thoat_luong_poll_ket_thuc_luong_va_khoa_bi_nhiem_doc() {
        let khoa_on = Arc::new(Mutex::new(()));
        let k = khoa_on.clone();
        let luong = std::thread::spawn(move || {
            let _giu = k.lock().unwrap(); // như `RawClient::callback` giữ khoá `on`
            let co = CoClient::default();
            client_chet(&co);
        });
        let kq = luong.join();
        let loi = kq.expect_err("luồng poll phải KẾT THÚC bằng unwind");
        assert!(loi.is::<ThoatLuongPoll>());
        assert!(khoa_on.lock().is_err(), "khoá bị nhiễm độc → callback sau trả Err, không treo");
        // Trên luồng ngắt của ta: chỉ bật cờ, không unwind.
        LA_LUONG_NGAT.with(|c| c.set(true));
        let co = CoClient::default();
        client_chet(&co);
        assert!(co.chet.load(Ordering::SeqCst));
        co.nghi.store(true, Ordering::SeqCst);
        assert!(da_nghi(&co), "client đã nghỉ: callback thôi làm gì");
        LA_LUONG_NGAT.with(|c| c.set(false));
        assert!(!da_nghi(&CoClient::default()));
    }

    #[test]
    fn r_i_nhan_ra_server_tu_choi_ket_noi() {
        let err = Payload::Text(vec![serde_json::json!("Received an ConnectError frame: {\"message\":\"unauthorized\"}")]);
        let chu = chu_payload(&err);
        assert_eq!(ly_do_tu_choi(&chu).as_deref(), Some("unauthorized"));
        assert_eq!(ly_do_tu_choi("Received an ConnectError frame: \"No error message provided\"").as_deref(), Some("No error message provided"));
        assert_eq!(ly_do_tu_choi("IncompleteResponseFromEngineIo(...)"), None, "lỗi mạng thường không phải bị từ chối");
    }

    #[test]
    fn payload_dau_lay_doi_so_dau() {
        let v = payload_dau(Payload::Text(vec![serde_json::json!({"hoTro": ["khong_ro"]}), serde_json::json!(1)]));
        assert_eq!(bao_cao::doc_cau_hinh(&v), HoTro { khong_ro: true, ..HoTro::default() });
        assert_eq!(payload_dau(Payload::Text(vec![])), serde_json::Value::Null);
    }
}
