// SPDX-License-Identifier: AGPL-3.0-or-later
//! Xác nhận job đã IN THẬT hay chưa bằng cách poll Windows print spooler
//! (EnumJobs/GetPrinter), thay vì suy "đã in" chỉ từ exit code SumatraPDF
//! (Sumatra trả 0 ngay khi ĐÃ GỬI XONG cho spooler, không đợi in xong —
//! ken exit 0 sai lệch với "đã in thật" đã bắt gặp trên máy in HP thật).
//!
//! NGUYÊN TẮC CAO NHẤT (chống in đôi): `Loi` = backend GỬI LẠI. Chỉ được trả
//! `Loi` khi CHẮC CHẮN không một byte nào của job đã rời máy tính tới máy in.
//! Mọi ca nghi ngờ → `KhongRo` (backend không gửi lại): job nằm yên trong hàng
//! đợi Windows và tự in khi hết lỗi; luồng "theo dõi tiếp" (theo_doi_tiep.rs)
//! canh nó và báo `da_in` muộn.
//!
//! Đường DUY NHẤT ra `Loi` (sửa sau giám sát 25/09 — xem `go_job_khoi_hang_doi`):
//! job SẠCH (chưa từng PRINTING, 0 trang đã in, cờ chỉ trong {SPOOLING, PAUSED,
//! BLOCKED_DEVQ}) mà máy in (cấp máy) báo sự cố chặn in hoặc job bị
//! BLOCKED_DEVQ → tạm dừng → đọc lại, kiểm LẠI đúng điều kiện → xoá → kiểm đã
//! hết. Cờ lỗi TRÊN JOB (ERROR/PAPEROUT/USER_INTERVENTION/OFFLINE) do port
//! monitor bật TRONG LÚC ĐANG GỬI byte — máy in mạng có thể đã đệm một phần —
//! nên job mang cờ đó KHÔNG BAO GIỜ bị xoá: `KhongRo`, để job tự in khi hết lỗi.
//!
//! Thiết kế tách lớp để test được trên Mac (không có Win32):
//!   1. `TrangThaiJob` — quan sát rời rạc rút gọn từ JOB_INFO_2W/PRINTER_INFO_2
//!      ở một lần poll.
//!   2. `BoSuy`/`suy_ket_qua` — THUẦN, nhận chuỗi quan sát → kết luận.
//!   3. `Spooler` — trait bọc đúng 4 thao tác Win32 cần dùng (đọc một vòng,
//!      đọc hàng đợi, điều khiển job, ngủ). `theo_doi_job_voi` (vòng poll),
//!      `go_job_khoi_hang_doi` (xoá job), theo dõi tiếp (theo_doi_tiep.rs) và
//!      `tiep_tuc_job_bi_dung_cua_app` viết trên trait này nên test được với
//!      spooler giả; bản thật `win::SpoolerWin` chỉ là lớp vỏ gọi Win32.
//!
//! Trong lúc poll, mọi điều quan sát được đều báo ra ngoài NGAY qua `bao`
//! (`QuanSat`) — sự cố để gửi `su-co` tức thì, trạng thái máy in để luồng theo
//! dõi máy in quyết có gửi `trang-thai-may-in` — không đợi hết 15 giây.

// Phần thuần (suy_ket_qua, go_job_khoi_hang_doi, …) chỉ được GỌI THẬT trên
// Windows; trên Mac nó chỉ chạy trong test — tắt cảnh báo dead_code ở đó thay
// vì rải `cfg` khắp nơi làm lớp thuần không test được.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::job::{self, KetQuaIn, LyDo};
use crate::nhat_ky;
use crate::su_co::{self, co, MaSuCo, TapMa};
use crate::usb_may_in::{DocUsb, TinhTrangUsb};
use std::time::Duration;

/// Chu kỳ poll spooler.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Thời gian tối đa chờ spooler xác nhận trước khi bỏ cuộc (→ KhongRo).
pub const POLL_TIMEOUT: Duration = Duration::from_secs(15);

/// Số lần quan sát VẮNG LIÊN TIẾP (sau khi đã thấy job đang in) để kết luận job
/// đã rời hàng đợi vì IN XONG — xem §"Rời hàng đợi sạch" ở `suy_ket_qua`.
///
/// VÌ SAO 2 chứ không phải 1: `EnumJobs` có thể trả rỗng thoáng qua đúng lúc
/// spooler đang cập nhật hàng đợi, một lần vắng đơn lẻ chưa đủ chắc. Hai lần
/// liên tiếp (2 × POLL_INTERVAL = 1s) đủ loại nhiễu đó mà vẫn nhanh hơn nhiều
/// so với chờ hết POLL_TIMEOUT. Đặt cao hơn nữa chỉ làm mỗi job chậm thêm mà
/// không tăng độ chắc — job đã in xong thì không bao giờ quay lại hàng đợi.
pub const SO_LAN_VANG_LA_XONG: usize = 2;

/// Sau khi job rời hàng đợi sạch: đọc trạng thái máy in thêm SO_LAN × POLL_INTERVAL
/// (~2 s) trước khi báo `da_in` (R5c). Máy in MẠNG nhận trọn job vào bộ nhớ
/// máy rồi mới hết giấy/kẹt giấy — job rời hàng đợi Windows sạch sẽ mà giấy
/// chưa ra. Máy in báo sự cố chặn in trong khoảng này → `KhongRo(<mã>)`.
pub const SO_LAN_DOC_MAY_IN_SAU_KHI_ROI: usize = 4;

/// Máy in USB đọc được trạng thái (U2): sau khi job rời hàng đợi, đọc tối đa
/// chừng này lần (60 × 500 ms = 30 s) để thấy máy IN XONG (BUSY → IDLE) hoặc
/// báo lỗi. Hoá đơn 1–2 trang trên HP Laser 107 in ~10 s kể cả lúc máy vừa
/// thức; quá 30 s mà vẫn BUSY → `khong_ro` + theo dõi tiếp qua USB. Tổng thời
/// gian một job vẫn phải dưới 90 s backend chờ (hợp đồng §3.1).
pub const SO_LAN_USB_TOI_DA: usize = 60;

/// Máy USB KHÔNG có trường STATUS (không thấy được BUSY/IDLE): chừng này lần
/// đọc liên tiếp không lỗi (6 s) là xong. Dài hơn 4 lần của máy mạng vì máy
/// USB báo lỗi lúc KÉO GIẤY, sau khi nhận xong dữ liệu vài giây. Máy CÓ STATUS
/// thì không dùng số này — phải thấy BUSY → IDLE.
pub const SO_LAN_USB_KHONG_THAY_IN: usize = 12;

/// Trần THỜI GIAN THẬT của bước sau khi rời hàng đợi trên máy USB — ngoài số
/// lần đọc: mỗi lần đọc có thể chậm (IOCTL/registry treo) làm 60 lần kéo quá 30 s.
pub const THOI_GIAN_USB_TOI_DA: Duration = Duration::from_secs(30);

/// Sau lệnh xoá: kiểm lại hàng đợi tối đa SO_LAN × CHU_KY (5 s) cho tới khi
/// job biến mất. Job không vướng cổng máy in thì xoá gần như tức thì; kẹt quá
/// 5 s nghĩa là spooler đang giữ nó (cổng treo) — thà KhongRo còn hơn chờ lâu
/// làm backend hết giờ chờ (90 s, hợp đồng §3.1).
pub const CHU_KY_KIEM_SAU_XOA: Duration = Duration::from_millis(250);
pub const SO_LAN_KIEM_SAU_XOA: usize = 20;

/// Sau lệnh tạm dừng: chờ chừng này rồi mới đọc lại hàng đợi (R-K). SetJob
/// PAUSE trả về ngay, nhưng spooler có thể đang ở giữa lượt khởi động job —
/// đọc liền có thể thấy trạng thái trước lúc spooler kịp đổi cờ.
pub const CHO_SAU_TAM_DUNG: Duration = Duration::from_millis(500);

/// Cờ job cho thấy hàng đợi đang KẸT ở job đó (R-A/R-J, giám sát vòng 2):
/// port monitor không đẩy được byte (hết giấy, lỗi, offline, cần người xử lý).
/// KHÔNG gồm BLOCKED_DEVQ — đó là lỗi riêng của một job (driver không in được
/// job ấy), job khác vẫn in bình thường.
pub const CO_JOB_KET: u32 =
    co::JOB_STATUS_PAPEROUT | co::JOB_STATUS_ERROR | co::JOB_STATUS_OFFLINE | co::JOB_STATUS_USER_INTERVENTION;

/// Cờ job DUY NHẤT được phép có mặt khi xoá job (R2): job chỉ đang ghi spool,
/// đang bị tạm dừng (chính ta dừng trước khi xoá) hoặc bị spooler giữ lại vì
/// driver không in được. Viết theo DANH SÁCH TRẮNG chứ không danh sách đen:
/// cờ lạ (RENDERING_LOCALLY, cờ Windows đời sau…) tự động thành "không an toàn".
pub const CO_CHO_PHEP_XOA: u32 = co::JOB_STATUS_SPOOLING | co::JOB_STATUS_PAUSED | co::JOB_STATUS_BLOCKED_DEVQ;

/// Câu `loiCuoi` khi đã thấy job bị huỷ/khởi động lại rồi mất khỏi hàng đợi (R5a).
pub const CHU_BI_HUY: &str = "job bị huỷ/khởi động lại trong hàng đợi — biến mất không tính là đã in";

/// Trạng thái job đọc từ JOB_INFO_2W.Status tại một lần poll, đã rút gọn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrangThaiJob {
    /// Job đã tìm thấy trong hàng đợi nhưng chưa bắt đầu in (spooling/paused/...).
    DangCho,
    /// Job đang thực sự in (PRINTING / COMPLETE / RETAINED / PagesPrinted > 0).
    DangIn,
    /// Job đã in xong (cờ JOB_STATUS_PRINTED) — bằng chứng mạnh nhất.
    DaInXong,
    /// Job mang cờ lỗi do port monitor bật TRONG LÚC GỬI (ERROR/PAPEROUT/
    /// USER_INTERVENTION/OFFLINE) — có thể đã có byte tới máy in → không xoá.
    LoiJob(MaSuCo),
    /// Job bị spooler giữ lại vì driver không in được (BLOCKED_DEVQ) và không
    /// mang cờ lỗi nào khác — ứng viên xoá.
    KetHangDoi(MaSuCo),
    /// Job bị huỷ (DELETING/DELETED) hoặc khởi động lại (RESTART): từ đây job
    /// biến mất KHÔNG được tính là "đã in" (R5a).
    BiHuy,
    /// Không tìm thấy job này trong hàng đợi tại lần poll này (đã xong & bị dọn,
    /// hoặc chưa kịp xuất hiện, hoặc không match được) — để `BoSuy` quyết theo
    /// lịch sử.
    KhongThay,
    /// Máy in (cấp máy) báo sự cố chặn in, và job hoặc không có trong hàng đợi,
    /// hoặc có nhưng còn SẠCH — ứng viên xoá (nếu chưa từng thấy in).
    MayInLoi(MaSuCo),
    /// Poll thất bại (OpenPrinter/EnumJobs lỗi) — observability failure, KHÔNG
    /// phải bằng chứng in lỗi, cũng KHÔNG phải "hàng đợi rỗng" (R5b).
    LoiTruyVan,
}

/// Bằng chứng đã tích luỹ về job trong lúc theo dõi — chuyển sang "theo dõi
/// tiếp" (R3) để không phải thấy lại PRINTING mới kết luận được.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BangChungJob {
    /// Đã thấy PRINTING / PagesPrinted > 0 / COMPLETE / RETAINED / RESTART.
    pub da_thay_in: bool,
    /// Đã thấy DELETING / DELETED / RESTART.
    pub da_thay_huy: bool,
    /// Mã sự cố CẤP MÁY có sẵn ở lần đọc ĐẦU của job ("cờ nền", R-B) — không
    /// tính chống lại job, kể cả khi theo dõi tiếp. Lần đọc đầu không đọc được
    /// máy in thì nền rỗng (thận trọng: mọi mã đều tính).
    pub nen: TapMa,
}

/// Điều quan sát được trong lúc theo dõi một job — báo ra ngoài NGAY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuanSat {
    /// Sự cố chặn in (mức `loi`) trên máy in hoặc trên job. Báo MỖI vòng poll
    /// còn thấy — lọc "mỗi (job, loai) một lần" là việc của nơi gửi (net.rs).
    SuCo { loai: MaSuCo, chi_tiet: String },
    /// Trạng thái máy in đọc được ở vòng này (kể cả `BinhThuong`, `HetMuc`).
    MayIn { ma: MaSuCo, chi_tiet: Option<String> },
    /// Vòng theo dõi kết luận `KhongRo` mà lần đọc cuối VẪN THẤY job trong
    /// hàng đợi Windows — net.rs đưa job vào luồng theo dõi tiếp (R3).
    ConTrongHangDoi(BangChungJob),
    /// Job đã rời hàng đợi Windows sang BỘ NHỚ máy in USB mà chưa ra giấy (U2):
    /// máy báo lỗi (`da_thay_loi`) hoặc vẫn bận quá hạn — net.rs đưa job vào theo
    /// dõi tiếp QUA USB, báo backend `conTrongHangDoi:true` (tự in, KHÔNG in lại).
    /// `da_thay_in`: máy USB đã BUSY trong bước sau khi rời hàng đợi (giao vì
    /// bận quá hạn) — theo dõi tiếp chỉ cần thấy về IDLE.
    TrongMayInUsb { bang_chung: BangChungJob, da_thay_loi: bool, da_thay_in: bool },
}

/// Kết luận của `BoSuy`.
#[derive(Debug, Clone, PartialEq)]
enum KetLuan {
    /// `qua_vang`: suy từ "rời hàng đợi sạch" (còn phải qua `kiem_may_in_sau_khi_roi`);
    /// `false` = thấy thẳng cờ PRINTED.
    DaIn { qua_vang: bool },
    KhongRo(LyDo),
    /// Sự cố chặn in trên một job SẠCH — vòng poll phải qua `go_job_khoi_hang_doi`.
    UngVienXoa(LyDo),
}

/// Bộ suy kết quả từ chuỗi quan sát theo thời gian — THUẦN, test được mọi OS.
///
/// Quy tắc:
/// - Gặp `DaInXong` bất kỳ lúc nào → DaIn.
/// - `LoiJob` (cờ lỗi TRÊN JOB) → KhongRo NGAY, không bao giờ xoá (R2): cờ đó
///   do port monitor bật trong lúc đang gửi byte.
/// - `MayInLoi`/`KetHangDoi` khi CHƯA từng thấy in/huỷ → ghi nhận ỨNG VIÊN xoá,
///   quyết ở cuối cửa sổ theo dõi (xem `het_gio`); đã từng thấy in/huỷ → KhongRo.
/// - ĐÃ thấy DangIn rồi job RỜI HÀNG ĐỢI SẠCH (KhongThay liên tiếp đủ
///   `SO_LAN_VANG_LA_XONG` lần) → DaIn. Đã thấy `BiHuy` thì cùng chuỗi vắng đó
///   ra KhongRo(`khong_xac_nhan`) kèm `CHU_BI_HUY` (R5a).
/// - `KhongThay`/`LoiTruyVan` khi CHƯA thấy in: chưa có bằng chứng gì — tiếp tục.
///
/// VÌ SAO ứng viên xoá chờ tới HẾT cửa sổ thay vì xoá ngay lần đầu thấy: cờ
/// cấp máy in có thể bật dai dẳng mà máy vẫn in được (một khay hết giấy, máy
/// khác đang in từ khay kia; SNMP báo nhầm). Xoá ngay thì job không bao giờ có
/// cơ hội in, backend giữ job "chờ máy hết lỗi" mãi → cả shop ngừng in. Chờ
/// hết cửa sổ: máy in được thật thì job chuyển PRINTING (không còn sạch) và đi
/// đường thường; máy kẹt thật thì 15 s sau vẫn sạch và mới xoá.
///
/// # Rời hàng đợi sạch = đã in xong
///
/// ĐO THẬT 18/09 trên máy build .207 (máy in ảo "Microsoft Print To PDF", poll
/// 50ms): job đi `Spooling` ×7 → `Printing` ×3 → **biến mất**, và file PDF RA
/// THẬT 310KB. Cờ `JOB_STATUS_PRINTED` **không xuất hiện lần nào**.
///
/// Windows xoá job khỏi hàng đợi NGAY khi in xong; `PRINTED` chỉ là trạng thái
/// thoáng qua giữa hai lần poll, thường không bao giờ bắt được. Bản trước chỉ
/// trả DaIn khi TRỰC TIẾP thấy `PRINTED` → mọi ca in thành công bình thường đều
/// rơi vào KhongRo. Đó chính là 3 job `khong_ro` của máy HCM ngày 14–15/09:
/// **giấy đã ra rồi mà hệ thống báo không rõ.**
///
/// Vì sao suy DaIn ở đây KHÔNG phá luật chống in đôi: DaIn không làm backend
/// gửi lại. Job GẶP SỰ CỐ thì **nằm lại** hàng đợi kèm cờ lỗi — nhánh lỗi bắt
/// trước; job bị NV huỷ thì có DELETING — `BiHuy` chặn; máy in mạng nhận job
/// rồi mới hết giấy thì `kiem_may_in_sau_khi_roi` bắt (R5c).
#[derive(Debug, Default)]
struct BoSuy {
    da_thay_in: bool,
    da_thay_huy: bool,
    vang_lien_tiep: usize,
    /// Mã sự cố của quan sát "ứng viên xoá" GẦN NHẤT; quan sát khác xoá nó.
    ung_vien_xoa: Option<MaSuCo>,
    /// Ứng viên đó do cờ CẤP MÁY (`MayInLoi`), không phải BLOCKED_DEVQ của
    /// riêng job (`KetHangDoi`) — T4: `loi_may_in` cấp máy báo `can_xu_ly`.
    ung_vien_cap_may: bool,
    /// Đã từng tìm thấy job trong hàng đợi / lần đọc được gần nhất còn thấy —
    /// chỉ để câu `loiCuoi` lúc hết giờ nói ĐÚNG chuyện gì đã xảy ra (R-J).
    /// Vòng poll ghi qua `thay_job`; `suy_ket_qua` (test) không ghi.
    da_thay_job: bool,
    con_thay_cuoi: bool,
    nen: TapMa,
    /// Job SẠCH đang chờ mà máy in chỉ báo sự cố NỀN (có từ trước Sumatra) —
    /// KHÔNG bao giờ là ứng viên xoá (kiểm cuối 25/09). Ca thật: máy WSD ngủ,
    /// ERROR nền, job chờ hơn 15 s → bản trước xoá → `can_xu_ly` → gửi thử 3
    /// phút sau lại bị xoá trước khi máy kịp thức → lặp vô hạn, cả máy bị giữ.
    /// Giờ job nằm lại, hết cửa sổ ra `khong_ro(<mã nền>)` và được theo dõi
    /// tiếp; máy thức in ra thì `da_in` trễ, backend đóng cầu dao.
    nen_chan: Option<MaSuCo>,
}

impl BoSuy {
    /// Lần đọc hàng đợi THÀNH CÔNG này có tìm thấy job của ta không.
    fn thay_job(&mut self, thay: bool) {
        self.da_thay_job |= thay;
        self.con_thay_cuoi = thay;
    }

    fn them(&mut self, ts: TrangThaiJob) -> Option<KetLuan> {
        use TrangThaiJob::*;
        match ts {
            DaInXong => return Some(KetLuan::DaIn { qua_vang: false }),
            DangIn => {
                self.da_thay_in = true;
                self.vang_lien_tiep = 0; // còn thấy job → chuỗi vắng bị ngắt
                self.ung_vien_xoa = None;
            }
            BiHuy => {
                self.da_thay_huy = true;
                self.vang_lien_tiep = 0;
                self.ung_vien_xoa = None;
            }
            LoiJob(ma) => {
                let chu = if self.da_thay_in {
                    format!("loi sau khi da bat dau in: {}", ma.nhan())
                } else {
                    format!(
                        "job bao loi {} (co the da gui mot phan du lieu toi may in) — khong xoa, job nam lai hang doi va tu in khi het loi",
                        ma.nhan()
                    )
                };
                return Some(KetLuan::KhongRo(LyDo::co_loai(chu, ma)));
            }
            KetHangDoi(ma) | MayInLoi(ma) => {
                if self.da_thay_in || self.da_thay_huy {
                    return Some(KetLuan::KhongRo(LyDo::co_loai(
                        format!("loi sau khi da bat dau in: {}", ma.nhan()),
                        ma,
                    )));
                }
                self.vang_lien_tiep = 0;
                self.ung_vien_xoa = Some(ma);
                self.ung_vien_cap_may = matches!(ts, MayInLoi(_));
            }
            KhongThay => {
                self.ung_vien_xoa = None;
                self.vang_lien_tiep += 1;
                if self.vang_lien_tiep >= SO_LAN_VANG_LA_XONG {
                    if self.da_thay_huy {
                        return Some(KetLuan::KhongRo(LyDo::co_loai(CHU_BI_HUY, MaSuCo::KhongXacNhan)));
                    }
                    if self.da_thay_in {
                        return Some(KetLuan::DaIn { qua_vang: true });
                    }
                }
            }
            DangCho => {
                self.vang_lien_tiep = 0;
                self.ung_vien_xoa = None;
            }
            // Không ĐỌC ĐƯỢC hàng đợi — khác hẳn với việc job đã rời đi: ngắt
            // chuỗi vắng, giữ nguyên ứng viên (không biết gì mới về job).
            LoiTruyVan => self.vang_lien_tiep = 0,
        }
        None
    }

    /// Hết cửa sổ theo dõi mà chưa có kết luận.
    fn het_gio(&self) -> KetLuan {
        if let Some(ma) = self.ung_vien_xoa {
            return KetLuan::UngVienXoa(LyDo::co_loai(format!("loi truoc khi in: {}", ma.nhan()), ma));
        }
        if let Some(ma) = self.nen_chan {
            if !self.da_thay_in && !self.da_thay_huy && self.da_thay_job && self.con_thay_cuoi {
                return KetLuan::KhongRo(LyDo::co_loai(
                    format!(
                        "job van nam trong hang doi Windows, chua bat dau in — may in bao {} tu truoc khi in (khong xoa: job tu in khi may het loi)",
                        ma.nhan()
                    ),
                    ma,
                ));
            }
        }
        // R-J: câu cũ "khong quan sat duoc job…" bị dùng cả khi job NẰM NGAY
        // TRONG hàng đợi suốt 15 s — người đọc nhật ký tưởng app không thấy job.
        let chu = if self.da_thay_huy {
            CHU_BI_HUY
        } else if self.da_thay_in {
            "da bat dau in nhung khong xac nhan duoc luc in xong"
        } else if self.da_thay_job && self.con_thay_cuoi {
            "job van nam trong hang doi Windows, chua bat dau in (het thoi gian theo doi)"
        } else if self.da_thay_job {
            "job roi hang doi Windows ma chua thay bat dau in (co the in rat nhanh, hoac bi xoa) — khong xac nhan duoc"
        } else {
            "khong thay job trong hang doi Windows (het thoi gian theo doi)"
        };
        KetLuan::KhongRo(LyDo::co_loai(chu, MaSuCo::KhongXacNhan))
    }

    fn bang_chung(&self) -> BangChungJob {
        BangChungJob { da_thay_in: self.da_thay_in, da_thay_huy: self.da_thay_huy, nen: self.nen }
    }
}

/// Suy KetQuaIn từ TRỌN chuỗi quan sát (quan sát đầu = sớm nhất) — dạng thuần
/// của `BoSuy` để test. Ứng viên xoá ra `Loi` ("ứng viên" — vòng poll thật
/// còn phải qua `go_job_khoi_hang_doi`); DaIn qua vắng chưa qua kiểm máy in (R5c).
#[cfg(test)]
pub fn suy_ket_qua(quan_sat: &[TrangThaiJob]) -> KetQuaIn {
    let mut bo = BoSuy::default();
    let mut kl = None;
    for ts in quan_sat {
        kl = bo.them(*ts);
        if kl.is_some() {
            break;
        }
    }
    match kl.unwrap_or_else(|| bo.het_gio()) {
        KetLuan::DaIn { .. } => KetQuaIn::DaIn,
        KetLuan::KhongRo(l) => KetQuaIn::KhongRo(l),
        KetLuan::UngVienXoa(l) => KetQuaIn::Loi(l),
    }
}

/// JOB_INFO_2W của job ta → các quan sát đưa vào `BoSuy`.
///
/// Trả NHIỀU quan sát khi job vừa đang in vừa mang cờ lỗi (vd PRINTING|PAPEROUT
/// — hết giấy giữa chừng): `[DangIn, LoiJob]`. Mã lỗi chung chung được đọc lại
/// theo câu trạng thái driver (`chu_driver`, R9).
fn quan_sat_theo(status: u32, trang_da_in: u32, chu_driver: &str) -> Vec<TrangThaiJob> {
    if status & co::JOB_STATUS_PRINTED != 0 {
        return vec![TrangThaiJob::DaInXong];
    }
    let mut v = Vec::with_capacity(2);
    const CO_DANG_IN: u32 = co::JOB_STATUS_PRINTING | co::JOB_STATUS_COMPLETE | co::JOB_STATUS_RETAINED;
    if status & CO_DANG_IN != 0 || trang_da_in > 0 {
        v.push(TrangThaiJob::DangIn);
    }
    if status & (co::JOB_STATUS_DELETING | co::JOB_STATUS_DELETED | co::JOB_STATUS_RESTART) != 0 {
        v.push(TrangThaiJob::BiHuy);
    }
    if let Some(ma) = su_co::ma_tu_co_job(status & !co::JOB_STATUS_BLOCKED_DEVQ) {
        v.push(TrangThaiJob::LoiJob(su_co::tinh_chinh_theo_chu(ma, chu_driver)));
    } else if status & co::JOB_STATUS_BLOCKED_DEVQ != 0 {
        v.push(TrangThaiJob::KetHangDoi(su_co::tinh_chinh_theo_chu(MaSuCo::LoiMayIn, chu_driver)));
    }
    if v.is_empty() {
        v.push(TrangThaiJob::DangCho);
    }
    v
}

/// JOB_INFO_2W.Status → quan sát (không có câu driver / số trang) — cho test.
#[cfg(test)]
pub fn quan_sat_job(status: u32) -> Vec<TrangThaiJob> {
    quan_sat_theo(status, 0, "")
}

/// Mã sự cố của job để báo `su-co` (gồm cả BLOCKED_DEVQ), đã đọc lại theo câu
/// trạng thái driver khi cờ chung chung (R9).
pub fn ma_su_co_job(j: &JobHangDoi) -> Option<MaSuCo> {
    su_co::ma_tu_co_job(j.status).map(|ma| su_co::tinh_chinh_theo_chu(ma, &j.mo_ta_driver))
}

/// KhongRo vì hết giờ (`khong_xac_nhan`) mà trong lúc theo dõi ĐÃ thấy sự cố
/// chặn in → gắn mã sự cố đó: "Không rõ: Hết giấy" cho NV biết phải làm gì,
/// "Không rõ: không xác nhận được" thì không.
pub fn bo_sung_loai(kq: KetQuaIn, su_co_da_thay: Option<MaSuCo>) -> KetQuaIn {
    match (kq, su_co_da_thay) {
        (KetQuaIn::KhongRo(mut ly_do), Some(ma)) if ly_do.loai == Some(MaSuCo::KhongXacNhan) => {
            ly_do.loai = Some(ma);
            KetQuaIn::KhongRo(ly_do)
        }
        (kq, _) => kq,
    }
}

/// Trạng thái máy in (PRINTER_INFO_2W.Status + Attributes) → (mã §1, chiTiet).
pub fn tinh_trang_tu_co(status: u32, thuoc_tinh: u32) -> (MaSuCo, Option<String>) {
    su_co::tinh_trang_may_in(status, thuoc_tinh)
}

// ===================== Hàng đợi Windows (thuần, qua trait) =====================

/// Một job trong hàng đợi Windows (JOB_INFO_2W) rút gọn field cần dùng.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobHangDoi {
    /// JOB_INFO_2W.JobId — khoá để SetJob.
    pub id: u32,
    /// DocumentName — SumatraPDF đặt bằng tên file PDF tạm, CHỨA job_id.
    pub document: String,
    pub status: u32,
    /// PagesPrinted — > 0 là đã có tờ ra, tuyệt đối không được coi là "chưa in".
    pub trang_da_in: u32,
    /// pStatus — câu trạng thái driver tự viết (vd "Paper out"), để ghi chiTiet
    /// và suy mã khi cờ chung chung (R9).
    pub mo_ta_driver: String,
    /// Giây kể từ UNIX epoch, quy đổi từ SYSTEMTIME UTC (Submitted) — CÙNG
    /// đơn vị với mốc gửi lệnh in để so sánh đúng nghĩa "job nộp sau lúc ta
    /// gửi lệnh in", không phải so bừa hai đại lượng khác đơn vị.
    pub submitted_epoch_secs: i64,
    /// pMachineName — máy tính đã nộp job (thường `\\TENMAY`). Hàng đợi CHIA
    /// SẺ (`\\PC\may`) có job của máy khác; lúc khởi động app chỉ đụng/nhận
    /// lại job của CHÍNH máy này (T8, `cung_may`). Rỗng = không biết.
    pub may_tinh: String,
}

/// Một vòng đọc spooler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VongDoc {
    /// PRINTER_INFO_2W.Status; `None` = không đọc được.
    pub co_may_in: Option<u32>,
    /// PRINTER_INFO_2W.Attributes, đọc CÙNG lần GetPrinterW với `co_may_in`
    /// (R10: WORK_OFFLINE). 0 khi không đọc được.
    pub thuoc_tinh_may_in: u32,
    /// Hàng đợi; `None` = không đọc được (mở máy in hoặc EnumJobs lỗi) — KHÔNG
    /// BAO GIỜ là "rỗng" khi đọc lỗi (R5b).
    pub hang_doi: Option<Vec<JobHangDoi>>,
    /// OpenPrinter lỗi vì TÊN máy in không tồn tại trong Windows.
    pub khong_tim_thay_may_in: bool,
    /// Máy in CỤC BỘ trên đúng một cổng `USBnnn` (usb_may_in.rs) — biết được
    /// kể cả lúc không hỏi thiết bị (hàng đợi đang gửi).
    pub la_may_usb: bool,
    /// Trạng thái hỏi THẲNG thiết bị USB — chỉ có khi `la_may_usb` VÀ mọi job
    /// trong hàng đợi đã gửi xong (`hang_doi_cho_doc_usb`) VÀ worker không tạm
    /// ngừng. `None` = không phải máy USB / không hỏi / không đọc được: mọi luật
    /// giữ nguyên như trước khi có lớp này.
    pub usb: Option<crate::usb_may_in::DocUsb>,
    /// ĐÃ hỏi thiết bị USB mà không tìm/mở/hỏi được (máy tắt, rút dây).
    pub usb_khong_doc_duoc: bool,
}

/// Được hỏi thiết bị USB khi mọi job trong hàng đợi đã GỬI XONG xuống máy in
/// (PRINTED / COMPLETE / RETAINED — "Keep printed documents" giữ job lại mãi)
/// hoặc hàng đợi rỗng: không còn byte nào đang đi xuống cổng.
pub fn hang_doi_cho_doc_usb(jobs: &[JobHangDoi]) -> bool {
    jobs.iter().all(|j| j.status & (co::JOB_STATUS_PRINTED | co::JOB_STATUS_COMPLETE | co::JOB_STATUS_RETAINED) != 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LenhJob {
    TamDung,
    TiepTuc,
    Xoa,
}

/// Bốn thao tác spooler mà vòng theo dõi + xoá job cần. Bản thật: `win::SpoolerWin`.
pub trait Spooler {
    /// Đọc một vòng poll: cờ máy in + hàng đợi. Đọc CHẶT: EnumJobs lỗi →
    /// `hang_doi: None`, không bao giờ thành hàng đợi rỗng — "rời hàng đợi
    /// sạch" (= đã in) dựa vào chữ "rỗng" này (R5b).
    fn doc_vong(&mut self) -> VongDoc;
    /// Chỉ đọc hàng đợi (CHẶT như trên). Dùng khi quyết xoá job.
    fn doc_hang_doi(&mut self) -> Option<Vec<JobHangDoi>>;
    /// SetJob(JOB_CONTROL_PAUSE / RESUME / DELETE).
    fn dieu_khien(&mut self, id: u32, lenh: LenhJob) -> Result<(), String>;
    fn cho(&mut self, d: Duration);
}

/// Tìm job của ta trong danh sách: DocumentName chứa job_id + submit sau
/// mốc gửi lệnh in. Nhiều candidate khớp → ambiguous, coi như không thấy
/// (KHÔNG chọn bừa — đúng yêu cầu chống nhầm job).
pub fn tim_job<'a>(jobs: &'a [JobHangDoi], job_id: &str, submit_after_epoch_secs: i64) -> Option<&'a JobHangDoi> {
    // Trừ hao 2s: SYSTEMTIME.wSecond làm tròn giây, submit_after lấy ngay
    // trước khi gọi Sumatra nên job thật có thể "trước" vài trăm ms theo
    // đồng hồ hệ thống — biên an toàn nhỏ, KHÔNG rộng tới mức bắt nhầm job cũ.
    const BIEN_AN_TOAN_GIAY: i64 = 2;
    let ung_vien: Vec<&JobHangDoi> = jobs
        .iter()
        .filter(|j| {
            la_cua_job(j, job_id) && j.submitted_epoch_secs >= submit_after_epoch_secs - BIEN_AN_TOAN_GIAY
        })
        .collect();
    match ung_vien.len() {
        0 => None,
        1 => Some(ung_vien[0]),
        _ => {
            // Nhiều job trùng job_id trong document name (không nên xảy ra
            // vì job_id sinh duy nhất, nhưng phòng hờ) — chọn submit gần
            // nhất CHỈ KHI có đúng 1 giá trị lớn nhất rõ ràng; nếu bằng
            // nhau, vẫn ambiguous.
            let max_secs = ung_vien.iter().map(|j| j.submitted_epoch_secs).max().unwrap_or(i64::MIN);
            let cung_moi_nhat: Vec<&&JobHangDoi> =
                ung_vien.iter().filter(|j| j.submitted_epoch_secs == max_secs).collect();
            if cung_moi_nhat.len() == 1 {
                Some(*cung_moi_nhat[0])
            } else {
                None // thật sự ambiguous — coi như không thấy
            }
        }
    }
}

/// Job trong hàng đợi thuộc lần in này. KHÔNG lọc theo giờ nộp như
/// `tim_job`: ở đây "thấy thừa" chỉ làm ta thận trọng hơn (không xoá/KhongRo),
/// còn "thấy thiếu" là báo Loi cho job còn nằm đó. Chỉ coi `job_id` là một
/// CHUỖI CON của tên file — không giả định định dạng (backend mới gửi
/// `<ms>-<n>`, bản cũ `<token>-<ms>-<n>`).
///
/// Ranh giới phải: ký tự ngay sau `job_id` KHÔNG được là chữ số — id
/// `…-1790251200000-7` không được khớp nhầm file của job `…-1790251200000-71`.
pub fn la_cua_job(j: &JobHangDoi, job_id: &str) -> bool {
    !job_id.is_empty()
        && j.document.match_indices(job_id).any(|(i, _)| {
            !j.document[i + job_id.len()..].starts_with(|c: char| c.is_ascii_digit())
        })
}

/// Đã có dấu hiệu bắt đầu ra giấy / đã gửi byte tới máy in (kể cả RESTART:
/// job đã in dở rồi mới bị khởi động lại).
pub fn da_bat_dau_in(j: &JobHangDoi) -> bool {
    const CO: u32 = co::JOB_STATUS_PRINTING
        | co::JOB_STATUS_PRINTED
        | co::JOB_STATUS_COMPLETE
        | co::JOB_STATUS_RETAINED
        | co::JOB_STATUS_RESTART;
    j.status & CO != 0 || j.trang_da_in > 0
}

/// Job xoá được mà CHẮC CHẮN chưa byte nào rời máy (R2): 0 trang đã in và mọi
/// cờ đều nằm trong `CO_CHO_PHEP_XOA`.
fn xoa_an_toan(j: &JobHangDoi) -> bool {
    j.trang_da_in == 0 && j.status & !CO_CHO_PHEP_XOA == 0
}

fn ly_do_khong_an_toan(j: &JobHangDoi) -> String {
    format!(
        "job {} status 0x{:08X}, {} trang da in — co the da co byte toi may in, khong xoa",
        j.id, j.status, j.trang_da_in
    )
}

/// Kết quả thử gỡ job khỏi hàng đợi Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KetQuaGoJob {
    /// Đọc được hàng đợi (chặt) và không có job nào của lần in này.
    KhongCoTrongHangDoi,
    /// Đã tạm dừng → đọc lại vẫn sạch → xoá → kiểm lại thấy hết.
    DaGoXong,
    /// Job không còn "sạch" (đã/đang in, mang cờ lỗi do port monitor, bị huỷ…)
    /// — KHÔNG đụng vào: xoá lúc này có thể đã có byte tới máy in, backend gửi
    /// lại là in đôi. Job nằm lại hàng đợi và tự in khi hết lỗi.
    KhongAnToanDeXoa(String),
    /// Không đọc được hàng đợi / không dừng hoặc không xoá được / xoá rồi mà
    /// vẫn còn / hàng đợi đổi bất thường giữa chừng.
    KhongGoDuoc(String),
}

impl KetQuaGoJob {
    /// Job (nhiều khả năng) còn nằm trong hàng đợi — cần theo dõi tiếp (R3).
    pub fn job_con_trong_hang_doi(&self) -> bool {
        matches!(self, KetQuaGoJob::KhongAnToanDeXoa(_) | KetQuaGoJob::KhongGoDuoc(_))
    }
}

/// Trả các job đã tạm dừng về chạy tiếp. Gọi ở MỌI lối ra không xoá: bỏ job ở
/// trạng thái dừng mà báo KhongRo (server không gửi lại) là hoá đơn không bao
/// giờ ra giấy; cho nó chạy tiếp thì NV xử lý máy xong là in.
///
/// Lỗi thì GHI FILE NHẬT KÝ và trả về cho người gọi (R11a) — bản trước nuốt
/// lỗi bằng `let _`, job kẹt ở "Paused" mà không ai biết.
fn tiep_tuc_lai(sp: &mut dyn Spooler, ids: &[u32]) -> Vec<String> {
    let mut loi = Vec::new();
    for &id in ids {
        if let Err(e) = sp.dieu_khien(id, LenhJob::TiepTuc) {
            nhat_ky::ghi("tiep_tuc_loi", &format!("job_windows={} {}", id, e));
            loi.push(format!("job {}: {}", id, e));
        }
    }
    loi
}

/// Phần đuôi câu lỗi khi không cho job chạy tiếp được.
fn ghep_loi_tiep_tuc(loi: &[String]) -> String {
    if loi.is_empty() {
        String::new()
    } else {
        format!("; KHONG cho chay tiep duoc ({}) — job con bi tam dung trong hang doi", loi.join(", "))
    }
}

/// Gỡ job của lần in này khỏi hàng đợi Windows — đường duy nhất ra `Loi` (R2).
///
/// Chỉ xoá khi MỌI job của lần in này đều `xoa_an_toan` VÀ trong lúc theo dõi
/// chưa từng thấy in/huỷ (`da_thay_in`). TẠM DỪNG TRƯỚC, XOÁ SAU: giữa lúc đọc
/// "sạch" và lúc xoá, NV có thể vừa nạp giấy và spooler bắt đầu gửi. Tạm dừng
/// khoá spooler không khởi động job đó nữa; đọc lại sau khi dừng và kiểm LẠI
/// đúng điều kiện mới là bằng chứng đáng tin để xoá.
pub fn go_job_khoi_hang_doi(sp: &mut dyn Spooler, job_id: &str, da_thay_in: bool) -> KetQuaGoJob {
    use KetQuaGoJob::*;

    if da_thay_in {
        return KhongAnToanDeXoa("trong luc theo doi da thay job bat dau in / bi huy".into());
    }
    let Some(jobs) = sp.doc_hang_doi() else {
        return KhongGoDuoc("khong doc duoc hang doi Windows".into());
    };
    let cua_ta: Vec<&JobHangDoi> = jobs.iter().filter(|j| la_cua_job(j, job_id)).collect();
    if cua_ta.is_empty() {
        return KhongCoTrongHangDoi;
    }
    if let Some(j) = cua_ta.iter().find(|j| !xoa_an_toan(j)) {
        return KhongAnToanDeXoa(ly_do_khong_an_toan(j));
    }
    let ids: Vec<u32> = cua_ta.iter().map(|j| j.id).collect();

    // 1. Tạm dừng.
    let mut da_dung: Vec<u32> = Vec::with_capacity(ids.len());
    for &id in &ids {
        if let Err(e) = sp.dieu_khien(id, LenhJob::TamDung) {
            let loi = tiep_tuc_lai(sp, &da_dung);
            return KhongGoDuoc(format!("khong tam dung duoc job {}: {}{}", id, e, ghep_loi_tiep_tuc(&loi)));
        }
        da_dung.push(id);
    }

    // 2. Đọc lại sau khi dừng: đúng những job đó, VẪN sạch (PAUSED là cờ của ta).
    // Chờ CHO_SAU_TAM_DUNG trước (R-K): đọc liền sau SetJob có thể còn thấy
    // trạng thái cũ trong khi spooler đang khởi động job.
    sp.cho(CHO_SAU_TAM_DUNG);
    let Some(jobs) = sp.doc_hang_doi() else {
        let loi = tiep_tuc_lai(sp, &da_dung);
        return KhongGoDuoc(format!("khong doc lai duoc hang doi sau khi tam dung{}", ghep_loi_tiep_tuc(&loi)));
    };
    let hien_tai: Vec<&JobHangDoi> = jobs.iter().filter(|j| la_cua_job(j, job_id)).collect();
    if let Some(j) = hien_tai.iter().find(|j| !xoa_an_toan(j)) {
        let ly_do = ly_do_khong_an_toan(j);
        let loi = tiep_tuc_lai(sp, &da_dung);
        return KhongAnToanDeXoa(format!("{} (sau khi tam dung){}", ly_do, ghep_loi_tiep_tuc(&loi)));
    }
    let khop = hien_tai.len() == ids.len() && hien_tai.iter().all(|j| ids.contains(&j.id));
    if !khop {
        // Job biến mất sau khi dừng (có thể vừa in xong) hoặc mọc thêm job —
        // không còn chắc gì nữa.
        let loi = tiep_tuc_lai(sp, &da_dung);
        return KhongGoDuoc(format!("hang doi thay doi trong luc xoa{}", ghep_loi_tiep_tuc(&loi)));
    }

    // 3. Xoá.
    for &id in &ids {
        if let Err(e) = sp.dieu_khien(id, LenhJob::Xoa) {
            let loi = tiep_tuc_lai(sp, &da_dung);
            return KhongGoDuoc(format!("khong xoa duoc job {}: {}{}", id, e, ghep_loi_tiep_tuc(&loi)));
        }
    }

    // 4. Kiểm lại tới khi hết (một lần ngay, rồi mỗi CHU_KY). Trong lúc chờ
    // xoá mà thấy job của ta ĐÃ BẮT ĐẦU IN (R-K) — spooler kịp gửi byte giữa
    // hai bước — thì không còn chắc "chưa byte nào rời máy": không được `Loi`.
    for lan in 0..SO_LAN_KIEM_SAU_XOA {
        if lan > 0 {
            sp.cho(CHU_KY_KIEM_SAU_XOA);
        }
        if let Some(jobs) = sp.doc_hang_doi() {
            let con: Vec<&JobHangDoi> = jobs.iter().filter(|j| ids.contains(&j.id) || la_cua_job(j, job_id)).collect();
            if let Some(j) = con.iter().find(|j| da_bat_dau_in(j)) {
                return KhongGoDuoc(format!(
                    "job {} bat dau in trong luc xoa (status 0x{:08X}, {} trang) — co the da co byte toi may in",
                    j.id, j.status, j.trang_da_in
                ));
            }
            if con.is_empty() {
                return DaGoXong;
            }
        }
    }
    KhongGoDuoc(format!(
        "da lenh xoa nhung job van con trong hang doi sau {} ms",
        SO_LAN_KIEM_SAU_XOA as u128 * CHU_KY_KIEM_SAU_XOA.as_millis()
    ))
}

/// Quyết kết quả cuối cho một ứng viên Loi (sự cố chặn in trên job SẠCH).
///
/// `go` tiêm được: thật = `go_job_khoi_hang_doi` trên spooler Windows, test =
/// closure trả sẵn kết quả. Chỉ MỘT đường ra Loi: CHÍNH app đã xoá job khỏi
/// hàng đợi và kiểm lại là hết — tức chắc chắn không còn bản nào chờ in. Mọi
/// đường khác → KhongRo (giữ nguyên mã sự cố để giao diện vẫn nói
/// "Đang chờ trong máy in: Hết giấy").
///
/// VÌ SAO "máy in báo lỗi + job KHÔNG có trong hàng đợi" là KhongRo chứ không
/// phải Loi (quyết 24/09, sửa hợp đồng §0.1): với máy in MẠNG (TCP/WSD), job
/// rời hàng đợi Windows ngay khi đã đẩy hết dữ liệu sang BỘ NHỚ MÁY IN. Nếu
/// ngay sau đó máy báo hết giấy mà ta trả Loi, backend gửi lại → nạp giấy xong
/// máy in CẢ HAI bản. Không thấy job thì không có bằng chứng nó chưa tới máy —
/// chống in đôi thắng việc tự thử lại.
pub fn quyet_loi_truoc_khi_in(ly_do: LyDo, go: impl FnOnce() -> KetQuaGoJob) -> KetQuaIn {
    match go() {
        KetQuaGoJob::KhongCoTrongHangDoi => KetQuaIn::KhongRo(LyDo {
            chu: format!("{} — job khong con trong hang doi Windows (co the da o bo nho may in), khong gui lai", ly_do.chu),
            loai: ly_do.loai,
            ban_da_in: None,
        }),
        KetQuaGoJob::DaGoXong => KetQuaIn::Loi(LyDo {
            chu: format!("{} (da xoa job khoi hang doi Windows)", ly_do.chu),
            loai: ly_do.loai,
            ban_da_in: None,
        }),
        KetQuaGoJob::KhongAnToanDeXoa(e) => KetQuaIn::KhongRo(LyDo {
            chu: format!("{} — {}", ly_do.chu, e),
            loai: ly_do.loai,
            ban_da_in: None,
        }),
        KetQuaGoJob::KhongGoDuoc(e) => KetQuaIn::KhongRo(LyDo {
            chu: format!("{} — khong go duoc job khoi hang doi: {}", ly_do.chu, e),
            loai: ly_do.loai,
            ban_da_in: None,
        }),
    }
}

/// Câu chiTiet cho sự cố đọc từ job: cờ + câu của driver nếu có.
fn mo_ta_job(j: &JobHangDoi) -> String {
    let co = su_co::mo_ta_co_job(j.status);
    if j.mo_ta_driver.trim().is_empty() {
        co
    } else {
        format!("{}; driver: {}", co, j.mo_ta_driver.trim())
    }
}

/// Trạng thái máy in (CẤP MÁY) của một vòng đọc (None = không đọc được).
/// Máy USB báo lỗi qua thiết bị (U1) mà cờ spooler "bình thường" → mã USB
/// thắng theo thứ tự ưu tiên §1, `chiTiet` ghép cả hai nguồn.
fn doc_may_in(vong: &VongDoc) -> Option<(MaSuCo, Option<String>)> {
    if vong.khong_tim_thay_may_in {
        return Some((MaSuCo::KhongTimThayMayIn, Some("OpenPrinter: ERROR_INVALID_PRINTER_NAME".to_string())));
    }
    let (ma, ct) = vong.co_may_in.map(|s| tinh_trang_tu_co(s, vong.thuoc_tinh_may_in))?;
    let Some((usb, ma_usb)) = vong.usb.as_ref().and_then(|u| u.ma_su_co().map(|m| (u, m))) else {
        return Some((ma, ct));
    };
    let ct_usb = usb.mo_ta();
    let ct = match ct {
        Some(c) => format!("{}; {}", c, ct_usb),
        None => ct_usb,
    };
    Some((su_co::uu_tien_hon(ma, ma_usb), Some(ct)))
}

/// MỌI mã cấp máy của một vòng đọc (None = không đọc được máy in) — R-B.
/// Gồm mã lỗi đọc thẳng từ thiết bị USB (U1).
pub fn tap_may_in(vong: &VongDoc) -> Option<TapMa> {
    if vong.khong_tim_thay_may_in {
        return Some([MaSuCo::KhongTimThayMayIn].into_iter().collect());
    }
    let mut tap = vong.co_may_in.map(|s| su_co::cac_ma_may_in(s, vong.thuoc_tinh_may_in))?;
    if let Some(ma) = vong.usb.as_ref().and_then(crate::usb_may_in::DocUsb::ma_su_co) {
        tap.them(ma);
    }
    Some(tap)
}

/// Chụp cờ nền (R-B) từ một vòng đọc. `khong_tim_thay_may_in` KHÔNG BAO GIỜ
/// là nền: tên máy in không có trong Windows thì không gì in được — không thể
/// là "cờ báo sai mà máy vẫn in". Không đọc được máy in → nền rỗng.
///
/// Mã lỗi đọc thẳng từ USB (U1) cũng KHÔNG BAO GIỜ là nền: cờ nền sinh ra cho
/// cờ spooler báo sai dai dẳng (HP 4003 qua WSD bật ERROR suốt mà vẫn in); bit
/// lỗi USB thì đã đo là đổi đúng theo giấy — máy lỗi từ trước job thì job gửi
/// xuống cũng nằm chờ trong máy.
pub fn chup_nen(vong: &VongDoc) -> TapMa {
    let khong_tim_thay: TapMa = [MaSuCo::KhongTimThayMayIn].into_iter().collect();
    let chi_spooler = VongDoc { usb: None, ..vong.clone() };
    tap_may_in(&chi_spooler).unwrap_or_default().tru(khong_tim_thay)
}

/// Cờ job cho thấy job KHÔNG chặn hàng đợi dù còn mang cờ lỗi (T2, giám sát
/// vòng 3) — không bao giờ tính là "job kẹt":
/// - DELETING/DELETED: đang bị xoá;
/// - PAUSED: NV tạm dừng job kẹt — spooler bỏ qua nó, in tiếp job sau (bản
///   trước từ chối in MÃI tới khi có người xoá job đó);
/// - PRINTED/COMPLETE/RETAINED: đã in / đã gửi hết byte / giữ lại sau khi in
///   ("Keep printed documents") — cờ lỗi còn sót không chặn job sau.
pub const CO_JOB_KHONG_CHAN: u32 = co::JOB_STATUS_DELETING
    | co::JOB_STATUS_DELETED
    | co::JOB_STATUS_PAUSED
    | co::JOB_STATUS_PRINTED
    | co::JOB_STATUS_COMPLETE
    | co::JOB_STATUS_RETAINED;

/// Mã sự cố của một job ĐANG KẸT hàng đợi (có cờ `CO_JOB_KET`, không mang cờ
/// nào trong `CO_JOB_KHONG_CHAN`), đã đọc lại theo câu driver.
pub fn ma_job_ket(j: &JobHangDoi) -> Option<MaSuCo> {
    if j.status & CO_JOB_KHONG_CHAN != 0 || j.status & CO_JOB_KET == 0 {
        return None;
    }
    su_co::ma_tu_co_job(j.status & !co::JOB_STATUS_BLOCKED_DEVQ).map(|m| su_co::tinh_chinh_theo_chu(m, &j.mo_ta_driver))
}

/// Job đang KẸT hàng đợi (của app hay chương trình khác; bỏ qua job đang bị
/// xoá) — lấy mã ưu tiên cao nhất §1 (R-A/R-J).
pub fn job_dang_ket(jobs: &[JobHangDoi]) -> Option<(&JobHangDoi, MaSuCo)> {
    jobs.iter().filter_map(|j| ma_job_ket(j).map(|m| (j, m))).fold(None, |tot, (j, m)| match tot {
        Some((_, mt)) if su_co::uu_tien_hon(mt, m) == mt => tot,
        _ => Some((j, m)),
    })
}

/// Số hoá đơn của một job CỦA APP, đọc từ tên file (backend đặt
/// `AI-<số>-<khách>-<jobId>.pdf`); không bóc được thì id job đã cắt token.
pub fn so_hoa_don_cua(j: &JobHangDoi) -> Option<String> {
    let id = tach_job_id_tu_ten(&j.document)?;
    Some(job::boc_hoa_don(ten_file(&j.document), &id).map_or_else(|| job::rut_gon_job_id(&id), |(so, _)| so))
}

/// chiTiet cho một job kẹt: KHÔNG ghi tên tài liệu của chương trình khác
/// (có thể là giấy tờ riêng của shop) — chỉ cờ + câu driver.
fn mo_ta_job_ket(j: &JobHangDoi) -> String {
    match la_job_cua_app(&j.document).then(|| so_hoa_don_cua(j)).flatten() {
        Some(so) => format!("hoá đơn {} kẹt trong hàng đợi: {}", so, mo_ta_job(j)),
        None => format!("job khác kẹt trong hàng đợi: {}", mo_ta_job(j)),
    }
}

/// Trạng thái máy in GỘP (R-A(2)): cờ cấp máy + cờ của job đang kẹt trong
/// hàng đợi, lấy mã ưu tiên cao nhất §1. `None` = không đọc được máy in.
///
/// VÌ SAO: máy in mạng chỉ bật cờ trên JOB, cấp máy vẫn "bình thường". Báo
/// `binh_thuong` lúc có job kẹt là nói sai (backend đóng cầu dao, gửi hoá đơn
/// tiếp — hoá đơn đó lại xếp sau job kẹt). Gộp CẢ job của chương trình khác:
/// hoá đơn gửi tới cũng xếp sau nó, và R-J từ chối in vì nó.
pub fn tinh_trang_gop(vong: &VongDoc) -> Option<(MaSuCo, Option<String>)> {
    let (ma_may, ct_may) = doc_may_in(vong)?;
    let Some((j, ma_job)) = vong.hang_doi.as_deref().and_then(job_dang_ket) else {
        return Some((ma_may, ct_may));
    };
    let ct_job = mo_ta_job_ket(j);
    let ct = match ct_may {
        Some(c) => format!("{}; {}", c, ct_job),
        None => ct_job,
    };
    Some((su_co::uu_tien_hon(ma_may, ma_job), Some(ct)))
}

/// Trạng thái máy in đọc được trong MỘT vòng poll.
struct MayInVong {
    /// Mã chặn in cấp máy (kể cả nền) — luật xoá R2 dùng.
    chan: Option<MaSuCo>,
    /// Mã chặn in cấp máy KHÔNG thuộc nền (R-B) — mọi luật "job có thể nằm
    /// trong bộ nhớ máy in" dùng mã này.
    chan_moi: Option<MaSuCo>,
}

/// Báo trạng thái máy in (GỘP cờ job kẹt, R-A(2)) ra ngoài; báo `SuCo` chỉ
/// cho mã chặn in cấp máy KHÔNG thuộc nền — mã nền là tình trạng có từ trước
/// job (luồng rảnh đã báo), không phải sự cố "trong lúc in job này" (R-B).
fn bao_may_in(vong: &VongDoc, nen: TapMa, bao: &dyn Fn(QuanSat)) -> MayInVong {
    if let Some((ma, chi_tiet)) = tinh_trang_gop(vong) {
        bao(QuanSat::MayIn { ma, chi_tiet });
    }
    let tap = tap_may_in(vong);
    let chan = tap.and_then(|t| t.chan_in_uu_tien());
    let chan_moi = tap.and_then(|t| t.tru(nen).chan_in_uu_tien());
    if let Some(ma) = chan_moi {
        let ct = doc_may_in(vong).and_then(|(_, c)| c).unwrap_or_default();
        bao(QuanSat::SuCo { loai: ma, chi_tiet: ct });
    }
    MayInVong { chan, chan_moi }
}

/// Kết luận của bước đọc máy in SAU KHI job rời hàng đợi sạch (R5c, U2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SauKhiRoi {
    /// Không thấy gì bất thường — hoặc máy USB xác nhận IN XONG (BUSY → IDLE).
    Sach,
    /// Cờ spooler báo sự cố chặn in MỚI (R5c): có thể còn trong bộ nhớ máy in
    /// mạng — không có cách nào theo dõi tiếp.
    SuCo(MaSuCo),
    /// Máy USB báo lỗi: hoá đơn nằm trong bộ nhớ máy, tự in khi xử lý xong (U2).
    TrongMayInUsb(MaSuCo),
    /// Máy USB vẫn BUSY quá `SO_LAN_USB_TOI_DA` lần đọc — chưa biết in xong chưa.
    UsbChuaXong,
}

/// Bộ quyết THUẦN của bước sau khi rời hàng đợi (R5c + U2), mỗi lần đọc một bước.
///
/// Máy KHÔNG đọc được USB: luật cũ R5c — `SO_LAN_DOC_MAY_IN_SAU_KHI_ROI` lần
/// sạch là xong. Máy USB (U2): phải thấy máy IN XONG — đã thấy BUSY rồi về
/// IDLE, không lỗi — mới là `Sach`; lỗi USB bất cứ lúc nào → hoá đơn nằm
/// trong bộ nhớ máy; hết trần mà chưa thấy in xong → `UsbChuaXong` (theo dõi
/// tiếp). Chỉ máy KHÔNG có trường STATUS mới được `Sach` sau
/// `SO_LAN_USB_KHONG_THAY_IN` lần "không lỗi" liên tiếp.
///
/// `KhongLoi` (không lỗi, STATUS lạ/thiếu) KHÔNG phải "in xong" với máy đã
/// từng báo STATUS: một lần hỏi chuỗi 1284 trục trặc giữa BUSY và lúc kéo giấy
/// hỏng thì bản trước báo `da_in` (giám sát 25/09). Chỉ máy KHÔNG BAO GIỜ báo
/// STATUS mới đếm `KhongLoi` như lần sạch.
#[derive(Debug, Default)]
pub struct BoSauKhiRoi {
    so_lan: usize,
    da_thay_usb: bool,
    /// Máy đã từng báo STATUS đo được (IDLE/BUSY).
    co_status: bool,
    da_thay_dang_in: bool,
    sach_chua_thay_in: usize,
}

impl BoSauKhiRoi {
    /// `chan_moi` = mã chặn in MỚI của vòng (cờ spooler ngoài nền + lỗi USB);
    /// `usb` = tình trạng USB của vòng (`None` = không đọc được / không hỏi).
    pub fn them(&mut self, chan_moi: Option<MaSuCo>, usb: Option<TinhTrangUsb>) -> Option<SauKhiRoi> {
        self.so_lan += 1;
        if usb.is_some() {
            self.da_thay_usb = true;
        }
        if let Some(TinhTrangUsb::Loi(ma)) = usb {
            return Some(SauKhiRoi::TrongMayInUsb(chan_moi.map_or(ma, |m| su_co::uu_tien_hon(m, ma))));
        }
        if let Some(ma) = chan_moi {
            // Cờ spooler báo lỗi mà máy vẫn đọc được USB → theo dõi được qua USB.
            return Some(if self.da_thay_usb { SauKhiRoi::TrongMayInUsb(ma) } else { SauKhiRoi::SuCo(ma) });
        }
        if !self.da_thay_usb {
            return (self.so_lan >= SO_LAN_DOC_MAY_IN_SAU_KHI_ROI).then_some(SauKhiRoi::Sach);
        }
        match usb {
            Some(TinhTrangUsb::DangIn) => {
                self.co_status = true;
                self.da_thay_dang_in = true;
                self.sach_chua_thay_in = 0;
            }
            // Máy CÓ STATUS: "đã in" CHỈ khi đã thấy BUSY rồi về IDLE (giám sát
            // vòng 2, 25/09). Máy laser in một tờ mất ≥ 5 s, lần đọc 500 ms không
            // thể lỡ; IDLE mà chưa từng BUSY = máy CHƯA in (đang "chuẩn bị"? bỏ
            // lệnh?) — chờ tới trần rồi giao theo dõi tiếp, không bao giờ `da_in`.
            Some(TinhTrangUsb::Ranh) => {
                self.co_status = true;
                if self.da_thay_dang_in {
                    return Some(SauKhiRoi::Sach);
                }
            }
            // Máy KHÔNG BAO GIỜ báo STATUS: chỉ biết "không lỗi" — đủ số lần sạch là xong.
            Some(TinhTrangUsb::KhongLoi) if !self.co_status => {
                self.sach_chua_thay_in += 1;
                if self.sach_chua_thay_in >= SO_LAN_USB_KHONG_THAY_IN {
                    return Some(SauKhiRoi::Sach);
                }
            }
            // STATUS thiếu ở máy có STATUS / không đọc được lần này — chờ tiếp.
            _ => {}
        }
        if self.so_lan >= SO_LAN_USB_TOI_DA {
            return Some(self.het_gio());
        }
        None
    }

    /// Hết trần (số lần đọc hoặc `THOI_GIAN_USB_TOI_DA`) mà chưa kết luận.
    /// Máy USB: chưa thấy in xong → `UsbChuaXong` (theo dõi tiếp qua USB, không
    /// đoán `da_in`). Máy không đọc được USB thì không bao giờ tới đây trước 4 lần.
    pub fn het_gio(&self) -> SauKhiRoi {
        if self.da_thay_usb {
            SauKhiRoi::UsbChuaXong
        } else {
            SauKhiRoi::Sach
        }
    }

    /// Đã thấy máy USB đang in trong bước này (giao theo dõi tiếp với "đã thấy in").
    pub fn da_thay_dang_in(&self) -> bool {
        self.da_thay_dang_in
    }
}

/// Sau khi job rời hàng đợi sạch: đọc máy in tiếp tới khi `BoSauKhiRoi` kết
/// luận (R5c; máy USB: U2). Mọi lần đọc vẫn báo trạng thái/sự cố ra ngoài NGAY.
fn kiem_may_in_sau_khi_roi(
    sp: &mut dyn Spooler,
    nen: TapMa,
    job_id: &str,
    vet: &mut VetJob,
    bao: &dyn Fn(QuanSat),
) -> (SauKhiRoi, bool) {
    let mut bo = BoSauKhiRoi::default();
    let bat_dau = std::time::Instant::now();
    loop {
        sp.cho(POLL_INTERVAL);
        let vong = sp.doc_vong();
        vet.ghi("sau_roi", &vong, job_id);
        let chan_moi = bao_may_in(&vong, nen, bao).chan_moi;
        if let Some(kl) = bo.them(chan_moi, vong.usb.as_ref().map(DocUsb::tinh_trang)) {
            return (kl, bo.da_thay_dang_in());
        }
        if bat_dau.elapsed() >= THOI_GIAN_USB_TOI_DA {
            return (bo.het_gio(), bo.da_thay_dang_in());
        }
    }
}

/// Vết TỪNG hoá đơn (chủ yêu cầu 25/09 — "ghi log ra nhằm cải thiện sau này"):
/// mỗi nhịp đọc ghi một dòng `vet_in` — cờ job của ta trong hàng đợi (+ số
/// trang), cờ máy in, USB — dòng giống nhau liên tiếp được gộp (vẫn ghi lại mỗi
/// `NHIP_GHI_LAI`), và một dòng KẾT cuối cùng. Đủ để dựng lại dòng thời gian
/// "hàng đợi → máy in → giấy ra" của từng hoá đơn ở cửa hàng.
struct VetJob {
    id: String,
    bat_dau: std::time::Instant,
    gop: crate::usb_may_in::GopDong,
}

impl VetJob {
    fn moi(job_id: &str) -> Self {
        Self { id: job::rut_gon_job_id(job_id), bat_dau: std::time::Instant::now(), gop: Default::default() }
    }

    fn ghi(&mut self, giai_doan: &str, vong: &VongDoc, job_id: &str) {
        let dong = format!("gd={} {}", giai_doan, mo_ta_vong(vong, job_id));
        if let Some(d) = self.gop.them(&dong, std::time::Instant::now(), crate::usb_may_in::NHIP_GHI_LAI) {
            nhat_ky::ghi("vet_in", &format!("job={} t={}ms {}", self.id, self.bat_dau.elapsed().as_millis(), d));
        }
    }

    fn ket(&self, kq: &KetQuaIn) {
        nhat_ky::ghi("vet_in", &format!("job={} t={}ms KET {:?}", self.id, self.bat_dau.elapsed().as_millis(), kq));
    }
}

/// Một vòng đọc, gọn cho `vet_in`: `hd=[0x2010/p1] khac=0 may=0x0 usb=0x98/BUSY`.
fn mo_ta_vong(vong: &VongDoc, job_id: &str) -> String {
    let hd = match &vong.hang_doi {
        None => "hd=?".to_string(),
        Some(jobs) => {
            let ta: Vec<String> =
                jobs.iter().filter(|j| la_cua_job(j, job_id)).map(|j| format!("0x{:x}/p{}", j.status, j.trang_da_in)).collect();
            let khac = jobs.len() - ta.len();
            format!("hd=[{}] khac={}", if ta.is_empty() { "vang".to_string() } else { ta.join(",") }, khac)
        }
    };
    let may = if vong.khong_tim_thay_may_in {
        "khong_tim_thay".to_string()
    } else {
        vong.co_may_in.map_or_else(|| "?".to_string(), |c| format!("0x{:x}", c))
    };
    let usb = match (&vong.usb, vong.usb_khong_doc_duoc, vong.la_may_usb) {
        (Some(d), _, _) => d.mo_ta_ngan(),
        (None, true, _) => "KHONG_DOC_DUOC".to_string(),
        (None, false, true) => "-".to_string(),
        (None, false, false) => "khong_usb".to_string(),
    };
    format!("{} may={} usb={}", hd, may, usb)
}

/// Vòng theo dõi một job — viết trên trait `Spooler` để test được.
///
/// Mỗi vòng: đọc máy in + hàng đợi → báo `QuanSat` ra ngoài NGAY → dựng quan
/// sát → `BoSuy`. Dừng khi `BoSuy` kết luận (in xong / KhongRo) hoặc
/// `con_thoi_gian()` trả false (lúc đó ứng viên xoá mới được xử lý). Kết luận
/// `KhongRo` mà lần đọc cuối còn thấy job → báo `ConTrongHangDoi` (R3).
pub fn theo_doi_job_voi(
    sp: &mut dyn Spooler,
    job_id: &str,
    submit_after_epoch_secs: i64,
    nen: TapMa,
    bao: &dyn Fn(QuanSat),
    con_thoi_gian: &mut dyn FnMut() -> bool,
) -> KetQuaIn {
    // Cờ nền (R-B): chụp TRƯỚC khi gọi Sumatra (bước kiểm trước khi in — T3,
    // giám sát vòng 3), KHÔNG phải ở lần đọc đầu của vòng này: vòng này chỉ
    // bắt đầu sau khi Sumatra chạy xong (1–60 s) — sự cố bắt đầu trong khoảng
    // đó từng bị coi là "có từ trước job" → job rời hàng đợi → `da_in` sai.
    // HP 4003 qua WSD bật ERROR cấp máy suốt mà vẫn in — ảnh chụp trước
    // Sumatra vẫn có ERROR nên vẫn là nền.
    let mut bo_suy = BoSuy { nen, ..BoSuy::default() };
    // Sự cố chặn in gần nhất đã thấy (cờ job, hoặc cờ cấp máy NGOÀI nền) — gắn
    // vào KhongRo hết giờ (`bo_sung_loai`). Mã nền không gắn: nó có từ trước
    // job, gắn vào là backend ngắt cầu dao vì một cờ báo sai dai dẳng (R-B).
    let mut su_co_da_thay: Option<MaSuCo> = None;
    // Lần đọc hàng đợi THÀNH CÔNG gần nhất có thấy job của ta không.
    let mut con_trong_hang_doi = false;
    // U1/U2: máy USB cục bộ? + lỗi USB ở lần đọc được USB gần nhất.
    let mut usb = UsbCuaJob::default();
    let mut vet = VetJob::moi(job_id);

    loop {
        let vong = sp.doc_vong();
        vet.ghi("theo_doi", &vong, job_id);
        usb.la_may_usb |= vong.la_may_usb;
        if let Some(d) = &vong.usb {
            usb.loi_cuoi = d.ma_su_co();
        }

        // 1. Máy in.
        let may_in = bao_may_in(&vong, nen, bao);
        if may_in.chan_moi.is_some() {
            su_co_da_thay = may_in.chan_moi;
        }

        // 2. Job.
        let quan_sat: Vec<TrangThaiJob> = match &vong.hang_doi {
            None => vec![TrangThaiJob::LoiTruyVan],
            Some(jobs) => match tim_job(jobs, job_id, submit_after_epoch_secs) {
                None => {
                    con_trong_hang_doi = false;
                    bo_suy.thay_job(false);
                    // Không thấy job mà máy in đang báo sự cố chặn in MỚI: job
                    // có thể đã sang bộ nhớ máy in (đã in dở) hoặc chưa hề tới —
                    // `BoSuy` quyết theo lịch sử. Mực yếu không chặn in, cờ nền
                    // có từ trước job (R-B) → không tính.
                    match may_in.chan_moi {
                        Some(ma) => vec![TrangThaiJob::MayInLoi(ma)],
                        None => vec![TrangThaiJob::KhongThay],
                    }
                }
                Some(j) => {
                    con_trong_hang_doi = true;
                    bo_suy.thay_job(true);
                    // Job KHÁC đang kẹt phía trước (vd hoá đơn trước kẹt giấy trên
                    // máy in mạng — cấp máy vẫn "bình thường"): job ta xếp sau nó,
                    // hết giờ thì `khong_ro` mang MÃ CỦA JOB KẸT (dải nói "Hết giấy
                    // — đang chờ trong máy in", backend ngắt cầu dao) thay vì
                    // "không xác nhận được". Không xoá, không gửi su-co thay job kia.
                    if let Some(ma) =
                        jobs.iter().filter(|x| !la_cua_job(x, job_id)).filter_map(ma_job_ket).reduce(su_co::uu_tien_hon)
                    {
                        su_co_da_thay = Some(ma);
                    }
                    if let Some(ma) = ma_su_co_job(j) {
                        bao(QuanSat::SuCo { loai: ma, chi_tiet: mo_ta_job(j) });
                        su_co_da_thay = Some(ma);
                    }
                    let mut qs = quan_sat_theo(j.status, j.trang_da_in, &j.mo_ta_driver);
                    // Job SẠCH (chỉ đang chờ) mà máy in báo sự cố chặn in MỚI (không
                    // có trong ảnh chụp trước Sumatra) → ứng viên xoá (R2, quyết ở
                    // cuối cửa sổ). Chỉ có sự cố NỀN → KHÔNG bao giờ xoá: nhớ lại
                    // để hết cửa sổ ra `khong_ro(<mã nền>)`, job nằm lại tự in
                    // (xem `BoSuy::nen_chan`).
                    if qs == [TrangThaiJob::DangCho] {
                        if let Some(ma) = may_in.chan_moi {
                            qs = vec![TrangThaiJob::MayInLoi(ma)];
                        } else if !bo_suy.da_thay_in && !bo_suy.da_thay_huy {
                            bo_suy.nen_chan = may_in.chan;
                        }
                    }
                    qs
                }
            },
        };

        for q in quan_sat {
            if let Some(kl) = bo_suy.them(q) {
                let kq = ket_thuc(sp, job_id, kl, &bo_suy, su_co_da_thay, con_trong_hang_doi, usb, &mut vet, bao);
                vet.ket(&kq);
                return kq;
            }
        }
        if !con_thoi_gian() {
            let kl = bo_suy.het_gio();
            let kq = ket_thuc(sp, job_id, kl, &bo_suy, su_co_da_thay, con_trong_hang_doi, usb, &mut vet, bao);
            vet.ket(&kq);
            return kq;
        }
        sp.cho(POLL_INTERVAL);
    }
}

/// Máy USB của job đang theo dõi (U1/U2) — gom trong vòng poll, dùng ở `ket_thuc`.
#[derive(Debug, Clone, Copy, Default)]
struct UsbCuaJob {
    la_may_usb: bool,
    /// Lỗi USB ở lần ĐỌC ĐƯỢC USB gần nhất (`None` = không lỗi / chưa đọc được).
    loi_cuoi: Option<MaSuCo>,
}

/// `loiCuoi` khi hoá đơn nằm trong bộ nhớ máy in USB đang báo lỗi (U2).
fn chu_trong_may_in_usb(ma: MaSuCo) -> String {
    format!(
        "may in bao {} qua USB — hoa don dang nam trong bo nho may in, tu in khi xu ly xong (app theo doi va bao khi in xong)",
        ma.nhan()
    )
}

/// Biến kết luận của `BoSuy` thành KetQuaIn — ba việc cần spooler thật: kiểm
/// máy in sau khi job rời đi (R5c, U2), xoá job ứng viên (R2), báo job còn
/// trong hàng đợi (R3) hoặc trong bộ nhớ máy in USB (U2) để theo dõi tiếp.
#[allow(clippy::too_many_arguments)]
fn ket_thuc(
    sp: &mut dyn Spooler,
    job_id: &str,
    kl: KetLuan,
    bo_suy: &BoSuy,
    su_co_da_thay: Option<MaSuCo>,
    con_trong_hang_doi: bool,
    usb: UsbCuaJob,
    vet: &mut VetJob,
    bao: &dyn Fn(QuanSat),
) -> KetQuaIn {
    match kl {
        // PRINTED trên máy USB chỉ nghĩa là byte cuối đã vào BỘ NHỚ máy in —
        // máy hết giấy vẫn giữ đó (giám sát 25/09): máy USB cũng phải qua U2.
        KetLuan::DaIn { qua_vang: false } if !usb.la_may_usb => KetQuaIn::DaIn,
        KetLuan::DaIn { .. } => match kiem_may_in_sau_khi_roi(sp, bo_suy.nen, job_id, vet, bao) {
            (SauKhiRoi::Sach, _) => KetQuaIn::DaIn,
            (SauKhiRoi::SuCo(ma), _) => KetQuaIn::KhongRo(LyDo::co_loai(
                format!(
                    "job da roi hang doi Windows nhung may in bao {} ngay sau do — co the con trong bo nho may in",
                    ma.nhan()
                ),
                ma,
            )),
            (SauKhiRoi::TrongMayInUsb(ma), _) => {
                bao(QuanSat::TrongMayInUsb { bang_chung: bo_suy.bang_chung(), da_thay_loi: true, da_thay_in: false });
                KetQuaIn::KhongRo(LyDo::co_loai(chu_trong_may_in_usb(ma), ma))
            }
            (SauKhiRoi::UsbChuaXong, da_thay_in) => {
                bao(QuanSat::TrongMayInUsb { bang_chung: bo_suy.bang_chung(), da_thay_loi: false, da_thay_in });
                KetQuaIn::KhongRo(LyDo::co_loai(
                    "may in USB chua in xong sau 30 giay — app theo doi tiep va bao khi in xong",
                    MaSuCo::KhongXacNhan,
                ))
            }
        },
        KetLuan::KhongRo(ly_do) => {
            if con_trong_hang_doi {
                bao(QuanSat::ConTrongHangDoi(bo_suy.bang_chung()));
                return bo_sung_loai(KetQuaIn::KhongRo(ly_do), su_co_da_thay);
            }
            // U2 (giám sát 25/09): job ĐÃ vào hàng đợi rồi rời đi (không bị xoá)
            // đúng lúc máy USB báo lỗi — vd hoá đơn gửi thêm khi máy đang giữ
            // tờ trước: lần đọc rỗng đầu tiên đã thấy lỗi, `BoSuy` kết luận ngay
            // mà không qua U2. Nó đang trong bộ nhớ máy → theo dõi qua USB, báo
            // backend "tự in, KHÔNG in lại". Job chưa từng thấy trong hàng đợi:
            // không biết đã tới máy chưa → giữ nguyên.
            if let Some(ma_usb) = usb.loi_cuoi.filter(|_| bo_suy.da_thay_job && !bo_suy.da_thay_huy) {
                bao(QuanSat::TrongMayInUsb { bang_chung: bo_suy.bang_chung(), da_thay_loi: true, da_thay_in: false });
                let ma = match ly_do.loai {
                    Some(m) if m.la_su_co_may_in() => su_co::uu_tien_hon(m, ma_usb),
                    _ => ma_usb,
                };
                return KetQuaIn::KhongRo(LyDo::co_loai(chu_trong_may_in_usb(ma), ma));
            }
            bo_sung_loai(KetQuaIn::KhongRo(ly_do), su_co_da_thay)
        }
        KetLuan::UngVienXoa(ly_do) => {
            let bc = bo_suy.bang_chung();
            let go = go_job_khoi_hang_doi(sp, job_id, bc.da_thay_in || bc.da_thay_huy);
            let con = go.job_con_trong_hang_doi();
            let kq = quyet_loi_truoc_khi_in(ly_do, || go);
            if con && matches!(kq, KetQuaIn::KhongRo(_)) {
                bao(QuanSat::ConTrongHangDoi(bc));
            }
            ma_loi_khong_tieu_luot(kq, bo_suy.ung_vien_cap_may)
        }
    }
}

/// T4 (giám sát vòng 3): job SẠCH bị gỡ vì máy in (CẤP MÁY) chỉ bật cờ chung
/// chung ERROR (`loi_may_in`) → báo `can_xu_ly` ("máy cần người xử lý").
///
/// VÌ SAO: backend TIÊU một lượt thử với `loi_may_in` (quá 5 → `that_bai`, ~15
/// phút) trong khi dải trên máy shop bảo NV "hệ thống TỰ in lại — KHÔNG in
/// tay": hoá đơn vô tội mất, NV không in tay → khách không có hoá đơn. Cờ cấp
/// máy nói về MÁY, không phải về hoá đơn này → mã không tiêu lượt. `loi_may_in`
/// chỉ còn cho lỗi RIÊNG một job (BLOCKED_DEVQ — driver không in được job đó),
/// để job hỏng thật không lặp "gỡ → gửi lại" mãi.
fn ma_loi_khong_tieu_luot(kq: KetQuaIn, cap_may: bool) -> KetQuaIn {
    match kq {
        KetQuaIn::Loi(mut ly_do) if cap_may && ly_do.loai == Some(MaSuCo::LoiMayIn) => {
            ly_do.loai = Some(MaSuCo::CanXuLy);
            ly_do.chu = format!("{} [co ERROR chung chung cap may — bao can_xu_ly, khong tieu luot thu]", ly_do.chu);
            KetQuaIn::Loi(ly_do)
        }
        kq => kq,
    }
}

/// Phần TÊN FILE của DocumentName — driver có thể để cả đường dẫn.
fn ten_file(document: &str) -> &str {
    document.rsplit(['\\', '/']).next().unwrap_or(document)
}

/// Job "In thử" của chính app (`print-agent-in-thu-<hex>.pdf`, ui.rs).
pub fn la_job_in_thu(document: &str) -> bool {
    ten_file(document).starts_with("print-agent-in-thu-")
}

/// Tên document là job của app: `AI-<số>-<khách>-<jobId>.pdf` (tên backend
/// đặt) hoặc `print-agent-<jobId>-<hex>.pdf` (tên dự phòng,
/// printing::ten_file_in) — VÀ `<jobId>` đúng dạng id backend (T8, giám sát
/// vòng 3: `AI-Report-Q3-x.pdf` của chương trình khác không phải job của app);
/// hoặc job "In thử".
pub fn la_job_cua_app(document: &str) -> bool {
    la_job_in_thu(document) || tach_job_id_tu_ten(document).is_some()
}

/// Id job backend đúng dạng (T8, hợp đồng v4 §0.3):
/// - `<uuid>-<13 số>` (print_jobs.id UUID trên prod) hoặc `<cuid>-<13 số>`
///   (cuid mặc định Prisma: `[a-z0-9]{8,40}`);
/// - `<13 số>-<số>` (không có printJobId);
/// - id CŨ `<token>-<13 số>-<số>` (backend bản trước nhét token vào id).
///
/// UUID có dấu `-` bên trong — tách từ PHẢI, không tách từ trái.
pub fn la_id_backend(id: &str) -> bool {
    let la_so = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let la_ms = |s: &str| s.len() == 13 && la_so(s);
    let la_uuid = |s: &str| {
        let p: Vec<&str> = s.split('-').collect();
        p.len() == 5 && [8, 4, 4, 4, 12].iter().zip(&p).all(|(n, x)| x.len() == *n && x.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    let la_cuid = |s: &str| (8..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let la_token = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && !s.ends_with('-')
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if let Some((ms, n)) = id.split_once('-') {
        if la_ms(ms) && la_so(n) {
            return true;
        }
    }
    let Some((dau, cuoi)) = id.rsplit_once('-') else { return false };
    if la_ms(cuoi) && (la_uuid(dau) || la_cuid(dau)) {
        return true;
    }
    la_so(cuoi) && dau.rsplit_once('-').is_some_and(|(token, ms)| la_ms(ms) && la_token(token))
}

/// Chuẩn hoá tên máy tính để so (T8): bỏ `\\` đầu, chỉ lấy nhãn đầu (bỏ
/// miền `.lan`), chữ thường.
fn chuan_ten_may(s: &str) -> String {
    s.trim().trim_start_matches('\\').split('.').next().unwrap_or("").to_lowercase()
}

/// Job do CHÍNH máy này nộp (pMachineName = tên máy này, không phân biệt hoa
/// thường, bỏ tiền tố `\\`) — T8. Không biết tên (rỗng) → KHÔNG phải của ta:
/// resume nhầm job của máy khác đang "tạm dừng → xoá" là in đôi, còn bỏ sót
/// job của ta chỉ làm nó nằm chờ.
pub fn cung_may(may_tinh_job: &str, ten_may: &str) -> bool {
    let a = chuan_ten_may(may_tinh_job);
    !a.is_empty() && a == chuan_ten_may(ten_may)
}

/// Tách id job backend từ tên file của job CỦA APP (R-E — nhận lại job khi
/// khởi động, lúc không còn bộ nhớ nào nói job nào là của hoá đơn nào):
/// - `AI-<số>-<khách>-<jobId>.pdf` → mọi thứ sau dấu `-` THỨ BA. Backend thay
///   mọi ký tự lạ trong số hoá đơn/tên khách bằng `_` (ten-file-in.ts
///   `doanAnToan`), nên hai đoạn đầu không bao giờ chứa `-`; jobId thì có
///   (`<printJobId>-<ms>` hoặc `<ms>-<n>`).
/// - `print-agent-<jobId>-<hex>.pdf` (tên dự phòng, `printing::ten_file_in`)
///   → bỏ tiền tố và đoạn `-<hex>` cuối.
///
/// `None` khi không theo mẫu, hoặc phần tách ra không đúng dạng id backend
/// (`la_id_backend`, T8).
pub fn tach_job_id_tu_ten(document: &str) -> Option<String> {
    let ten = ten_file(document);
    // Đuôi `.pdf` bỏ nếu có (DocumentName do Sumatra đặt theo tên file; driver
    // lạ có thể bỏ đuôi — id vẫn phải qua bộ lọc ký tự bên dưới).
    let goc = match ten.len().checked_sub(4).and_then(|i| ten.get(i..)) {
        Some(duoi) if duoi.eq_ignore_ascii_case(".pdf") => &ten[..ten.len() - 4],
        _ => ten,
    };
    let id = if let Some(con) = goc.strip_prefix("AI-") {
        let mut phan = con.splitn(3, '-');
        let (so, khach, id) = (phan.next()?, phan.next()?, phan.next()?);
        if so.is_empty() || khach.is_empty() {
            return None;
        }
        id
    } else if let Some(con) = goc.strip_prefix("print-agent-") {
        let (id, duoi) = con.rsplit_once('-')?;
        if duoi.is_empty() || !duoi.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        id
    } else {
        return None;
    };
    la_id_backend(id).then(|| id.to_string())
}

/// Câu `loiCuoi` khi từ chối in vì một hoá đơn trước đang kẹt (R-A).
pub fn chu_ket_hoa_don(so_hoa_don: &str) -> String {
    format!("Máy in đang kẹt hoá đơn {} — chưa gửi hoá đơn này xuống máy in", so_hoa_don)
}

/// Câu `loiCuoi` khi từ chối in vì job của chương trình khác đang kẹt (R-J).
pub const CHU_KET_JOB_KHAC: &str = "Hàng đợi máy in đang kẹt job khác (không phải hoá đơn) — chưa gửi hoá đơn này xuống máy in";

/// Quyết TRƯỚC KHI IN một hoá đơn (R-A/R-J, giám sát vòng 2) — THUẦN.
///
/// Hàng đợi có job (của app hay chương trình khác, không đang bị xoá) mang cờ
/// kẹt (`CO_JOB_KET`) → `Some(lý do)`: KHÔNG gọi Sumatra, trả `loi` ngay. An
/// toàn tuyệt đối — chưa một byte nào của hoá đơn này rời máy tính.
///
/// VÌ SAO: máy in mạng chỉ bật cờ lỗi trên JOB. Hoá đơn mới xếp SAU job kẹt,
/// cờ của nó sạch → hết 15 s ra `khong_ro(khong_xac_nhan)` → ZaloCRM ghi "kiểm
/// máy in rồi mới in lại" → quản lý in lại, còn hoá đơn này tự in khi NV gỡ
/// kẹt → HAI tờ. `loi` thì backend giữ hoá đơn, gửi lại khi máy hết lỗi.
///
/// `hang_doi = None` (không đọc được hàng đợi): dựa vào lần đọc gần nhất (chưa
/// quá 60 s — T2) của luồng theo dõi tiếp (`ket_theo_doi` = số hoá đơn + mã của
/// job của ta đang kẹt). Job chỉ bị BLOCKED_DEVQ (lỗi riêng một job), hoặc
/// mang cờ trong `CO_JOB_KHONG_CHAN` (đã tạm dừng/đã in/đang xoá — T2), không chặn.
///
/// Job kẹt chỉ mang cờ chung chung (`loi_may_in`) → báo `can_xu_ly` thay vì
/// `loi_may_in`: backend TIÊU lượt thử với `loi_may_in` (chặn vòng lặp của job
/// hỏng thật), mà hoá đơn bị từ chối ở đây không hỏng gì — nó chỉ đứng sau một
/// job kẹt cần người gỡ. Giữ `loi_may_in` thì cứ ~15 phút một hoá đơn vô tội
/// thành `that_bai`.
pub fn kiem_hang_doi_truoc_khi_in(
    hang_doi: Option<&[JobHangDoi]>,
    ket_theo_doi: Option<(String, MaSuCo)>,
) -> Option<LyDo> {
    let ma_tu_choi = |ma: MaSuCo| if ma == MaSuCo::LoiMayIn { MaSuCo::CanXuLy } else { ma };
    let Some(jobs) = hang_doi else {
        return ket_theo_doi.map(|(so, ma)| LyDo::co_loai(chu_ket_hoa_don(&so), ma_tu_choi(ma)));
    };
    let (j, ma) = job_dang_ket(jobs)?;
    let chu = match la_job_cua_app(&j.document).then(|| so_hoa_don_cua(j)).flatten() {
        Some(so) => chu_ket_hoa_don(&so),
        None => CHU_KET_JOB_KHAC.to_string(),
    };
    Some(LyDo::co_loai(chu, ma_tu_choi(ma)))
}

/// Kết quả bước kiểm TRƯỚC KHI gọi Sumatra (R-A/R-J, T2, T3, T5).
#[derive(Debug, Clone, PartialEq)]
pub enum KiemTruoc {
    /// KHÔNG in, trả `loi` ngay — chưa một byte nào của hoá đơn rời máy.
    /// `su_kien` = tên dòng file nhật ký.
    TuChoi { ly_do: LyDo, su_kien: &'static str },
    /// In. `nen` = cờ CẤP MÁY chụp NGAY TRƯỚC Sumatra (R-B, T3); `None` = chưa
    /// chụp (dry-run) — `printing::in_pdf` tự chụp trước khi gọi Sumatra.
    In { nen: Option<TapMa> },
}

/// Câu `loiCuoi` khi tên máy in trong cấu hình không còn trong Windows (T5).
pub fn chu_khong_tim_thay_may_in(may_in: &str) -> String {
    format!(
        "Không tìm thấy máy in \"{}\" trong Windows (đã đổi tên/gỡ?) — chưa gửi hoá đơn này xuống máy in",
        may_in
    )
}

/// Quyết TRƯỚC KHI gọi Sumatra — THUẦN, từ MỘT vòng đọc (`doc_vong`: cờ máy in
/// + hàng đợi) ngay trước khi in:
/// 1. T5: tên máy in không có trong Windows (`ERROR_INVALID_PRINTER_NAME`) →
///    `loi(khong_tim_thay_may_in)` ngay, KHÔNG gọi Sumatra. Bản trước gọi
///    Sumatra rồi 15 s sau báo `khong_ro(conTrongHangDoi:false)` — backend ghi
///    "có thể nằm trong bộ nhớ máy in", sai: máy đó không tồn tại. Backend coi
///    mã này là không tiêu lượt → hoá đơn chờ tới khi NV chọn lại máy in.
/// 2. R-A/R-J: hàng đợi có job kẹt → `loi` mã của job kẹt.
/// 3. T3: chụp cờ nền từ chính vòng đọc này (TRƯỚC Sumatra).
///
/// `backend_moi` = kết nối hiện tại đã nhận `cau-hinh` (T2): backend CŨ không
/// có cầu dao, không có "không tiêu lượt" — mỗi lần từ chối tiêu một lượt,
/// vài phút là `that_bai`. Với nó KHÔNG từ chối, in như trước.
pub fn kiem_truoc_khi_in(
    vong: &VongDoc,
    may_in: &str,
    ket_theo_doi: Option<(String, MaSuCo)>,
    backend_moi: bool,
) -> KiemTruoc {
    if backend_moi {
        if vong.khong_tim_thay_may_in {
            return KiemTruoc::TuChoi {
                ly_do: LyDo::co_loai(chu_khong_tim_thay_may_in(may_in), MaSuCo::KhongTimThayMayIn),
                su_kien: "khong_in_khong_tim_thay_may_in",
            };
        }
        if let Some(ly_do) = kiem_hang_doi_truoc_khi_in(vong.hang_doi.as_deref(), ket_theo_doi) {
            return KiemTruoc::TuChoi { ly_do, su_kien: "khong_in_hang_doi_ket" };
        }
    }
    KiemTruoc::In { nen: Some(chup_nen(vong)) }
}

/// Chụp cờ nền của máy in `printer` NGAY BÂY GIỜ (một vòng đọc) — cho đường in
/// không qua bước kiểm của worker (nút "In thử").
pub fn chup_nen_may_in(printer: &str) -> TapMa {
    chup_nen(&mo_spooler(printer).doc_vong())
}

/// Khi app khởi động (R11b): job CỦA APP đang PAUSED trong hàng đợi → RESUME.
///
/// VÌ SAO: `go_job_khoi_hang_doi` tạm dừng rồi mới xoá; app bị tắt đúng giữa
/// hai bước đó thì job nằm "Paused" vĩnh viễn — backend chưa nhận `loi` (không
/// gửi lại), hoá đơn không bao giờ ra giấy. Cho chạy tiếp là an toàn: chưa kết
/// quả nào báo backend gửi lại job này.
///
/// CHỈ job do CHÍNH máy này nộp (T8): hàng đợi chia sẻ `\\PC\may` có job
/// của máy khác — app máy kia có thể đang ở giữa "tạm dừng → xoá" của nó;
/// resume đúng khe đó là byte tới máy in rồi mới bị xoá + báo `loi` → in đôi.
///
/// Trả (JobId, document, kết quả RESUME) cho mỗi job đã thử — người gọi ghi nhật ký.
pub fn tiep_tuc_job_bi_dung_cua_app(sp: &mut dyn Spooler, ten_may: &str) -> Vec<(u32, String, Result<(), String>)> {
    let Some(jobs) = sp.doc_hang_doi() else { return Vec::new() };
    jobs.into_iter()
        .filter(|j| la_job_cua_app(&j.document) && j.status & co::JOB_STATUS_PAUSED != 0 && cung_may(&j.may_tinh, ten_may))
        .map(|j| {
            let kq = sp.dieu_khien(j.id, LenhJob::TiepTuc);
            (j.id, j.document, kq)
        })
        .collect()
}

// ===================== Phần Win32 thật (chỉ Windows) =====================

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::{Instant, SystemTime};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_INVALID_PRINTER_NAME, HANDLE};
    use windows::Win32::Graphics::Printing::{
        ClosePrinter, EnumJobsW, GetPrinterW, OpenPrinterW, SetJobW, JOB_CONTROL_DELETE, JOB_CONTROL_PAUSE,
        JOB_CONTROL_RESUME, JOB_INFO_2W, PRINTER_INFO_2W,
    };

    /// Chuyển chuỗi Rust → UTF-16 kết thúc \0 (Win32 PCWSTR cần null-terminated).
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Mở handle tới máy in theo tên (đóng bằng ClosePrinter khi xong).
    struct PrinterHandle(HANDLE);
    impl Drop for PrinterHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = ClosePrinter(self.0);
            }
        }
    }

    enum LoiMo {
        /// ERROR_INVALID_PRINTER_NAME — tên trong config không có trong Windows.
        KhongTimThay,
        Khac,
    }

    /// OpenPrinterW với quyền mặc định (PRINTER_ACCESS_USE). Đủ để tạm dừng/xoá
    /// job DO CHÍNH NGƯỜI DÙNG NÀY gửi (CREATOR OWNER có "Manage documents") —
    /// Sumatra chạy dưới cùng tài khoản với app. Cần kiểm trên máy thật.
    fn mo_may_in(printer: &str) -> Result<PrinterHandle, LoiMo> {
        let wide = to_wide(printer);
        let mut handle = HANDLE::default();
        match unsafe { OpenPrinterW(PCWSTR(wide.as_ptr()), &mut handle, None) } {
            Ok(()) => Ok(PrinterHandle(handle)),
            Err(e) if e.code() == ERROR_INVALID_PRINTER_NAME.to_hresult() => Err(LoiMo::KhongTimThay),
            Err(_) => Err(LoiMo::Khac),
        }
    }

    /// (PRINTER_INFO_2W.Status, .Attributes, .pPortName, .pServerName) đọc trong
    /// CÙNG một lần GetPrinterW (None nếu query lỗi). Cổng + máy chủ cho U1.
    fn doc_co_may_in(h: &PrinterHandle) -> Option<(u32, u32, String, String)> {
        let can = std::mem::size_of::<PRINTER_INFO_2W>();
        let mut needed: u32 = 0;
        // Lần gọi 1: chỉ để lấy kích thước buffer cần (luôn lỗi INSUFFICIENT_BUFFER).
        unsafe {
            let _ = GetPrinterW(h.0, 2, None, &mut needed);
        }
        // Kiểm biên TRƯỚC read_unaligned (R11c): buffer nhỏ hơn struct mà vẫn
        // đọc là đọc ra ngoài vùng nhớ.
        if (needed as usize) < can {
            return None;
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = unsafe { GetPrinterW(h.0, 2, Some(&mut buf), &mut needed) };
        if ok.is_err() || buf.len() < can {
            return None;
        }
        // read_unaligned: Vec<u8> chỉ bảo đảm căn 1 byte, PRINTER_INFO_2W cần căn
        // con trỏ — đọc lệch qua `&*` là UB dù x64 hiếm khi lộ ra.
        let info = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const PRINTER_INFO_2W) };
        // pPortName/pServerName trỏ vào `buf` — còn sống tới hết hàm.
        let cong = unsafe { pwstr_to_string(info.pPortName) };
        let may_chu = unsafe { pwstr_to_string(info.pServerName) };
        Some((info.Status, info.Attributes, cong, may_chu))
    }

    /// EnumJobs cấp độ 2 (JOB_INFO_2W), đọc CHẶT (R5b): lần hỏi kích thước lỗi
    /// mà `needed == 0` là KHÔNG đọc được (None), không phải "hàng đợi rỗng".
    ///
    /// Bản trước để vòng poll đọc dễ dãi (mọi `needed == 0` là rỗng): EnumJobs
    /// lỗi giữa lúc job đang in thành hai lần "vắng" → báo `da_in` cho job có
    /// thể đang kẹt giấy.
    fn doc_danh_sach_job(h: &PrinterHandle) -> Option<Vec<JobHangDoi>> {
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;
        let hoi_co = unsafe { EnumJobsW(h.0, 0, u32::MAX, 2, None, &mut needed, &mut returned) };
        if needed == 0 {
            // Hàng đợi rỗng — EnumJobs THÀNH CÔNG với 0 job.
            return hoi_co.is_ok().then(Vec::new);
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = unsafe { EnumJobsW(h.0, 0, u32::MAX, 2, Some(&mut buf), &mut needed, &mut returned) };
        if ok.is_err() {
            return None;
        }
        // Kiểm biên TRƯỚC read_unaligned (R11c).
        let can = (returned as usize).checked_mul(std::mem::size_of::<JOB_INFO_2W>())?;
        if can > buf.len() {
            return None;
        }
        let ptr = buf.as_ptr() as *const JOB_INFO_2W;
        let mut ra = Vec::with_capacity(returned as usize);
        for i in 0..returned as usize {
            // read_unaligned (chép struct ra): Vec<u8> chỉ bảo đảm căn 1 byte,
            // JOB_INFO_2W chứa con trỏ. Các PWSTR bên trong vẫn trỏ vào `buf`
            // — còn sống tới hết hàm.
            let job = unsafe { std::ptr::read_unaligned(ptr.add(i)) };
            ra.push(JobHangDoi {
                id: job.JobId,
                document: unsafe { pwstr_to_string(job.pDocument) },
                status: job.Status,
                trang_da_in: job.PagesPrinted,
                mo_ta_driver: unsafe { pwstr_to_string(job.pStatus) },
                // JOB_INFO_2W.Submitted là SYSTEMTIME UTC — quy đổi ra giây từ
                // UNIX epoch để so sánh ĐÚNG ĐƠN VỊ với mốc gửi lệnh in.
                submitted_epoch_secs: systemtime_utc_to_epoch_secs(&job.Submitted),
                may_tinh: unsafe { pwstr_to_string(job.pMachineName) },
            });
        }
        Some(ra)
    }

    /// Đọc chuỗi UTF-16 null-terminated từ con trỏ PWSTR (rỗng nếu null).
    unsafe fn pwstr_to_string(p: windows::core::PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        p.to_string().unwrap_or_default()
    }

    /// Quy đổi SYSTEMTIME (UTC — JOB_INFO_2W.Submitted đã là UTC theo tài liệu
    /// Win32) → giây kể từ UNIX epoch. Không dùng crate ngày giờ ngoài (chrono)
    /// chỉ cho 1 chỗ — thuật toán lịch civil_from_days chuẩn (Howard Hinnant),
    /// không phụ thuộc múi giờ hệ thống.
    fn systemtime_utc_to_epoch_secs(st: &windows::Win32::Foundation::SYSTEMTIME) -> i64 {
        fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
            let y = if m <= 2 { y - 1 } else { y };
            let era = if y >= 0 { y } else { y - 399 } / 400;
            let yoe = y - era * 400; // [0, 399]
            let mp = (m + 9) % 12; // [0, 11]
            let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
            let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
            era * 146097 + doe - 719468
        }
        let days = days_from_civil(st.wYear as i64, st.wMonth as i64, st.wDay as i64);
        days * 86400
            + (st.wHour as i64) * 3600
            + (st.wMinute as i64) * 60
            + (st.wSecond as i64)
    }

    /// Spooler Windows thật. Mở handle MỚI cho mỗi thao tác (như bản trước mỗi
    /// vòng poll): máy in bị gỡ/đổi tên giữa chừng thì lần sau báo đúng lỗi,
    /// không giữ handle chết.
    pub struct SpoolerWin<'a> {
        pub printer: &'a str,
    }

    impl Spooler for SpoolerWin<'_> {
        fn doc_vong(&mut self) -> VongDoc {
            match mo_may_in(self.printer) {
                Err(LoiMo::KhongTimThay) => VongDoc { khong_tim_thay_may_in: true, ..VongDoc::default() },
                Err(LoiMo::Khac) => VongDoc::default(),
                Ok(h) => {
                    use crate::usb_may_in::{self as usb, DocCong};
                    let may_in = doc_co_may_in(&h);
                    let hang_doi = doc_danh_sach_job(&h);
                    let la_may_usb =
                        may_in.as_ref().is_some_and(|(_, a, cong, may_chu)| usb::la_cong_usb_cuc_bo(may_chu, *a, cong).is_some());
                    // U1: hỏi thẳng thiết bị USB CHỈ khi mọi job đã gửi xong (không
                    // chen vào lúc spooler đang đẩy byte xuống cổng).
                    let doc = match (&may_in, &hang_doi) {
                        (Some((_, a, cong, may_chu)), Some(jobs)) if la_may_usb && hang_doi_cho_doc_usb(jobs) => {
                            usb::doc_theo_cong(may_chu, *a, cong)
                        }
                        _ => DocCong::KhongPhaiUsb,
                    };
                    VongDoc {
                        co_may_in: may_in.as_ref().map(|(s, _, _, _)| *s),
                        thuoc_tinh_may_in: may_in.as_ref().map_or(0, |(_, a, _, _)| *a),
                        hang_doi,
                        khong_tim_thay_may_in: false,
                        la_may_usb,
                        usb_khong_doc_duoc: doc == DocCong::KhongDocDuoc,
                        usb: match doc {
                            DocCong::Doc(d) => Some(d),
                            _ => None,
                        },
                    }
                }
            }
        }

        fn doc_hang_doi(&mut self) -> Option<Vec<JobHangDoi>> {
            let h = mo_may_in(self.printer).ok()?;
            doc_danh_sach_job(&h)
        }

        fn dieu_khien(&mut self, id: u32, lenh: LenhJob) -> Result<(), String> {
            let h = mo_may_in(self.printer).map_err(|_| "khong mo duoc may in".to_string())?;
            let lenh = match lenh {
                LenhJob::TamDung => JOB_CONTROL_PAUSE,
                LenhJob::TiepTuc => JOB_CONTROL_RESUME,
                LenhJob::Xoa => JOB_CONTROL_DELETE,
            };
            unsafe { SetJobW(h.0, id, 0, None, lenh) }.ok().map_err(|e| e.to_string())
        }

        fn cho(&mut self, d: Duration) {
            std::thread::sleep(d);
        }
    }

    /// Đọc trạng thái máy in một lần (luồng theo dõi máy in lúc rảnh) — GỘP
    /// cờ cấp máy với cờ của job đang kẹt trong hàng đợi (R-A(2)).
    /// `None` = không đọc được (spooler lỗi…) — KHÔNG phải "bình thường".
    pub fn doc_tinh_trang_may_in(printer: &str) -> Option<(MaSuCo, Option<String>)> {
        tinh_trang_gop(&SpoolerWin { printer }.doc_vong())
    }

    /// Poll spooler tối đa POLL_TIMEOUT, mỗi POLL_INTERVAL.
    pub fn theo_doi_job(printer: &str, job_id: &str, submit_after: SystemTime, nen: TapMa, bao: &dyn Fn(QuanSat)) -> KetQuaIn {
        let submit_after_epoch_secs =
            submit_after.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let bat_dau = Instant::now();
        let mut sp = SpoolerWin { printer };
        theo_doi_job_voi(&mut sp, job_id, submit_after_epoch_secs, nen, bao, &mut || bat_dau.elapsed() < POLL_TIMEOUT)
    }
}

/// `nen` = cờ cấp máy chụp TRƯỚC khi gọi Sumatra (R-B, T3).
#[cfg(windows)]
pub fn theo_doi_job(printer: &str, job_id: &str, submit_after: std::time::SystemTime, nen: TapMa, bao: &dyn Fn(QuanSat)) -> KetQuaIn {
    win::theo_doi_job(printer, job_id, submit_after, nen, bao)
}

#[cfg(windows)]
pub fn doc_tinh_trang_may_in(printer: &str) -> Option<(MaSuCo, Option<String>)> {
    win::doc_tinh_trang_may_in(printer)
}

/// Spooler thật của máy in `printer` — cho luồng theo dõi tiếp (R3) và bước
/// dọn job bị dừng lúc khởi động (R11b).
#[cfg(windows)]
pub fn mo_spooler(printer: &str) -> Box<dyn Spooler + '_> {
    Box::new(win::SpoolerWin { printer })
}

/// Mac/dev (không có spooler Windows) — stub chỉ để compile; hành vi in thật
/// LUÔN chạy trên Windows qua nhánh #[cfg(windows)] ở trên.
#[cfg(not(windows))]
pub fn theo_doi_job(
    _printer: &str,
    _job_id: &str,
    _submit_after: std::time::SystemTime,
    _nen: TapMa,
    _bao: &dyn Fn(QuanSat),
) -> KetQuaIn {
    KetQuaIn::DaIn
}

/// Mac/dev: không có máy in Windows để đọc — "không đọc được", không phải "bình thường".
#[cfg(not(windows))]
pub fn doc_tinh_trang_may_in(_printer: &str) -> Option<(MaSuCo, Option<String>)> {
    None
}

/// Mac/dev: spooler "không đọc được gì" — theo dõi tiếp/dọn job không làm gì.
#[cfg(not(windows))]
pub fn mo_spooler(_printer: &str) -> Box<dyn Spooler + '_> {
    struct SpoolerRong;
    impl Spooler for SpoolerRong {
        fn doc_vong(&mut self) -> VongDoc {
            VongDoc::default()
        }
        fn doc_hang_doi(&mut self) -> Option<Vec<JobHangDoi>> {
            None
        }
        fn dieu_khien(&mut self, _id: u32, _lenh: LenhJob) -> Result<(), String> {
            Err("khong co spooler Windows".into())
        }
        fn cho(&mut self, d: Duration) {
            std::thread::sleep(d);
        }
    }
    Box::new(SpoolerRong)
}

/// Spooler giả dùng chung cho test của spooler.rs và theo_doi_tiep.rs.
#[cfg(test)]
pub(crate) mod gia {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    pub const ID: &str = "tokHN-1727170000000-3";
    /// Tên máy tính của app trong test (COMPUTERNAME).
    pub const MAY: &str = "PC-SHOP";

    pub fn job(id: u32, status: u32) -> JobHangDoi {
        JobHangDoi {
            id,
            document: format!("AI-INV_2026_030045-Anh_Loc-{}.pdf", ID),
            status,
            trang_da_in: 0,
            mo_ta_driver: String::new(),
            submitted_epoch_secs: 1_000,
            may_tinh: r"\\PC-SHOP".into(),
        }
    }

    pub fn job_khac(id: u32) -> JobHangDoi {
        JobHangDoi { document: "Microsoft Word - bao gia.docx".into(), ..job(id, 0) }
    }

    pub fn vong(co_may_in: u32, jobs: Vec<JobHangDoi>) -> VongDoc {
        VongDoc { co_may_in: Some(co_may_in), hang_doi: Some(jobs), ..VongDoc::default() }
    }

    /// Ba trạng thái USB ĐO THẬT ở máy HCM (HP Laser 103 107 108, 25/09).
    pub const USB_RANH: (u8, &str) = (0x18, "IDLE");
    pub const USB_DANG_IN: (u8, &str) = (0x98, "BUSY");
    pub const USB_HET_GIAY: (u8, &str) = (0x90, "BUSY");

    /// Vòng đọc của máy USB lúc hàng đợi RỖNG (chỉ lúc đó mới đọc USB — U1),
    /// cờ spooler "bình thường" như máy HCM thật.
    pub fn vong_usb((byte, status): (u8, &str)) -> VongDoc {
        VongDoc {
            la_may_usb: true,
            usb: Some(crate::usb_may_in::DocUsb { byte, status: Some(status.to_string()) }),
            ..vong(0, vec![])
        }
    }

    /// Vòng đọc của máy USB lúc hàng đợi còn job đang gửi (không hỏi thiết bị).
    pub fn vong_may_usb(jobs: Vec<JobHangDoi>) -> VongDoc {
        VongDoc { la_may_usb: true, ..vong(0, jobs) }
    }

    /// Máy USB mà hỏi thiết bị không được (máy in tắt / rút dây).
    pub fn vong_usb_mat() -> VongDoc {
        VongDoc { la_may_usb: true, usb_khong_doc_duoc: true, ..vong(0, vec![]) }
    }

    #[derive(Default)]
    pub struct SpoolerGia {
        /// Trả lần lượt cho `doc_vong`; hết thì lặp vòng cuối.
        pub vong: Vec<VongDoc>,
        pub so_vong_da_doc: usize,
        /// Hàng đợi cho `doc_hang_doi` (đọc chặt).
        pub hang_doi: Option<Vec<JobHangDoi>>,
        pub lenh: Vec<(u32, LenhJob)>,
        pub lenh_loi: Option<LenhJob>,
        /// Sau lệnh xoá: hàng đợi rỗng từ lần đọc thứ (n+1); None = không bao giờ hết.
        pub het_sau_so_lan_doc: Option<usize>,
        /// Giả lập NV vừa nạp giấy: job bắt đầu in đúng lúc ta tạm dừng.
        pub in_ngay_khi_tam_dung: bool,
        /// Giả lập spooler kịp gửi byte giữa "tạm dừng" và "xoá": sau lệnh xoá,
        /// job còn trong hàng đợi mang PRINTING|DELETING (R-K).
        pub sau_xoa_thay_in: bool,
        pub da_xoa: bool,
        pub so_lan_doc_sau_xoa: usize,
        /// Dòng thời gian dùng chung với `bao` để kiểm "báo NGAY".
        pub su_kien: Rc<RefCell<Vec<String>>>,
    }

    impl Spooler for SpoolerGia {
        fn doc_vong(&mut self) -> VongDoc {
            let i = self.so_vong_da_doc.min(self.vong.len() - 1);
            self.so_vong_da_doc += 1;
            self.vong[i].clone()
        }
        fn doc_hang_doi(&mut self) -> Option<Vec<JobHangDoi>> {
            self.su_kien.borrow_mut().push("doc_hang_doi".into());
            if self.da_xoa {
                self.so_lan_doc_sau_xoa += 1;
                if self.het_sau_so_lan_doc.is_some_and(|n| self.so_lan_doc_sau_xoa > n) {
                    return Some(Vec::new());
                }
                if self.sau_xoa_thay_in {
                    let mut v = self.hang_doi.clone()?;
                    for j in &mut v {
                        j.status |= co::JOB_STATUS_PRINTING | co::JOB_STATUS_DELETING;
                    }
                    return Some(v);
                }
            }
            self.hang_doi.clone()
        }
        fn dieu_khien(&mut self, id: u32, lenh: LenhJob) -> Result<(), String> {
            self.lenh.push((id, lenh));
            if self.lenh_loi == Some(lenh) {
                return Err("ERROR_ACCESS_DENIED".into());
            }
            match lenh {
                LenhJob::TamDung => {
                    // Như spooler thật: job bị dừng mang cờ PAUSED.
                    for j in self.hang_doi.iter_mut().flatten().filter(|j| j.id == id) {
                        j.status |= co::JOB_STATUS_PAUSED;
                        if self.in_ngay_khi_tam_dung {
                            j.status |= co::JOB_STATUS_PRINTING;
                        }
                    }
                }
                LenhJob::TiepTuc => {
                    for j in self.hang_doi.iter_mut().flatten().filter(|j| j.id == id) {
                        j.status &= !co::JOB_STATUS_PAUSED;
                    }
                }
                LenhJob::Xoa => self.da_xoa = true,
            }
            Ok(())
        }
        fn cho(&mut self, d: Duration) {
            self.su_kien.borrow_mut().push(format!("cho {}ms", d.as_millis()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TrangThaiJob::*;

    // --- A: PRINTED quan sát trực tiếp → DaIn ---
    #[test]
    fn a_thay_printed_tra_da_in() {
        let kq = suy_ket_qua(&[DangCho, DangIn, DaInXong]);
        assert_eq!(kq, KetQuaIn::DaIn);
    }

    #[test]
    fn a2_printed_ngay_lan_dau_tra_da_in() {
        assert_eq!(suy_ket_qua(&[DaInXong]), KetQuaIn::DaIn);
    }

    // --- B: cờ lỗi TRÊN JOB → KhongRo, KHÔNG BAO GIỜ Loi (R2) ---

    /// ĐỔI HÀNH VI 25/09 (trước: Loi). JOB_STATUS OFFLINE/PAPEROUT/ERROR do
    /// port monitor bật TRONG LÚC GỬI — máy in mạng có thể đã đệm một phần.
    #[test]
    fn b_loi_tren_job_truoc_khi_thay_in_la_khong_ro_khong_phai_loi() {
        let kq = suy_ket_qua(&[DangCho, LoiJob(MaSuCo::Offline)]);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::Offline)), "got {:?}", kq);
    }

    #[test]
    fn b2_may_in_loi_ngay_dau_la_ung_vien_xoa() {
        let kq = suy_ket_qua(&[MayInLoi(MaSuCo::HetGiay)]);
        assert!(matches!(kq, KetQuaIn::Loi(_)));
    }

    // --- C: đã bắt đầu in rồi timeout/mất dấu mà chưa thấy PRINTED → KhongRo ---
    #[test]
    fn c_dang_in_roi_timeout_tra_khong_ro() {
        let kq = suy_ket_qua(&[DangCho, DangIn, DangCho, DangIn]); // hết chuỗi quan sát, chưa PRINTED
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "expect KhongRo, got {:?}", kq);
    }

    #[test]
    fn c2_dang_in_roi_loi_van_khong_ro_khong_phai_loi() {
        // đã in rồi mới lỗi -> KHÔNG được suy Loi (tránh in đôi vì có thể đã ra giấy)
        let kq = suy_ket_qua(&[DangIn, LoiJob(MaSuCo::KetGiay)]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "expect KhongRo, got {:?}", kq);
    }

    /// ĐỔI HÀNH VI 18/09 (trước đây bài này khẳng định KhongRo — chính là bug).
    ///
    /// Đang in rồi job RỜI HÀNG ĐỢI SẠCH = ĐÃ IN XONG. Đo thật trên .207 (máy in
    /// ảo, poll 50ms): Spooling ×7 → Printing ×3 → biến mất, file PDF RA THẬT
    /// 310KB, `PRINTED` không xuất hiện lần nào. Giữ KhongRo ở đây nghĩa là MỌI
    /// lần in thành công đều bị báo "không rõ" — đúng 3 job khong_ro của máy HCM
    /// ngày 14–15/09, giấy đã ra mà hệ thống không biết.
    #[test]
    fn c3_dang_in_roi_job_roi_hang_doi_sach_la_da_in() {
        let kq = suy_ket_qua(&[DangIn, KhongThay, KhongThay]);
        assert_eq!(kq, KetQuaIn::DaIn, "rời hàng đợi sạch sau khi đang in = in xong");
    }

    /// PHẢN CHỨNG cho c3 — vắng MỘT lần chưa đủ kết luận.
    #[test]
    fn c3b_vang_mot_lan_roi_thay_lai_thi_khong_tinh_la_xong() {
        let kq = suy_ket_qua(&[DangIn, KhongThay, DangIn, KhongThay]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)),
            "1 lần vắng xen giữa không đủ; hết chuỗi mà job vẫn còn → KhongRo, got {:?}", kq);
    }

    /// PHẢN CHỨNG — không được suy DaIn khi CHƯA từng thấy job đang in.
    #[test]
    fn c3c_chua_tung_thay_dang_in_thi_vang_bao_nhieu_cung_khong_phai_da_in() {
        let kq = suy_ket_qua(&[KhongThay, KhongThay, KhongThay, KhongThay]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)),
            "chưa thấy đang in thì vắng không chứng minh được gì, got {:?}", kq);
    }

    /// PHẢN CHỨNG — lỗi đọc hàng đợi KHÁC với job đã rời đi (R5b).
    #[test]
    fn c3d_loi_truy_van_khong_duoc_tinh_la_roi_hang_doi() {
        let kq = suy_ket_qua(&[DangIn, LoiTruyVan, LoiTruyVan, LoiTruyVan]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)),
            "không đọc được hàng đợi ≠ job đã in xong, got {:?}", kq);
    }

    /// PHẢN CHỨNG — job kẹt giấy NẰM LẠI hàng đợi kèm cờ lỗi, không rời đi.
    #[test]
    fn c3e_ket_giay_thi_van_khong_ro_khong_thanh_da_in() {
        let kq = suy_ket_qua(&[DangIn, LoiJob(MaSuCo::HetGiay), KhongThay, KhongThay]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)),
            "gặp lỗi sau khi đang in phải KhongRo, không được thành DaIn, got {:?}", kq);
    }

    // --- R5a: thấy huỷ / khởi động lại thì biến mất KHÔNG tính là in ---
    #[test]
    fn r5a_bi_huy_roi_bien_mat_khong_phai_da_in() {
        for chuoi in [
            vec![DangIn, BiHuy, KhongThay, KhongThay],
            vec![DangCho, BiHuy, KhongThay, KhongThay, KhongThay],
            vec![BiHuy, DangIn, KhongThay, KhongThay],
        ] {
            let kq = suy_ket_qua(&chuoi);
            let KetQuaIn::KhongRo(l) = kq else { panic!("{:?} → {:?}", chuoi, kq) };
            assert_eq!(l.loai, Some(MaSuCo::KhongXacNhan));
            assert!(l.chu.contains("huỷ/khởi động lại"), "{}", l.chu);
        }
    }

    #[test]
    fn r5a_restart_va_deleting_la_bi_huy() {
        assert_eq!(quan_sat_job(JOB_STATUS_RESTART | JOB_STATUS_PRINTING), vec![DangIn, BiHuy]);
        assert_eq!(quan_sat_job(JOB_STATUS_DELETING), vec![BiHuy]);
        assert_eq!(quan_sat_job(JOB_STATUS_DELETED), vec![BiHuy]);
    }

    // --- D: job KHÔNG BAO GIỜ tìm thấy (Sumatra exit 0 nhưng in quá nhanh) → KhongRo ---
    #[test]
    fn d_khong_bao_gio_thay_job_tra_khong_ro() {
        let kq = suy_ket_qua(&[KhongThay, KhongThay, KhongThay]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)));
    }

    // --- E: lỗi truy vấn spooler (observability) khi CHƯA quan sát gì → KhongRo, KHÔNG Loi ---
    #[test]
    fn e_loi_truy_van_chua_quan_sat_gi_khong_ro_khong_phai_loi() {
        let kq = suy_ket_qua(&[LoiTruyVan, LoiTruyVan]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "expect KhongRo, got {:?}", kq);
    }

    // --- F: chờ (spooling) rồi PRINTED → DaIn (không bị nhiễu bởi DangCho xen giữa) ---
    #[test]
    fn f_cho_lau_roi_in_xong_tra_da_in() {
        let kq = suy_ket_qua(&[DangCho, DangCho, DangCho, DangIn, DaInXong]);
        assert_eq!(kq, KetQuaIn::DaIn);
    }

    // --- G: PRINTED xuất hiện sau đó job biến mất (dọn hàng đợi) vẫn tính DaIn ---
    #[test]
    fn g_printed_roi_bien_mat_van_da_in() {
        let kq = suy_ket_qua(&[DangIn, DaInXong, KhongThay]);
        assert_eq!(kq, KetQuaIn::DaIn);
    }

    // --- H: máy in lỗi xen giữa lúc đang chờ (chưa in) → ứng viên xoá ---
    #[test]
    fn h_loi_may_in_khi_dang_cho_la_ung_vien_xoa() {
        let kq = suy_ket_qua(&[DangCho, MayInLoi(MaSuCo::Offline)]);
        assert!(matches!(kq, KetQuaIn::Loi(_)));
        // máy in hết lỗi ở vòng sau → không còn ứng viên
        let kq = suy_ket_qua(&[MayInLoi(MaSuCo::Offline), DangCho]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "{:?}", kq);
    }

    // --- I: chuỗi rỗng (không poll được lần nào) → KhongRo, không panic ---
    #[test]
    fn i_chuoi_rong_tra_khong_ro_khong_panic() {
        let kq = suy_ket_qua(&[]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)));
    }

    // ================= Mã §1, cờ job, xoá job trước khi báo Loi =================

    use crate::su_co::co::*;
    use gia::{job, job_khac, vong, vong_may_usb, vong_usb, SpoolerGia, ID, USB_DANG_IN, USB_HET_GIAY, USB_RANH};
    use std::cell::RefCell;

    #[test]
    fn quan_sat_job_tu_co() {
        assert_eq!(quan_sat_job(JOB_STATUS_PRINTED | JOB_STATUS_ERROR), vec![DaInXong]);
        assert_eq!(quan_sat_job(JOB_STATUS_PRINTING), vec![DangIn]);
        assert_eq!(quan_sat_job(0), vec![DangCho]);
        assert_eq!(quan_sat_job(JOB_STATUS_SPOOLING), vec![DangCho]);
        assert_eq!(quan_sat_job(JOB_STATUS_PAUSED), vec![DangCho]);
        assert_eq!(quan_sat_job(JOB_STATUS_PAPEROUT), vec![LoiJob(MaSuCo::HetGiay)]);
        assert_eq!(quan_sat_job(JOB_STATUS_USER_INTERVENTION), vec![LoiJob(MaSuCo::CanXuLy)]);
        assert_eq!(quan_sat_job(JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT), vec![DangIn, LoiJob(MaSuCo::HetGiay)]);
        // BLOCKED_DEVQ một mình = bị giữ ở hàng đợi (ứng viên); kèm ERROR = lỗi job
        assert_eq!(quan_sat_job(JOB_STATUS_BLOCKED_DEVQ), vec![KetHangDoi(MaSuCo::LoiMayIn)]);
        assert_eq!(quan_sat_job(JOB_STATUS_BLOCKED_DEVQ | JOB_STATUS_ERROR), vec![LoiJob(MaSuCo::LoiMayIn)]);
        // COMPLETE: đã gửi hết byte cho máy in
        assert_eq!(quan_sat_job(JOB_STATUS_COMPLETE), vec![DangIn]);
        // PagesPrinted > 0 là bằng chứng đã in
        assert_eq!(quan_sat_theo(0, 1, ""), vec![DangIn]);
    }

    /// ĐỔI HÀNH VI có chủ đích (hợp đồng v2 §0.1): bản trước xét cờ lỗi TRƯỚC
    /// cờ PRINTING, nên lần ĐẦU thấy job ở PRINTING|PAPEROUT (hết giấy giữa
    /// chừng) ra Loi → server gửi lại → trang đầu ra hai lần. Nay là KhongRo.
    #[test]
    fn j_dang_in_kem_loi_ngay_lan_dau_thay_la_khong_ro_khong_phai_loi() {
        let kq = suy_ket_qua(&quan_sat_job(JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT));
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "got {:?}", kq);
    }

    #[test]
    fn suy_ket_qua_gan_ma_su_co() {
        let KetQuaIn::KhongRo(l) = suy_ket_qua(&[DangCho, LoiJob(MaSuCo::HetGiay)]) else { panic!() };
        assert_eq!(l.loai, Some(MaSuCo::HetGiay));
        let KetQuaIn::Loi(l) = suy_ket_qua(&[DangCho, MayInLoi(MaSuCo::HetGiay)]) else { panic!() };
        assert_eq!(l.loai, Some(MaSuCo::HetGiay));
        let KetQuaIn::KhongRo(l) = suy_ket_qua(&[DangIn, MayInLoi(MaSuCo::KetGiay)]) else { panic!() };
        assert_eq!(l.loai, Some(MaSuCo::KetGiay));
        let KetQuaIn::KhongRo(l) = suy_ket_qua(&[DangCho, DangCho]) else { panic!() };
        assert_eq!(l.loai, Some(MaSuCo::KhongXacNhan));
    }

    #[test]
    fn bo_sung_loai_chi_thay_khong_xac_nhan() {
        let het_gio = KetQuaIn::KhongRo(LyDo::co_loai("het gio", MaSuCo::KhongXacNhan));
        let KetQuaIn::KhongRo(l) = bo_sung_loai(het_gio.clone(), Some(MaSuCo::Offline)) else { panic!() };
        assert_eq!(l.loai, Some(MaSuCo::Offline));
        assert_eq!(bo_sung_loai(het_gio.clone(), None), het_gio);
        // mã cụ thể đã có thì giữ; Loi/DaIn không đụng
        let ket = KetQuaIn::KhongRo(LyDo::co_loai("x", MaSuCo::KetGiay));
        assert_eq!(bo_sung_loai(ket.clone(), Some(MaSuCo::Offline)), ket);
        assert_eq!(bo_sung_loai(KetQuaIn::DaIn, Some(MaSuCo::Offline)), KetQuaIn::DaIn);
    }

    // --- quyết khi có sự cố TRƯỚC khi in (§0.1) ---

    fn ung_vien_loi() -> LyDo {
        LyDo::co_loai("loi truoc khi in: Hết giấy", MaSuCo::HetGiay)
    }

    /// Máy in MẠNG: job rời hàng đợi Windows ngay khi đã sang bộ nhớ máy in —
    /// không thấy job ≠ chưa in. Trả Loi ở đây là backend gửi lại → IN ĐÔI.
    #[test]
    fn quyet_loi_job_khong_trong_hang_doi_thi_khong_ro_chong_in_doi() {
        let kq = quyet_loi_truoc_khi_in(ung_vien_loi(), || KetQuaGoJob::KhongCoTrongHangDoi);
        let KetQuaIn::KhongRo(l) = kq else { panic!("phai KhongRo (chong in doi), got {:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::HetGiay));
    }

    #[test]
    fn quyet_loi_xoa_duoc_thi_loi_giu_ma() {
        let KetQuaIn::Loi(l) = quyet_loi_truoc_khi_in(ung_vien_loi(), || KetQuaGoJob::DaGoXong) else { panic!() };
        assert_eq!(l.loai, Some(MaSuCo::HetGiay));
        assert!(l.chu.contains("da xoa job"));
    }

    #[test]
    fn quyet_loi_khong_xoa_duoc_thi_khong_ro_giu_ma() {
        let kq = quyet_loi_truoc_khi_in(ung_vien_loi(), || KetQuaGoJob::KhongGoDuoc("ACCESS_DENIED".into()));
        let KetQuaIn::KhongRo(l) = kq else { panic!("xoá không được mà báo Loi là IN ĐÔI: {:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::HetGiay));
        assert!(l.chu.contains("ACCESS_DENIED"));
        let kq = quyet_loi_truoc_khi_in(ung_vien_loi(), || KetQuaGoJob::KhongAnToanDeXoa("PRINTING".into()));
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "job không sạch thì không bao giờ Loi: {:?}", kq);
    }

    // --- go_job_khoi_hang_doi ---

    #[test]
    fn go_job_khong_co_job_cua_ta_thi_khong_dong_gi() {
        let mut sp = SpoolerGia { hang_doi: Some(vec![job_khac(9)]), ..Default::default() };
        assert_eq!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::KhongCoTrongHangDoi);
        assert!(sp.lenh.is_empty(), "không được đụng job của người khác");
    }

    #[test]
    fn go_job_sach_thi_tam_dung_roi_xoa_roi_kiem_lai_het() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, JOB_STATUS_SPOOLING), job_khac(9)]),
            het_sau_so_lan_doc: Some(2),
            ..Default::default()
        };
        assert_eq!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::DaGoXong);
        assert_eq!(sp.lenh, vec![(7, LenhJob::TamDung), (7, LenhJob::Xoa)],
            "PAUSED do chính ta dừng không được làm hỏng lần kiểm lại");
        assert_eq!(sp.so_lan_doc_sau_xoa, 3, "phải kiểm lại tới khi thấy hết");
    }

    #[test]
    fn go_job_hai_ban_cung_lan_in_deu_bi_xoa() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, JOB_STATUS_BLOCKED_DEVQ), job(8, 0)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        assert_eq!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::DaGoXong);
        assert_eq!(sp.lenh, vec![(7, LenhJob::TamDung), (8, LenhJob::TamDung), (7, LenhJob::Xoa), (8, LenhJob::Xoa)]);
    }

    /// R2 — pb3 và mọi cờ "byte có thể đã rời máy": KHÔNG tạm dừng, KHÔNG xoá.
    #[test]
    fn go_job_co_bat_ky_co_nao_ngoai_danh_sach_trang_thi_khong_dong_vao() {
        for status in [
            JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR, // pb3
            JOB_STATUS_ERROR,
            JOB_STATUS_PAPEROUT,
            JOB_STATUS_USER_INTERVENTION,
            JOB_STATUS_OFFLINE,
            JOB_STATUS_RESTART,
            JOB_STATUS_PRINTING,
            JOB_STATUS_PRINTED,
            JOB_STATUS_DELETING,
            JOB_STATUS_DELETED,
            JOB_STATUS_COMPLETE,
            JOB_STATUS_RETAINED,
            JOB_STATUS_SPOOLING | JOB_STATUS_ERROR,
            0x4000, // RENDERING_LOCALLY — cờ lạ cũng không an toàn
        ] {
            let mut sp = SpoolerGia { hang_doi: Some(vec![job(7, status)]), ..Default::default() };
            let kq = go_job_khoi_hang_doi(&mut sp, ID, false);
            assert!(matches!(kq, KetQuaGoJob::KhongAnToanDeXoa(_)), "status 0x{:X}: {:?}", status, kq);
            assert!(sp.lenh.is_empty(), "status 0x{:X}: không được tạm dừng/xoá", status);
        }
        // đã in một trang dù cờ sạch
        let mut sp = SpoolerGia { hang_doi: Some(vec![JobHangDoi { trang_da_in: 1, ..job(7, 0) }]), ..Default::default() };
        assert!(matches!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::KhongAnToanDeXoa(_)));
        assert!(sp.lenh.is_empty());
    }

    #[test]
    fn go_job_da_tung_thay_in_trong_luc_theo_doi_thi_khong_doc_khong_xoa() {
        let mut sp = SpoolerGia { hang_doi: Some(vec![job(7, 0)]), ..Default::default() };
        assert!(matches!(go_job_khoi_hang_doi(&mut sp, ID, true), KetQuaGoJob::KhongAnToanDeXoa(_)));
        assert!(sp.lenh.is_empty());
    }

    #[test]
    fn go_job_xoa_bi_tu_choi_thi_khong_go_duoc_va_cho_chay_tiep() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, 0)]),
            lenh_loi: Some(LenhJob::Xoa),
            ..Default::default()
        };
        assert!(matches!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::KhongGoDuoc(_)));
        assert_eq!(sp.lenh.last(), Some(&(7, LenhJob::TiepTuc)),
            "đã tạm dừng mà không xoá được thì phải cho chạy tiếp — KhongRo không được gửi lại, job dừng mãi là mất hoá đơn");
    }

    #[test]
    fn go_job_tam_dung_bi_tu_choi_thi_khong_xoa() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, 0)]),
            lenh_loi: Some(LenhJob::TamDung),
            ..Default::default()
        };
        assert!(matches!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::KhongGoDuoc(_)));
        assert!(!sp.lenh.iter().any(|(_, l)| *l == LenhJob::Xoa));
    }

    #[test]
    fn go_job_xoa_roi_van_con_thi_khong_go_duoc() {
        let mut sp = SpoolerGia { hang_doi: Some(vec![job(7, JOB_STATUS_SPOOLING)]), ..Default::default() };
        assert!(matches!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::KhongGoDuoc(_)));
        assert_eq!(sp.so_lan_doc_sau_xoa, SO_LAN_KIEM_SAU_XOA, "kiểm đủ số lần rồi mới bỏ cuộc");
    }

    #[test]
    fn go_job_bat_dau_in_dung_luc_tam_dung_thi_khong_xoa_va_cho_chay_tiep() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, 0)]),
            in_ngay_khi_tam_dung: true,
            ..Default::default()
        };
        assert!(matches!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::KhongAnToanDeXoa(_)));
        assert_eq!(sp.lenh, vec![(7, LenhJob::TamDung), (7, LenhJob::TiepTuc)]);
    }

    /// R11a: cho chạy tiếp lỗi thì phải NÓI RA (câu lỗi + file nhật ký), không nuốt.
    #[test]
    fn go_job_cho_chay_tiep_loi_thi_bao_ra_cau_loi() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, 0)]),
            in_ngay_khi_tam_dung: true,
            lenh_loi: Some(LenhJob::TiepTuc),
            ..Default::default()
        };
        let KetQuaGoJob::KhongAnToanDeXoa(e) = go_job_khoi_hang_doi(&mut sp, ID, false) else { panic!() };
        assert!(e.contains("KHONG cho chay tiep duoc") && e.contains("ERROR_ACCESS_DENIED"), "{}", e);
    }

    #[test]
    fn go_job_khong_doc_duoc_hang_doi_thi_khong_go_duoc() {
        let mut sp = SpoolerGia { hang_doi: None, ..Default::default() };
        assert!(matches!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::KhongGoDuoc(_)));
        assert!(sp.lenh.is_empty());
    }

    // --- theo_doi_job_voi (vòng poll trên spooler giả) ---

    /// Chạy vòng theo dõi tối đa `so_vong` vòng; trả (kết quả, các QuanSat đã báo).
    fn chay(sp: &mut SpoolerGia, so_vong: usize) -> (KetQuaIn, Vec<QuanSat>) {
        let da_bao = RefCell::new(Vec::new());
        let su_kien = sp.su_kien.clone();
        let bao = |q: QuanSat| {
            if let QuanSat::SuCo { loai, .. } = &q {
                su_kien.borrow_mut().push(format!("su_co:{}", loai.ma()));
            }
            da_bao.borrow_mut().push(q);
        };
        // Các test này coi vòng đọc ĐẦU là ảnh chụp trước Sumatra (T3: nền
        // do bước kiểm trước khi in truyền xuống).
        let nen = chup_nen(&sp.vong[0]);
        chay_voi_nen(sp, so_vong, nen, &bao)
            .map(|kq| (kq, da_bao.into_inner()))
            .unwrap()
    }

    /// Như `chay` nhưng ảnh chụp trước Sumatra SẠCH — sự cố thấy trong vòng
    /// theo dõi là sự cố MỚI (xuất hiện sau khi gửi lệnh in).
    fn chay_su_co_moi(sp: &mut SpoolerGia, so_vong: usize) -> (KetQuaIn, Vec<QuanSat>) {
        let da_bao = RefCell::new(Vec::new());
        let bao = |q: QuanSat| da_bao.borrow_mut().push(q);
        let kq = chay_voi_nen(sp, so_vong, TapMa::default(), &bao).unwrap();
        (kq, da_bao.into_inner())
    }

    fn chay_voi_nen(sp: &mut SpoolerGia, so_vong: usize, nen: TapMa, bao: &dyn Fn(QuanSat)) -> Option<KetQuaIn> {
        let mut con = so_vong;
        Some(theo_doi_job_voi(sp, ID, 1_000, nen, bao, &mut || {
            con -= 1;
            con > 0
        }))
    }

    fn so_su_co(ds: &[QuanSat], ma: MaSuCo) -> usize {
        ds.iter().filter(|q| matches!(q, QuanSat::SuCo { loai, .. } if *loai == ma)).count()
    }

    fn co_theo_doi_tiep(ds: &[QuanSat]) -> bool {
        ds.iter().any(|q| matches!(q, QuanSat::ConTrongHangDoi(_)))
    }

    /// pb3 (giám sát 25/09): PAPEROUT|ERROR trên job, chưa PRINTING. Bản trước
    /// xoá job rồi báo Loi — nhưng cờ đó port monitor bật TRONG LÚC GỬI, máy in
    /// mạng có thể đã đệm một phần → gửi lại = in đôi. Nay: KhongRo, không một
    /// lệnh Xoa nào, job nằm lại và được theo dõi tiếp.
    #[test]
    fn t_pb3_job_paperout_error_chua_printing_thi_khong_ro_khong_xoa() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR)])],
            hang_doi: Some(vec![job(7, JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        assert!(!sp.lenh.iter().any(|(_, l)| *l == LenhJob::Xoa), "KHÔNG được xoá: {:?}", sp.lenh);
        assert!(sp.lenh.is_empty(), "cũng không tạm dừng: {:?}", sp.lenh);
        assert_eq!(so_su_co(&bao, MaSuCo::HetGiay), 1);
        assert_eq!(sp.su_kien.borrow().first().map(String::as_str), Some("su_co:het_giay"), "su-co báo NGAY");
        assert!(co_theo_doi_tiep(&bao), "job còn trong hàng đợi → phải theo dõi tiếp");
    }

    /// "Kể cả job OFFLINE" — cờ OFFLINE trên job cũng không được xoá.
    #[test]
    fn t_job_offline_cap_job_thi_khong_ro_khong_xoa() {
        let mut sp = SpoolerGia {
            vong: vec![vong(PRINTER_STATUS_OFFLINE, vec![job(7, JOB_STATUS_OFFLINE)])],
            hang_doi: Some(vec![job(7, JOB_STATUS_OFFLINE)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, _) = chay(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::Offline)), "{:?}", kq);
        assert!(sp.lenh.is_empty());
    }

    /// R2 — đường duy nhất ra Loi: job SẠCH suốt cửa sổ theo dõi + máy in (cấp
    /// máy) báo sự cố chặn in MỚI (không có trong ảnh chụp trước Sumatra) →
    /// tạm dừng → đọc lại → xoá → kiểm hết → Loi.
    #[test]
    fn t_may_in_het_giay_job_sach_het_cua_so_thi_xoa_roi_loi() {
        let mut sp = SpoolerGia {
            vong: vec![vong(PRINTER_STATUS_PAPER_OUT, vec![job(7, 0)])],
            hang_doi: Some(vec![job(7, 0)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        let KetQuaIn::Loi(l) = kq else { panic!("{:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::HetGiay));
        assert_eq!(sp.lenh, vec![(7, LenhJob::TamDung), (7, LenhJob::Xoa)]);
        assert_eq!(sp.so_vong_da_doc, 30, "ứng viên xoá chờ hết cửa sổ — cờ cấp máy có thể nhiễu");
        assert!(so_su_co(&bao, MaSuCo::HetGiay) >= 1, "sự cố MỚI → su-co trong lúc in job này");
        assert!(!co_theo_doi_tiep(&bao), "đã xoá thì không còn gì để theo dõi");
    }

    /// Kiểm cuối 25/09: sự cố CHỈ LÀ NỀN (có trước Sumatra) + job sạch chờ hết
    /// cửa sổ → KHÔNG xoá. `khong_ro(<mã nền>)`, job nằm lại + theo dõi tiếp.
    /// Ca thật: máy WSD ngủ + ERROR nền dai dẳng, job cần >15 s mới bắt đầu in —
    /// bản trước xoá → `can_xu_ly` → gửi thử lại bị xoá trước khi máy kịp thức →
    /// lặp vô hạn, cả máy bị giữ.
    #[test]
    fn kiem_cuoi_co_nen_khong_bao_gio_xoa_job_sach() {
        for co in [PRINTER_STATUS_PAPER_OUT, PRINTER_STATUS_ERROR] {
            let mut sp = SpoolerGia {
                vong: vec![vong(co, vec![job(7, JOB_STATUS_SPOOLING)])],
                hang_doi: Some(vec![job(7, JOB_STATUS_SPOOLING)]),
                het_sau_so_lan_doc: Some(0),
                ..Default::default()
            };
            let (kq, bao) = chay(&mut sp, 30);
            let KetQuaIn::KhongRo(l) = kq else { panic!("{:?}", kq) };
            assert!(l.loai.is_some_and(|m| m.chan_in()), "mang mã nền để backend ngắt cầu dao: {:?}", l);
            assert!(sp.lenh.is_empty(), "không tạm dừng/xoá: {:?}", sp.lenh);
            assert!(co_theo_doi_tiep(&bao), "job nằm lại → theo dõi tiếp, máy thức in ra thì da_in trễ");
            assert_eq!(so_su_co(&bao, l.loai.unwrap()), 0, "nền không gửi su-co 'trong lúc in job này'");
        }
    }

    /// Cờ cấp máy báo hết giấy nhưng máy vẫn in được (vd khay khác hết giấy):
    /// job chuyển PRINTING trong cửa sổ → không còn sạch → không bao giờ xoá.
    #[test]
    fn t_may_in_bao_het_giay_nhung_job_van_in_thi_khong_xoa() {
        let mut sp = SpoolerGia {
            vong: vec![
                vong(PRINTER_STATUS_PAPER_OUT, vec![job(7, 0)]),
                vong(PRINTER_STATUS_PAPER_OUT, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(PRINTER_STATUS_PAPER_OUT, vec![job(7, JOB_STATUS_PRINTING)]),
            ],
            hang_doi: Some(vec![job(7, JOB_STATUS_PRINTING)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 10);
        // R-B: hết giấy có từ trước job (nền) mà job vẫn PRINTING — cờ báo sai;
        // không gắn nó vào kết quả (gắn vào là backend ngắt cầu dao vì cờ sai).
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::KhongXacNhan)), "{:?}", kq);
        assert!(sp.lenh.is_empty(), "{:?}", sp.lenh);
        assert!(co_theo_doi_tiep(&bao));
    }

    #[test]
    fn t_may_in_het_giay_job_sach_nhung_xoa_bi_tu_choi_thi_khong_ro_va_theo_doi_tiep() {
        let mut sp = SpoolerGia {
            vong: vec![vong(PRINTER_STATUS_PAPER_OUT, vec![job(7, 0)])],
            hang_doi: Some(vec![job(7, 0)]),
            lenh_loi: Some(LenhJob::Xoa),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 5);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)),
            "job còn trong hàng đợi mà báo Loi là in đôi khi NV nạp giấy: {:?}", kq);
        assert!(co_theo_doi_tiep(&bao));
    }

    /// R10 + R2: "Use Printer Offline" — spooler giữ job; job sạch hết cửa sổ → xoá → Loi(offline).
    #[test]
    fn t_may_in_dung_offline_job_sach_thi_xoa_roi_loi_offline() {
        let mut sp = SpoolerGia {
            vong: vec![VongDoc { thuoc_tinh_may_in: PRINTER_ATTRIBUTE_WORK_OFFLINE, ..vong(0, vec![job(7, 0)]) }],
            hang_doi: Some(vec![job(7, 0)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, _) = chay_su_co_moi(&mut sp, 4);
        assert!(matches!(kq, KetQuaIn::Loi(ref l) if l.loai == Some(MaSuCo::Offline)), "{:?}", kq);
    }

    /// BLOCKED_DEVQ trên job sạch → xoá → Loi, mã đọc từ câu driver (R9).
    #[test]
    fn t_job_blocked_devq_sach_thi_xoa_va_ma_theo_chu_driver() {
        let j = JobHangDoi { mo_ta_driver: "Paper out".into(), ..job(7, JOB_STATUS_BLOCKED_DEVQ) };
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![j.clone()])],
            hang_doi: Some(vec![j]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 4);
        assert!(matches!(kq, KetQuaIn::Loi(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        assert_eq!(so_su_co(&bao, MaSuCo::HetGiay), 4, "su-co mang mã đã đọc lại theo câu driver");
    }

    /// R9: HP qua WSD chỉ bật ERROR, câu "Paper out" nằm ở pStatus.
    #[test]
    fn t_job_chi_co_error_ma_driver_noi_het_giay_thi_bao_het_giay() {
        let j = JobHangDoi { mo_ta_driver: "Error - Paper Out".into(), ..job(7, JOB_STATUS_ERROR) };
        let mut sp = SpoolerGia { vong: vec![vong(0, vec![j.clone()])], hang_doi: Some(vec![j]), ..Default::default() };
        let (kq, bao) = chay(&mut sp, 3);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        assert_eq!(so_su_co(&bao, MaSuCo::HetGiay), 1);
        assert_eq!(so_su_co(&bao, MaSuCo::LoiMayIn), 0);
    }

    /// Đổi 24/09 (trước: Loi "như cũ"): máy in mạng giữ job trong BỘ NHỚ MÁY
    /// sau khi rời hàng đợi Windows — Loi ở đây là backend gửi lại → in đôi.
    #[test]
    fn t_may_in_het_giay_job_khong_trong_hang_doi_thi_khong_ro_khong_gui_lai() {
        let mut sp = SpoolerGia {
            vong: vec![vong(PRINTER_STATUS_PAPER_OUT, vec![job_khac(9)])],
            hang_doi: Some(vec![job_khac(9)]),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        // R-B: hết giấy là NỀN (có từ trước job) → không gắn vào kết quả; vẫn KhongRo.
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::KhongXacNhan)), "{:?}", kq);
        assert!(sp.lenh.is_empty());
        assert!(!co_theo_doi_tiep(&bao), "job không trong hàng đợi → không có gì để theo dõi tiếp");
        // Hết giấy MỚI xuất hiện sau lần đọc đầu → tính: KhongRo(het_giay), không bao giờ Loi.
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job_khac(9)]), vong(PRINTER_STATUS_PAPER_OUT, vec![job_khac(9)])],
            hang_doi: Some(vec![job_khac(9)]),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        assert!(sp.lenh.is_empty());
        assert!(so_su_co(&bao, MaSuCo::HetGiay) >= 1, "sự cố MỚI thì báo su-co");
    }

    #[test]
    fn t_dang_in_ma_het_giay_khong_bao_gio_ra_loi_va_bao_su_co_truoc_khi_ket_luan() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT)])],
            hang_doi: Some(vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT)]),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 3);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        assert!(sp.lenh.is_empty(), "đang in thì không bao giờ xoá");
        assert_eq!(so_su_co(&bao, MaSuCo::HetGiay), 1);
        let pos_su_co = bao.iter().position(|q| matches!(q, QuanSat::SuCo { .. })).unwrap();
        let pos_tiep = bao.iter().position(|q| matches!(q, QuanSat::ConTrongHangDoi(_))).unwrap();
        assert!(pos_su_co < pos_tiep);
        assert_eq!(bao[pos_tiep], QuanSat::ConTrongHangDoi(BangChungJob { da_thay_in: true, ..Default::default() }),
            "bằng chứng đã in chuyển sang theo dõi tiếp");
    }

    #[test]
    fn t_dang_in_roi_roi_hang_doi_sach_van_la_da_in() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong(0, vec![]), vong(0, vec![])],
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        assert_eq!(kq, KetQuaIn::DaIn);
        assert_eq!(bao.iter().filter(|q| matches!(q, QuanSat::SuCo { .. })).count(), 0);
        assert_eq!(sp.so_vong_da_doc, 3 + SO_LAN_DOC_MAY_IN_SAU_KHI_ROI,
            "kết luận ngay khi đủ vắng (không chờ hết 15 s) + đọc máy in thêm ~2 s");
    }

    /// R5c: máy in mạng nhận trọn job rồi mới báo hết giấy — job rời hàng đợi
    /// sạch nhưng giấy chưa ra.
    #[test]
    fn t_roi_hang_doi_sach_nhung_may_in_het_giay_ngay_sau_thi_khong_ro() {
        let mut sp = SpoolerGia {
            vong: vec![
                vong(0, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(0, vec![]),
                vong(0, vec![]),
                vong(0, vec![]),
                vong(PRINTER_STATUS_PAPER_OUT, vec![]),
            ],
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        assert_eq!(so_su_co(&bao, MaSuCo::HetGiay), 1);
        assert!(!co_theo_doi_tiep(&bao), "job đã rời hàng đợi Windows");
    }

    /// R5a trên vòng poll: NV huỷ job (DELETING) rồi nó biến mất → không DaIn.
    #[test]
    fn t_nv_huy_job_roi_bien_mat_khong_phai_da_in() {
        let mut sp = SpoolerGia {
            vong: vec![
                vong(0, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(0, vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_DELETING)]),
                vong(0, vec![]),
            ],
            ..Default::default()
        };
        let (kq, _) = chay(&mut sp, 30);
        let KetQuaIn::KhongRo(l) = kq else { panic!("{:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::KhongXacNhan));
        assert!(l.chu.contains("huỷ/khởi động lại"));
    }

    /// R5b trên vòng poll: EnumJobs lỗi (hang_doi None) sau khi đang in không
    /// bao giờ thành "rời hàng đợi sạch".
    #[test]
    fn t_enumjobs_loi_sau_khi_dang_in_khong_thanh_da_in() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), VongDoc { co_may_in: Some(0), ..VongDoc::default() }],
            ..Default::default()
        };
        let (kq, _) = chay(&mut sp, 10);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "{:?}", kq);
    }

    #[test]
    fn t_cho_het_gio_ma_khong_co_su_co_thi_khong_ro_va_theo_doi_tiep() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, 0)])],
            hang_doi: Some(vec![job(7, 0)]),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 4);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::KhongXacNhan)), "{:?}", kq);
        assert!(sp.lenh.is_empty());
        assert!(co_theo_doi_tiep(&bao));
    }

    #[test]
    fn t_muc_yeu_khong_chan_in_khong_bao_su_co() {
        let mut sp = SpoolerGia {
            vong: vec![
                vong(PRINTER_STATUS_TONER_LOW, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(PRINTER_STATUS_TONER_LOW, vec![]),
                vong(PRINTER_STATUS_TONER_LOW, vec![]),
            ],
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 5);
        assert_eq!(kq, KetQuaIn::DaIn);
        assert!(bao.iter().all(|q| !matches!(q, QuanSat::SuCo { .. })), "mực yếu không phải su-co: {:?}", bao);
        assert!(bao.iter().any(|q| matches!(q, QuanSat::MayIn { ma: MaSuCo::HetMuc, .. })),
            "nhưng vẫn báo trạng thái máy in để gửi trang-thai-may-in");
    }

    #[test]
    fn t_khong_tim_thay_may_in_la_khong_ro_khong_phai_loi() {
        // OpenPrinter lỗi → không đọc được hàng đợi → không CHẮC job không có ở
        // đâu (máy in chia sẻ qua mạng mất kết nối cũng ra lỗi này) → KhongRo.
        let mut sp = SpoolerGia {
            vong: vec![VongDoc { khong_tim_thay_may_in: true, ..VongDoc::default() }],
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 3);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::KhongTimThayMayIn)), "{:?}", kq);
        assert!(sp.lenh.is_empty());
        assert_eq!(so_su_co(&bao, MaSuCo::KhongTimThayMayIn), 3);
    }

    /// jobId backend mới không còn token (`<ms>-<n>`) — chỉ là chuỗi con của tên file.
    #[test]
    fn t_job_id_khong_token_van_tim_duoc() {
        let id = "1790251200000-7";
        let j = JobHangDoi { document: format!("AI-INV_2026_030045-Anh_Loc-{}.pdf", id), ..job(7, JOB_STATUS_PRINTING) };
        assert!(la_cua_job(&j, id));
        assert_eq!(tim_job(std::slice::from_ref(&j), id, 1_000).map(|j| j.id), Some(7));
        // job …-71 không phải của job …-7
        let j71 = JobHangDoi { document: "AI-INV_1-Khach-1790251200000-71.pdf".into(), ..job(8, 0) };
        assert!(!la_cua_job(&j71, id));
        assert!(la_cua_job(&j71, "1790251200000-71"));
        // tên dự phòng print-agent-<id>-<hex>.pdf
        let jdp = JobHangDoi { document: format!("print-agent-{}-18a2f.pdf", id), ..job(9, 0) };
        assert!(la_cua_job(&jdp, id));
    }

    // --- R-B: cờ cấp máy có TỪ TRƯỚC job là nền ---

    /// Kịch bản S5 của giám sát vòng 2 (HP 4003 qua WSD ở HN): ERROR cấp máy
    /// bật SUỐT mà máy vẫn in — job Spooling → Printing → rời hàng đợi sạch
    /// PHẢI ra `DaIn` (bản trước: KhongRo, cầu dao backend ngắt mãi).
    #[test]
    fn r_b_s5_error_cap_may_suot_ma_job_in_xong_la_da_in() {
        let e = PRINTER_STATUS_ERROR;
        let mut sp = SpoolerGia {
            vong: vec![
                vong(e, vec![job(7, JOB_STATUS_SPOOLING)]),
                vong(e, vec![job(7, 0)]),
                vong(e, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(e, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(e, vec![]),
                vong(e, vec![]),
                vong(e, vec![]),
            ],
            hang_doi: Some(vec![]),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        assert_eq!(kq, KetQuaIn::DaIn);
        assert!(sp.lenh.is_empty());
        assert_eq!(so_su_co(&bao, MaSuCo::LoiMayIn), 0, "cờ nền không phải sự cố của job này");
        assert!(!co_theo_doi_tiep(&bao));
        // job ra giữa chừng: trạng thái 0 SAU khi đã PRINTING (giữa hai trang) +
        // cờ nền — không được thành "lỗi sau khi bắt đầu in".
        let mut sp = SpoolerGia {
            vong: vec![
                vong(e, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(e, vec![job(7, 0)]),
                vong(e, vec![]),
                vong(e, vec![]),
            ],
            ..Default::default()
        };
        assert_eq!(chay(&mut sp, 30).0, KetQuaIn::DaIn);
    }

    /// PHẢN CHỨNG R-B: ERROR xuất hiện SAU khi job bắt đầu (không có trong ảnh
    /// chụp đầu) rồi job rời hàng đợi → vẫn KhongRo (có thể trong bộ nhớ máy in).
    #[test]
    fn r_b_error_moi_xuat_hien_sau_khi_job_bat_dau_van_khong_ro() {
        let e = PRINTER_STATUS_ERROR;
        // rời hàng đợi lúc ERROR mới đang bật
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong(e, vec![]), vong(e, vec![]), vong(e, vec![])],
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::LoiMayIn)), "{:?}", kq);
        assert!(so_su_co(&bao, MaSuCo::LoiMayIn) >= 1, "sự cố MỚI thì báo su-co");
        // rời hàng đợi sạch, ERROR mới bật trong ~2 s đọc thêm (R5c)
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong(0, vec![]), vong(0, vec![]), vong(e, vec![])],
            ..Default::default()
        };
        assert!(matches!(chay(&mut sp, 30).0, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::LoiMayIn)));
        // nền ERROR, rồi HẾT GIẤY mới trong lúc đọc thêm → vẫn KhongRo(het_giay)
        let mut sp = SpoolerGia {
            vong: vec![
                vong(e, vec![job(7, JOB_STATUS_PRINTING)]),
                vong(e, vec![]),
                vong(e, vec![]),
                vong(e | PRINTER_STATUS_PAPER_OUT, vec![]),
            ],
            ..Default::default()
        };
        assert!(matches!(chay(&mut sp, 30).0, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)));
    }

    /// T4: ERROR chung chung CẤP MÁY MỚI trên job sạch → xoá → mã `can_xu_ly`
    /// (không tiêu lượt thử). ERROR chỉ là NỀN → không xoá (xem
    /// `kiem_cuoi_co_nen_khong_bao_gio_xoa_job_sach`).
    #[test]
    fn r_b_error_moi_tren_job_sach_thi_xoa_bao_can_xu_ly() {
        let mut sp = SpoolerGia {
            vong: vec![vong(PRINTER_STATUS_ERROR, vec![job(7, JOB_STATUS_SPOOLING)])],
            hang_doi: Some(vec![job(7, JOB_STATUS_SPOOLING)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, _) = chay_su_co_moi(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::Loi(ref l) if l.loai == Some(MaSuCo::CanXuLy)), "{:?}", kq);
        assert_eq!(sp.lenh, vec![(7, LenhJob::TamDung), (7, LenhJob::Xoa)]);
    }

    /// T4 (giám sát vòng 3): `loi_may_in` CHỈ còn cho lỗi riêng một job
    /// (BLOCKED_DEVQ); ERROR chung chung của MÁY trên job sạch → `can_xu_ly`
    /// (backend không tiêu lượt, hoá đơn chờ máy hết lỗi). Mã cụ thể giữ nguyên.
    #[test]
    fn t4_loi_chung_chung_cap_may_tren_job_sach_la_can_xu_ly() {
        let loi_cua = |v: VongDoc, j: JobHangDoi| {
            let mut sp = SpoolerGia { vong: vec![v], hang_doi: Some(vec![j]), het_sau_so_lan_doc: Some(0), ..Default::default() };
            match chay_su_co_moi(&mut sp, 30).0 {
                KetQuaIn::Loi(l) => l.loai,
                kq => panic!("{:?}", kq),
            }
        };
        // cấp máy ERROR mới xuất hiện (không phải nền) cũng vậy
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, 0)]), vong(PRINTER_STATUS_ERROR, vec![job(7, 0)])],
            hang_doi: Some(vec![job(7, 0)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        let (kq, _) = chay(&mut sp, 30);
        let KetQuaIn::Loi(l) = kq else { panic!("{:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::CanXuLy));
        assert!(l.chu.contains("can_xu_ly"), "{}", l.chu);
        // BLOCKED_DEVQ (lỗi riêng job) giữ loi_may_in — job hỏng thật không lặp mãi
        assert_eq!(loi_cua(vong(0, vec![job(7, JOB_STATUS_BLOCKED_DEVQ)]), job(7, JOB_STATUS_BLOCKED_DEVQ)), Some(MaSuCo::LoiMayIn));
        // mã cụ thể cấp máy giữ nguyên
        assert_eq!(loi_cua(vong(PRINTER_STATUS_PAPER_OUT, vec![job(7, 0)]), job(7, 0)), Some(MaSuCo::HetGiay));
        // KhongRo không bị đổi mã (không tiêu lượt gì cả)
        assert_eq!(ma_loi_khong_tieu_luot(KetQuaIn::KhongRo(LyDo::co_loai("x", MaSuCo::LoiMayIn)), true),
            KetQuaIn::KhongRo(LyDo::co_loai("x", MaSuCo::LoiMayIn)));
    }

    /// T3 (giám sát vòng 3): sự cố bắt đầu TRONG LÚC Sumatra chạy — ảnh chụp
    /// trước Sumatra sạch, lần đọc đầu của vòng theo dõi đã thấy hết giấy — là
    /// sự cố MỚI: job rời hàng đợi lúc máy báo hết giấy → `KhongRo`, không `da_in`.
    #[test]
    fn t3_su_co_bat_dau_luc_sumatra_chay_khong_phai_nen() {
        let po = PRINTER_STATUS_PAPER_OUT;
        let vongs = || vec![vong(po, vec![job(7, JOB_STATUS_PRINTING)]), vong(po, vec![]), vong(po, vec![]), vong(po, vec![])];
        let bao = |_: QuanSat| {};
        // nền chụp TRƯỚC Sumatra: sạch
        let mut sp = SpoolerGia { vong: vongs(), ..Default::default() };
        let kq = chay_voi_nen(&mut sp, 30, TapMa::default(), &bao).unwrap();
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        // (bản trước: nền = lần đọc đầu SAU Sumatra — đã có hết giấy — → DaIn sai)
        let mut sp = SpoolerGia { vong: vongs(), ..Default::default() };
        let nen_sai = chup_nen(&sp.vong[0]);
        assert_eq!(chay_voi_nen(&mut sp, 30, nen_sai, &bao).unwrap(), KetQuaIn::DaIn, "đối chứng: đây là lỗi cũ");
    }

    /// T2: job kẹt mà NV đã tạm dừng / đã in / giữ lại sau khi in KHÔNG chặn
    /// hàng đợi — không từ chối in mãi vì nó.
    #[test]
    fn t2_job_paused_printed_complete_retained_khong_phai_job_ket() {
        for them in [JOB_STATUS_PAUSED, JOB_STATUS_PRINTED, JOB_STATUS_COMPLETE, JOB_STATUS_RETAINED, JOB_STATUS_DELETING, JOB_STATUS_DELETED] {
            let j = JobHangDoi { status: JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR | them, ..job_khac(9) };
            assert_eq!(ma_job_ket(&j), None, "0x{:X}", them);
            assert_eq!(kiem_hang_doi_truoc_khi_in(Some(std::slice::from_ref(&j)), None), None, "0x{:X}", them);
            assert_eq!(tinh_trang_gop(&vong(0, vec![j])).map(|x| x.0), Some(MaSuCo::BinhThuong), "0x{:X}", them);
        }
        // còn kẹt thật (chưa dừng) thì vẫn từ chối
        let j = JobHangDoi { status: JOB_STATUS_PAPEROUT, ..job_khac(9) };
        assert_eq!(ma_job_ket(&j), Some(MaSuCo::HetGiay));
    }

    /// T5 + T2 + T3 (thuần): bước kiểm trước khi in.
    #[test]
    fn t5_kiem_truoc_khi_in_may_in_khong_ton_tai_va_backend_cu() {
        let khong_co = VongDoc { khong_tim_thay_may_in: true, ..VongDoc::default() };
        let KiemTruoc::TuChoi { ly_do, su_kien } = kiem_truoc_khi_in(&khong_co, "HP 4003", None, true) else { panic!() };
        assert_eq!(ly_do.loai, Some(MaSuCo::KhongTimThayMayIn));
        assert_eq!(ly_do.chu, "Không tìm thấy máy in \"HP 4003\" trong Windows (đã đổi tên/gỡ?) — chưa gửi hoá đơn này xuống máy in");
        assert_eq!(su_kien, "khong_in_khong_tim_thay_may_in");
        assert!(MaSuCo::KhongTimThayMayIn.khong_tieu_luot(), "backend giữ hoá đơn chờ, không tiêu lượt");
        // backend cũ: không từ chối (T2); nền không bao giờ chứa khong_tim_thay
        assert_eq!(kiem_truoc_khi_in(&khong_co, "HP", None, false), KiemTruoc::In { nen: Some(TapMa::default()) });
        // hàng đợi kẹt → từ chối (backend mới), dựa vào theo dõi tiếp khi không đọc được
        let ket = vong(0, vec![job(7, JOB_STATUS_PAPEROUT)]);
        assert!(matches!(kiem_truoc_khi_in(&ket, "HP", None, true), KiemTruoc::TuChoi { su_kien: "khong_in_hang_doi_ket", .. }));
        let khong_doc = VongDoc { co_may_in: Some(0), ..VongDoc::default() };
        assert!(matches!(kiem_truoc_khi_in(&khong_doc, "HP", Some(("INV_1".into(), MaSuCo::KetGiay)), true), KiemTruoc::TuChoi { .. }));
        assert_eq!(kiem_truoc_khi_in(&khong_doc, "HP", None, true), KiemTruoc::In { nen: Some(TapMa::default()) });
        // được in: nền = cờ cấp máy của CHÍNH vòng đọc này
        let KiemTruoc::In { nen: Some(nen) } = kiem_truoc_khi_in(&vong(PRINTER_STATUS_ERROR, vec![]), "HP", None, true) else { panic!() };
        assert!(nen.co(MaSuCo::LoiMayIn));
    }

    /// Kịch bản S2 phần J2 (J2 tới TRƯỚC khi J1 kẹt, nên qua được bước kiểm
    /// trước khi in): J2 sạch nằm sau J1 kẹt suốt cửa sổ → KhongRo mang mã
    /// của J1 (không phải "không xác nhận được"), không xoá, theo dõi tiếp.
    #[test]
    fn r_a_job_ta_xep_sau_job_ket_thi_khong_ro_mang_ma_job_ket() {
        let j1 = JobHangDoi { document: "AI-INV_1-K-1790000000000-1.pdf".into(), ..job(6, JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR) };
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![j1.clone(), job(7, 0)])],
            hang_doi: Some(vec![j1, job(7, 0)]),
            ..Default::default()
        };
        let (kq, bao) = chay(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)), "{:?}", kq);
        assert!(sp.lenh.is_empty(), "không xoá job của ta, không đụng job kia");
        assert!(co_theo_doi_tiep(&bao));
        assert_eq!(so_su_co(&bao, MaSuCo::HetGiay), 0, "su-co không gửi thay cho job khác");
        assert!(bao.iter().any(|q| matches!(q, QuanSat::MayIn { ma: MaSuCo::HetGiay, .. })), "trạng thái gộp báo hết giấy");
    }

    // --- R-J: câu loiCuoi lúc hết giờ nói đúng chuyện đã xảy ra ---
    #[test]
    fn r_j_cau_het_gio_theo_dung_quan_sat() {
        let chu = |vongs: Vec<VongDoc>| {
            let mut sp = SpoolerGia { vong: vongs, ..Default::default() };
            match chay(&mut sp, 5).0 {
                KetQuaIn::KhongRo(l) => l.chu,
                kq => panic!("{:?}", kq),
            }
        };
        assert!(chu(vec![vong(0, vec![job(7, 0)])]).starts_with("job van nam trong hang doi Windows, chua bat dau in"));
        assert!(chu(vec![vong(0, vec![job(7, 0)]), vong(0, vec![])]).starts_with("job roi hang doi Windows ma chua thay bat dau in"));
        assert!(chu(vec![vong(0, vec![])]).starts_with("khong thay job trong hang doi Windows"));
    }

    // --- R-K: kiểm sau lệnh xoá thấy job đã bắt đầu in → không Loi ---
    #[test]
    fn r_k_sau_lenh_xoa_thay_job_dang_in_thi_khong_go_duoc() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, JOB_STATUS_SPOOLING)]),
            sau_xoa_thay_in: true,
            ..Default::default()
        };
        let kq = go_job_khoi_hang_doi(&mut sp, ID, false);
        assert!(matches!(kq, KetQuaGoJob::KhongGoDuoc(ref e) if e.contains("bat dau in trong luc xoa")), "{:?}", kq);
        assert!(kq.job_con_trong_hang_doi(), "job còn đó → theo dõi tiếp");
        let kq = quyet_loi_truoc_khi_in(LyDo::co_loai("loi truoc khi in", MaSuCo::HetGiay), || kq);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "đã có byte có thể tới máy in → không bao giờ Loi: {:?}", kq);
    }

    #[test]
    fn r_k_cho_500ms_sau_tam_dung_roi_moi_doc_lai() {
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![job(7, JOB_STATUS_SPOOLING)]),
            het_sau_so_lan_doc: Some(0),
            ..Default::default()
        };
        assert_eq!(go_job_khoi_hang_doi(&mut sp, ID, false), KetQuaGoJob::DaGoXong);
        let sk = sp.su_kien.borrow().clone();
        let cho = sk.iter().position(|e| e == "cho 500ms").expect("phải chờ 500 ms sau tạm dừng");
        assert_eq!(sk[..cho].iter().filter(|e| *e == "doc_hang_doi").count(), 1, "chỉ lần đọc TRƯỚC khi dừng: {:?}", sk);
        assert_eq!(sk.get(cho + 1).map(String::as_str), Some("doc_hang_doi"), "đọc lại ngay SAU khi chờ: {:?}", sk);
        assert!(CHO_SAU_TAM_DUNG >= Duration::from_millis(500));
    }

    // --- R-A / R-J: không dồn hoá đơn sau job đang kẹt ---

    /// Kịch bản S2 của giám sát vòng 2: máy mạng, J1 kẹt (PAPEROUT|ERROR trên
    /// JOB, cấp máy bình thường). J2 tới → KHÔNG in, `loi` mã của J1.
    #[test]
    fn r_a_s2_job_moi_xep_sau_job_ket_thi_khong_in() {
        let j1 = job(7, JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR);
        let ly_do = kiem_hang_doi_truoc_khi_in(Some(std::slice::from_ref(&j1)), None).expect("phải từ chối");
        assert_eq!(ly_do.loai, Some(MaSuCo::HetGiay));
        assert_eq!(ly_do.chu, "Máy in đang kẹt hoá đơn INV_2026_030045 — chưa gửi hoá đơn này xuống máy in");
        // R-F: chữ driver nói kẹt → mã kẹt giấy
        let jam = JobHangDoi { mo_ta_driver: "Paper jam".into(), ..j1.clone() };
        assert_eq!(kiem_hang_doi_truoc_khi_in(Some(&[jam]), None).unwrap().loai, Some(MaSuCo::KetGiay));
        // R-J: job của chương trình khác kẹt — không lộ tên tài liệu
        let word = JobHangDoi { status: JOB_STATUS_OFFLINE, ..job_khac(9) };
        let ly_do = kiem_hang_doi_truoc_khi_in(Some(&[job(8, 0), word]), None).unwrap();
        assert_eq!((ly_do.chu.as_str(), ly_do.loai), (CHU_KET_JOB_KHAC, Some(MaSuCo::Offline)));
        assert!(!ly_do.chu.contains("bao gia"));
        // nhiều job kẹt → mã ưu tiên cao nhất §1
        let ds = [JobHangDoi { status: JOB_STATUS_USER_INTERVENTION, ..job_khac(9) }, JobHangDoi { status: JOB_STATUS_PAPEROUT, ..job_khac(10) }];
        assert_eq!(kiem_hang_doi_truoc_khi_in(Some(&ds), None).unwrap().loai, Some(MaSuCo::HetGiay));
    }

    #[test]
    fn r_a_job_ket_chi_co_error_chung_chung_thi_tu_choi_bang_can_xu_ly_khong_tieu_luot() {
        // Backend tiêu lượt thử với `loi_may_in`; hoá đơn bị từ chối vì đứng sau
        // job kẹt không có lỗi gì → phải là mã KHÔNG tiêu lượt (`can_xu_ly`).
        let ket = job(7, JOB_STATUS_ERROR);
        let ly_do = kiem_hang_doi_truoc_khi_in(Some(std::slice::from_ref(&ket)), None).expect("phải từ chối");
        assert_eq!(ly_do.loai, Some(MaSuCo::CanXuLy));
        let tu_theo_doi = kiem_hang_doi_truoc_khi_in(None, Some(("INV_1".into(), MaSuCo::LoiMayIn))).unwrap();
        assert_eq!(tu_theo_doi.loai, Some(MaSuCo::CanXuLy));
    }

    #[test]
    fn r_a_ngoai_le_khong_chan() {
        // hàng đợi sạch / chỉ BLOCKED_DEVQ (lỗi riêng một job) / job đang bị xoá / đang in bình thường
        for ds in [
            vec![],
            vec![job(7, JOB_STATUS_BLOCKED_DEVQ)],
            vec![job(7, JOB_STATUS_ERROR | JOB_STATUS_DELETING)],
            vec![JobHangDoi { status: JOB_STATUS_PAPEROUT | JOB_STATUS_DELETED, ..job_khac(9) }],
            vec![job(7, JOB_STATUS_PRINTING), job_khac(9)],
            vec![job(7, JOB_STATUS_PAUSED | JOB_STATUS_SPOOLING)],
        ] {
            assert_eq!(kiem_hang_doi_truoc_khi_in(Some(&ds), Some(("INV_1".into(), MaSuCo::HetGiay))), None, "{:?}", ds);
        }
        // không đọc được hàng đợi → dựa vào lần đọc gần nhất của theo dõi tiếp
        let ly_do = kiem_hang_doi_truoc_khi_in(None, Some(("INV_1".into(), MaSuCo::KetGiay))).unwrap();
        assert_eq!((ly_do.chu, ly_do.loai), (chu_ket_hoa_don("INV_1"), Some(MaSuCo::KetGiay)));
        assert_eq!(kiem_hang_doi_truoc_khi_in(None, None), None);
    }

    /// R-A(2): trạng thái máy in lúc rảnh = cấp máy GỘP cờ của job đang kẹt.
    #[test]
    fn r_a_trang_thai_ranh_gop_co_job_ket() {
        let (ma, ct) = tinh_trang_gop(&vong(0, vec![job(7, JOB_STATUS_PAPEROUT | JOB_STATUS_ERROR)])).unwrap();
        assert_eq!(ma, MaSuCo::HetGiay, "cấp máy bình thường nhưng job kẹt → không được báo binh_thuong");
        assert!(ct.as_deref().unwrap().contains("hoá đơn INV_2026_030045 kẹt"), "{:?}", ct);
        // ưu tiên §1 giữa cấp máy và job
        assert_eq!(tinh_trang_gop(&vong(PRINTER_STATUS_OFFLINE, vec![job(7, JOB_STATUS_ERROR)])).unwrap().0, MaSuCo::Offline);
        assert_eq!(tinh_trang_gop(&vong(PRINTER_STATUS_TONER_LOW, vec![job(7, JOB_STATUS_ERROR)])).unwrap().0, MaSuCo::LoiMayIn);
        // job của chương trình khác: không ghi tên tài liệu
        let (_, ct) = tinh_trang_gop(&vong(0, vec![JobHangDoi { status: JOB_STATUS_OFFLINE, ..job_khac(9) }])).unwrap();
        assert!(ct.as_deref().unwrap().starts_with("job khác kẹt") && !ct.unwrap().contains("bao gia"));
        // cả hai sạch → binh_thuong; không đọc được máy in → None
        assert_eq!(tinh_trang_gop(&vong(0, vec![job(7, JOB_STATUS_BLOCKED_DEVQ)])), Some((MaSuCo::BinhThuong, None)));
        assert_eq!(tinh_trang_gop(&VongDoc::default()), None);
    }

    // --- R-E: tách jobId từ tên file ---
    #[test]
    fn r_e_tach_job_id_tu_ten_file() {
        let bang = [
            ("AI-INV_2026_030045-Anh_Loc-clx0abc12345678-1727170000000.pdf", Some("clx0abc12345678-1727170000000")),
            (
                "AI-INV_1-Anh_Loc-550e8400-e29b-41d4-a716-446655440000-1727170000000.pdf",
                Some("550e8400-e29b-41d4-a716-446655440000-1727170000000"),
            ),
            ("AI-INV_1-Khong_ro-1790251200000-7.pdf", Some("1790251200000-7")),
            ("AI-INV_1-Khach-1790251200000-7.PDF", Some("1790251200000-7")),
            (r"C:\Users\NV\AppData\Local\Temp\AI-INV_1-K-1790251200000-7.pdf", Some("1790251200000-7")),
            ("AI-INV_1-K-1790251200000-7", Some("1790251200000-7")),
            ("AI-INV_1-K-tokHN-1727170000000-3.pdf", Some("tokHN-1727170000000-3")),
            ("print-agent-1790251200000-7-18a2f.pdf", Some("1790251200000-7")),
            ("print-agent-clx0abc12345678-1727170000000-18a2f00ab.pdf", Some("clx0abc12345678-1727170000000")),
            // T8: phần tách ra phải đúng dạng id backend
            ("print-agent-in-thu-18a.pdf", None),
            ("AI-Report-Q3-x.pdf", None),
            ("AI-Report-Q3-2024-final.pdf", None),
            ("AI-INV_1-K-12345-7.pdf", None),
            ("AI-INV_1-K-CLX0ABC12345678-1727170000000.pdf", None),
            ("AI-INV_1.pdf", None),
            ("AI-INV_1-Khach.pdf", None),
            ("AI--Khach-1-2.pdf", None),
            ("AI-INV_1-Khach-.pdf", None),
            ("AI-INV_1-Khach-a b.pdf", None),
            ("print-agent-1790251200000-7-xyz.pdf", None),
            ("print-agent-18a.pdf", None),
            ("Microsoft Word - AI-bao cao.docx", None),
        ];
        for (ten, mong) in bang {
            assert_eq!(tach_job_id_tu_ten(ten).as_deref(), mong, "{}", ten);
        }
        // khớp ngược: id tách được đúng là id `la_cua_job` nhận ra
        let j = job(7, 0);
        assert_eq!(tach_job_id_tu_ten(&j.document).as_deref(), Some(ID));
        assert!(la_cua_job(&j, &tach_job_id_tu_ten(&j.document).unwrap()));
        assert_eq!(so_hoa_don_cua(&j).as_deref(), Some("INV_2026_030045"));
    }

    /// T8: id backend đúng dạng — UUID có dấu `-` bên trong, tách từ PHẢI.
    #[test]
    fn t8_la_id_backend() {
        for id in [
            "550e8400-e29b-41d4-a716-446655440000-1727170000000",
            "550E8400-E29B-41D4-A716-446655440000-1727170000000",
            "clx0abc12345678-1727170000000",
            "cmf1a2b3c4d5e6f7g8h9i0j1k-1790251200000",
            "1790251200000-7",
            "1790251200000-12345",
            "tokHN-1727170000000-3",
            "tok-co-gach-1727170000000-3",
        ] {
            assert!(la_id_backend(id), "{}", id);
        }
        for id in [
            "", "x", "in-thu", "1727-3", "179025120000-7", "1790251200000-", "-1790251200000-7",
            "550e8400-e29b-41d4-a716-44665544000-1727170000000", "clx0ab-1727170000000", "Q3-x",
            "abc def-1790251200000-3", "1790251200000-7a",
        ] {
            assert!(!la_id_backend(id), "{}", id);
        }
    }

    /// T8: so pMachineName với tên máy này — không phân biệt hoa thường, bỏ
    /// tiền tố `\\`, bỏ phần miền; rỗng = không phải của ta.
    #[test]
    fn t8_cung_may() {
        assert!(cung_may(r"\\PC-SHOP", "PC-SHOP"));
        assert!(cung_may(r"\\pc-shop", "PC-SHOP"));
        assert!(cung_may("PC-SHOP", "pc-shop"));
        assert!(cung_may(r"\\PC-SHOP.lan", "PC-SHOP"));
        assert!(!cung_may(r"\\PC-KHO", "PC-SHOP"));
        assert!(!cung_may("", "PC-SHOP"), "không biết máy → không phải của ta");
        assert!(!cung_may(r"\\PC-SHOP", ""), "không biết tên máy này → không đụng");
        assert!(!cung_may(r"\\", ""));
    }

    // --- R11b: dọn job của app bị tạm dừng lúc khởi động ---
    #[test]
    fn la_job_cua_app_theo_ten_file() {
        assert!(la_job_cua_app("AI-INV_2026_030045-Anh_Loc-1790251200000-7.pdf"));
        assert!(la_job_cua_app("print-agent-in-thu-18a.pdf"));
        assert!(la_job_cua_app(r"C:\Users\NV\AppData\Local\Temp\AI-INV_1-x-1790251200000-2.pdf"));
        assert!(!la_job_cua_app(r"C:\Users\NV\AppData\Local\Temp\AI-INV_1-x-1-2.pdf"), "T8: id sai dạng");
        assert!(!la_job_cua_app("AI-Report-Q3-x.pdf"), "T8: tài liệu 'AI-…' của chương trình khác");
        assert!(!la_job_cua_app("Microsoft Word - AI-bao cao.docx"));
        assert!(!la_job_cua_app("bao-gia.pdf"));
    }

    #[test]
    fn khoi_dong_cho_chay_tiep_job_cua_app_bi_dung_khong_dung_job_khac() {
        use gia::MAY;
        let mut sp = SpoolerGia {
            hang_doi: Some(vec![
                job(7, JOB_STATUS_PAUSED),
                job(8, 0),
                JobHangDoi { status: JOB_STATUS_PAUSED, ..job_khac(9) },
                JobHangDoi { document: "print-agent-in-thu-1.pdf".into(), ..job(10, JOB_STATUS_PAUSED | JOB_STATUS_SPOOLING) },
                // T8: hàng đợi chia sẻ — job của app ở MÁY KHÁC đang dừng (app máy
                // kia có thể đang "tạm dừng → xoá") và job không rõ máy
                JobHangDoi { may_tinh: r"\\PC-KHO".into(), ..job(11, JOB_STATUS_PAUSED) },
                JobHangDoi { may_tinh: String::new(), ..job(12, JOB_STATUS_PAUSED) },
                // T8: tài liệu "AI-…" không phải của app
                JobHangDoi { document: "AI-Report-Q3-x.pdf".into(), ..job(13, JOB_STATUS_PAUSED) },
            ]),
            ..Default::default()
        };
        let kq = tiep_tuc_job_bi_dung_cua_app(&mut sp, MAY);
        assert_eq!(kq.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(), vec![7, 10]);
        assert!(kq.iter().all(|(_, _, r)| r.is_ok()));
        assert_eq!(sp.lenh, vec![(7, LenhJob::TiepTuc), (10, LenhJob::TiepTuc)],
            "không đụng job của Word / máy khác / không rõ máy / không dừng");
        // không đọc được hàng đợi → không làm gì
        let mut sp = SpoolerGia::default();
        assert!(tiep_tuc_job_bi_dung_cua_app(&mut sp, MAY).is_empty());
    }

    // --- U1/U2: máy in USB — hỏi thẳng thiết bị (usb_may_in.rs), đo ở HCM 25/09 ---

    fn trong_may_in_usb(ds: &[QuanSat]) -> Option<bool> {
        ds.iter().find_map(|q| match q {
            QuanSat::TrongMayInUsb { da_thay_loi, .. } => Some(*da_thay_loi),
            _ => None,
        })
    }

    /// Ca THẬT INV/2026/030110: HP Laser 107 cắm USB, hết giấy. Windows không
    /// biết gì (cờ máy 0, hàng đợi rỗng sau vài giây) — bản 0.2.0 báo `da_in`,
    /// NV gửi lại 3 lần, nạp giấy ra 3 tờ. Máy báo lỗi qua USB lúc KÉO GIẤY,
    /// muộn hơn khung 4 lần đọc cũ → phải bắt được, ra `khong_ro` + theo dõi USB.
    #[test]
    fn u2_ca_that_hcm_het_giay_sau_khi_roi_hang_doi_la_khong_ro_trong_may_in() {
        let mut vongs = vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)])];
        vongs.extend(std::iter::repeat_n(vong_usb(USB_DANG_IN), 2 + 5));
        vongs.push(vong_usb(USB_HET_GIAY));
        let mut sp = SpoolerGia { vong: vongs, ..Default::default() };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        let KetQuaIn::KhongRo(l) = kq else { panic!("{:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::CanXuLy));
        assert!(l.chu.contains("bo nho may in") && l.chu.contains("tu in"), "{}", l.chu);
        assert_eq!(trong_may_in_usb(&bao), Some(true), "giao theo dõi tiếp QUA USB");
        assert!(!co_theo_doi_tiep(&bao), "không còn trong hàng đợi Windows");
        assert_eq!(so_su_co(&bao, MaSuCo::CanXuLy), 1, "su-co gửi NGAY lúc thấy lỗi");
    }

    /// Đối chứng cùng máy: in bình thường — BUSY vài giây rồi IDLE → `da_in`,
    /// và PHẢI chờ tới IDLE (không kết luận sau 4 lần đọc như máy mạng).
    #[test]
    fn u2_usb_in_xong_busy_roi_idle_moi_la_da_in() {
        let mut vongs = vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)])];
        vongs.extend(std::iter::repeat_n(vong_usb(USB_DANG_IN), 2 + 10));
        vongs.push(vong_usb(USB_RANH));
        let mut sp = SpoolerGia { vong: vongs, ..Default::default() };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        assert_eq!(kq, KetQuaIn::DaIn);
        assert_eq!(sp.so_vong_da_doc, 1 + 2 + 10 + 1, "đọc tới đúng lần IDLE");
        assert_eq!(trong_may_in_usb(&bao), None);
    }

    /// Giám sát vòng 2: máy CÓ STATUS mà lần nào cũng IDLE (chưa từng BUSY) —
    /// máy CHƯA in (đang "chuẩn bị"? bỏ lệnh?). Không bao giờ `da_in`: hết trần
    /// → `khong_ro` + theo dõi tiếp qua USB. (0.2.1: 12 lần IDLE là `da_in`.)
    #[test]
    fn u2_usb_idle_ma_chua_tung_busy_khong_bao_gio_da_in() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong_usb(USB_RANH)],
            ..Default::default()
        };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::KhongXacNhan)), "{:?}", kq);
        assert_eq!(trong_may_in_usb(&bao), Some(false));
        assert_eq!(sp.so_vong_da_doc, 1 + SO_LAN_VANG_LA_XONG + SO_LAN_USB_TOI_DA);
        // Chuẩn bị lâu (IDLE 20 lần) rồi mới in → vẫn chờ đúng tới IDLE sau BUSY.
        let mut vongs = vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)])];
        vongs.extend(std::iter::repeat_n(vong_usb(USB_RANH), 2 + 20));
        vongs.extend(std::iter::repeat_n(vong_usb(USB_DANG_IN), 8));
        vongs.push(vong_usb(USB_RANH));
        let mut sp = SpoolerGia { vong: vongs, ..Default::default() };
        let (kq, _) = chay_su_co_moi(&mut sp, 30);
        assert_eq!(kq, KetQuaIn::DaIn);
        assert_eq!(sp.so_vong_da_doc, 1 + 2 + 20 + 8 + 1);
    }

    /// Máy USB BUSY mãi (không lỗi): hết `SO_LAN_USB_TOI_DA` lần → `khong_ro
    /// (khong_xac_nhan)` + theo dõi tiếp qua USB (chưa từng thấy lỗi).
    #[test]
    fn u2_usb_ban_qua_han_la_khong_ro_va_theo_doi_usb() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong_usb(USB_DANG_IN)],
            ..Default::default()
        };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        let KetQuaIn::KhongRo(l) = kq else { panic!("{:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::KhongXacNhan));
        assert_eq!(trong_may_in_usb(&bao), Some(false));
        assert_eq!(sp.so_vong_da_doc, 1 + SO_LAN_VANG_LA_XONG + SO_LAN_USB_TOI_DA);
        assert_eq!(so_su_co(&bao, MaSuCo::KhongXacNhan), 0, "bận không phải sự cố");
    }

    /// Máy KHÔNG đọc được USB (mạng/WSD, hoặc thiết bị không mở được): luật
    /// cũ R5c giữ nguyên — đúng `SO_LAN_DOC_MAY_IN_SAU_KHI_ROI` lần là xong.
    #[test]
    fn u2_khong_co_usb_giu_luat_cu() {
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong(0, vec![])],
            ..Default::default()
        };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        assert_eq!(kq, KetQuaIn::DaIn);
        assert_eq!(sp.so_vong_da_doc, 1 + SO_LAN_VANG_LA_XONG + SO_LAN_DOC_MAY_IN_SAU_KHI_ROI);
        assert_eq!(trong_may_in_usb(&bao), None);
    }

    #[test]
    fn u2_bo_sau_khi_roi_thuan() {
        use crate::usb_may_in::TinhTrangUsb::*;
        // Lỗi USB ngay lần đầu.
        let mut bo = BoSauKhiRoi::default();
        assert_eq!(bo.them(Some(MaSuCo::CanXuLy), Some(Loi(MaSuCo::CanXuLy))), Some(SauKhiRoi::TrongMayInUsb(MaSuCo::CanXuLy)));
        // Cờ spooler báo lỗi, máy đọc được USB (chưa lỗi) → vẫn theo dõi được qua USB.
        let mut bo = BoSauKhiRoi::default();
        assert_eq!(bo.them(Some(MaSuCo::HetGiay), Some(DangIn)), Some(SauKhiRoi::TrongMayInUsb(MaSuCo::HetGiay)));
        // Không USB → luật cũ.
        let mut bo = BoSauKhiRoi::default();
        assert_eq!(bo.them(Some(MaSuCo::HetGiay), None), Some(SauKhiRoi::SuCo(MaSuCo::HetGiay)));
        // Đã thấy BUSY, giữa chừng không đọc được USB (hàng đợi có job khác) → chờ, rồi IDLE → Sach.
        let mut bo = BoSauKhiRoi::default();
        assert_eq!(bo.them(None, Some(DangIn)), None);
        for _ in 0..10 {
            assert_eq!(bo.them(None, None), None);
        }
        assert_eq!(bo.them(None, Some(Ranh)), Some(SauKhiRoi::Sach));
        // Máy không có STATUS (KhongLoi): chưa thấy in → cần đủ SO_LAN_USB_KHONG_THAY_IN.
        let mut bo = BoSauKhiRoi::default();
        for _ in 0..SO_LAN_USB_KHONG_THAY_IN - 1 {
            assert_eq!(bo.them(None, Some(KhongLoi)), None);
        }
        assert_eq!(bo.them(None, Some(KhongLoi)), Some(SauKhiRoi::Sach));
        // BUSY rồi lại BUSY: đếm "sạch chưa thấy in" không cộng dồn qua lần BUSY.
        let mut bo = BoSauKhiRoi::default();
        assert_eq!(bo.them(None, Some(DangIn)), None);
        assert_eq!(bo.them(None, Some(Ranh)), Some(SauKhiRoi::Sach));
    }

    /// U1: lỗi USB vào trạng thái gộp (luồng rảnh báo `can_xu_ly`, backend
    /// ngắt cầu dao), `chiTiet` nói rõ nguồn USB.
    #[test]
    fn u1_tinh_trang_gop_co_loi_usb() {
        let (ma, ct) = tinh_trang_gop(&vong_usb(USB_HET_GIAY)).unwrap();
        assert_eq!(ma, MaSuCo::CanXuLy);
        assert!(ct.as_deref().is_some_and(|c| c.contains("USB 0x90") && c.contains("hết giấy")), "{:?}", ct);
        assert_eq!(tinh_trang_gop(&vong_usb(USB_RANH)).unwrap().0, MaSuCo::BinhThuong);
        assert_eq!(tinh_trang_gop(&vong_usb(USB_DANG_IN)).unwrap().0, MaSuCo::BinhThuong);
        // Cờ spooler hết giấy + lỗi USB chung → mã ưu tiên cao hơn (hết giấy), chiTiet ghép hai nguồn.
        let v = VongDoc { co_may_in: Some(PRINTER_STATUS_PAPER_OUT), ..vong_usb(USB_HET_GIAY) };
        let (ma, ct) = tinh_trang_gop(&v).unwrap();
        assert_eq!(ma, MaSuCo::HetGiay);
        assert!(ct.as_deref().is_some_and(|c| c.contains("PAPER_OUT") && c.contains("USB 0x90")), "{:?}", ct);
    }

    /// U1: lỗi USB KHÔNG BAO GIỜ là cờ nền (R-B chỉ cho cờ spooler báo sai dai dẳng).
    #[test]
    fn u1_loi_usb_khong_vao_nen() {
        assert!(chup_nen(&vong_usb(USB_HET_GIAY)).is_empty());
        assert!(tap_may_in(&vong_usb(USB_HET_GIAY)).is_some_and(|t| t.co(MaSuCo::CanXuLy)));
    }

    // --- Giám sát 25/09 (bản 0.2.1 → 0.2.2) ---

    /// H1: máy USB, spooler kịp gắn PRINTED (byte cuối vào BỘ NHỚ máy in) rồi
    /// máy báo lỗi — bản 0.2.1 trả `da_in` ngay. Nay qua U2.
    #[test]
    fn g1_printed_tren_may_usb_van_phai_hoi_usb() {
        let mut sp = SpoolerGia {
            vong: vec![
                vong_may_usb(vec![job(7, JOB_STATUS_PRINTING)]),
                vong_may_usb(vec![job(7, JOB_STATUS_PRINTED)]),
                vong_usb(USB_DANG_IN),
                vong_usb(USB_HET_GIAY),
            ],
            ..Default::default()
        };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::CanXuLy)), "{:?}", kq);
        assert_eq!(trong_may_in_usb(&bao), Some(true));
        // Đối chứng máy KHÔNG phải USB: PRINTED = da_in ngay, không đọc thêm lần nào.
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong(0, vec![job(7, JOB_STATUS_PRINTED)])],
            ..Default::default()
        };
        let (kq, _) = chay_su_co_moi(&mut sp, 30);
        assert_eq!(kq, KetQuaIn::DaIn);
        assert_eq!(sp.so_vong_da_doc, 2);
    }

    /// H3: hoá đơn gửi thêm khi máy ĐANG GIỮ tờ trước (lỗi có sẵn) — lần đọc rỗng
    /// đầu tiên đã thấy lỗi USB, `BoSuy` kết luận ngay. Bản 0.2.1: `khong_ro` KHÔNG
    /// theo dõi, `conTrongHangDoi:false`, dải bảo "chỉ in lại nếu…". Nay: trong máy.
    #[test]
    fn g3_gui_them_khi_may_dang_giu_to_truoc_la_trong_may_in_usb() {
        let mut sp = SpoolerGia {
            vong: vec![vong_may_usb(vec![job(7, JOB_STATUS_PRINTING)]), vong_usb(USB_HET_GIAY)],
            ..Default::default()
        };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        let KetQuaIn::KhongRo(l) = kq else { panic!("{:?}", kq) };
        assert_eq!(l.loai, Some(MaSuCo::CanXuLy));
        assert!(l.chu.contains("bo nho may in"), "{}", l.chu);
        assert_eq!(trong_may_in_usb(&bao), Some(true));
        // Job CHƯA từng thấy trong hàng đợi (không biết đã tới máy chưa): không đoán.
        let mut sp = SpoolerGia { vong: vec![vong_usb(USB_HET_GIAY)], ..Default::default() };
        let (kq, bao) = chay_su_co_moi(&mut sp, 3);
        assert!(!matches!(kq, KetQuaIn::DaIn), "{:?}", kq);
        assert_eq!(trong_may_in_usb(&bao), None);
    }

    /// M5: một lần hỏi chuỗi 1284 trục trặc (STATUS thiếu) giữa BUSY và lúc kéo
    /// giấy hỏng — bản 0.2.1 báo `da_in`. Nay là thiếu tin, chờ tiếp.
    #[test]
    fn g5_status_thieu_sau_busy_khong_phai_in_xong() {
        let thieu = VongDoc { usb: Some(DocUsb { byte: 0x98, status: None }), ..vong_usb(USB_DANG_IN) };
        let mut vongs = vec![vong_may_usb(vec![job(7, JOB_STATUS_PRINTING)])];
        vongs.extend(std::iter::repeat_n(vong_usb(USB_DANG_IN), 3));
        vongs.push(thieu);
        vongs.push(vong_usb(USB_HET_GIAY));
        let mut sp = SpoolerGia { vong: vongs, ..Default::default() };
        let (kq, bao) = chay_su_co_moi(&mut sp, 30);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::CanXuLy)), "{:?}", kq);
        assert_eq!(trong_may_in_usb(&bao), Some(true));
        // Máy KHÔNG BAO GIỜ báo STATUS: KhongLoi vẫn đếm như lần sạch.
        let mut bo = BoSauKhiRoi::default();
        for _ in 0..SO_LAN_USB_KHONG_THAY_IN - 1 {
            assert_eq!(bo.them(None, Some(TinhTrangUsb::KhongLoi)), None);
        }
        assert_eq!(bo.them(None, Some(TinhTrangUsb::KhongLoi)), Some(SauKhiRoi::Sach));
    }

    /// Cổng hỏi USB: mọi job đã GỬI XONG (PRINTED/COMPLETE/RETAINED — "Keep
    /// printed documents") hoặc hàng đợi rỗng. Job đang chờ/đang gửi → không hỏi.
    #[test]
    fn g_cong_hoi_usb_theo_hang_doi() {
        assert!(hang_doi_cho_doc_usb(&[]));
        assert!(hang_doi_cho_doc_usb(&[job(1, JOB_STATUS_PRINTED | JOB_STATUS_RETAINED), job(2, JOB_STATUS_COMPLETE)]));
        for st in [0, JOB_STATUS_SPOOLING, JOB_STATUS_PRINTING, JOB_STATUS_PAUSED, JOB_STATUS_ERROR | JOB_STATUS_PAPEROUT] {
            assert!(!hang_doi_cho_doc_usb(&[job(1, JOB_STATUS_RETAINED | JOB_STATUS_PRINTED), job(2, st)]), "{:#x}", st);
        }
    }

    /// Trần thời gian: bận quá hạn ở máy USB → `UsbChuaXong`, không đoán `da_in`.
    #[test]
    fn g7_het_gio_may_usb_la_chua_xong() {
        let mut bo = BoSauKhiRoi::default();
        assert_eq!(bo.them(None, Some(TinhTrangUsb::Ranh)), None);
        assert_eq!(bo.het_gio(), SauKhiRoi::UsbChuaXong);
        assert_eq!(BoSauKhiRoi::default().het_gio(), SauKhiRoi::Sach, "máy không USB: như cũ");
    }
}
