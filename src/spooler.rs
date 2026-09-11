// SPDX-License-Identifier: AGPL-3.0-or-later
//! Xác nhận job đã IN THẬT hay chưa bằng cách poll Windows print spooler
//! (EnumJobs/GetPrinter), thay vì suy "đã in" chỉ từ exit code SumatraPDF
//! (Sumatra trả 0 ngay khi ĐÃ GỬI XONG cho spooler, không đợi in xong —
//! ken exit 0 sai lệch với "đã in thật" đã bắt gặp trên máy in HP thật).
//!
//! NGUYÊN TẮC CAO NHẤT (chống in đôi): khi không chắc job đã in hay chưa,
//! KHÔNG BAO GIỜ trả DaIn. Chỉ trả DaIn khi TRỰC TIẾP quan sát job ở trạng
//! thái PRINTED. Mọi tình huống mơ hồ sau khi đã thấy đang in → KhongRo
//! (job.rs sẽ KHÔNG emit gì cho trạng thái này — server tự suy "khong_ro",
//! KHÔNG retry). Lỗi rõ ràng TRƯỚC KHI thấy in → Loi (an toàn để retry vì
//! chắc chắn chưa in tờ nào).
//!
//! Thiết kế tách 2 lớp để test được trên Mac (không có Win32):
//!   1. `TrangThaiJob` — quan sát rời rạc rút gọn từ JOB_INFO_2W/PRINTER_INFO_2
//!      ở một lần poll (Win32 build ra, hoặc test tạo tay).
//!   2. `suy_ket_qua` — hàm THUẦN nhận chuỗi quan sát → KetQuaIn. Test được
//!      trên mọi OS vì không đụng Win32.
//! `theo_doi_job` (chỉ Windows) là vòng poll thật: gọi Win32, dựng
//! `TrangThaiJob` mỗi vòng, đẩy vào `suy_ket_qua` tăng dần, dừng sớm khi đã
//! có kết quả chắc chắn (DaIn) hoặc hết thời gian.

use crate::job::KetQuaIn;
use std::time::Duration;

/// Chu kỳ poll spooler.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Thời gian tối đa chờ spooler xác nhận trước khi bỏ cuộc (→ KhongRo, không emit).
pub const POLL_TIMEOUT: Duration = Duration::from_secs(15);

/// Trạng thái máy in đọc từ PRINTER_INFO_2.Status tại một lần poll (rút gọn
/// các cờ PRINTER_STATUS_* liên quan tới lỗi vật lý — offline/hết giấy/kẹt).
/// Chỉ dùng trên Windows (nơi có PRINTER_INFO_2 thật) — gate cfg để tránh
/// cảnh báo "never constructed" khi build trên Mac (stub không cần type này).
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrangThaiMayIn {
    BinhThuong,
    Loi(&'static str), // offline | het_giay | loi_vat_ly
}

/// Trạng thái job đọc từ JOB_INFO_2W.Status tại một lần poll, đã rút gọn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrangThaiJob {
    /// Job đã tìm thấy trong hàng đợi nhưng chưa bắt đầu in (spooling/paused/...).
    DangCho,
    /// Job đang thực sự in (cờ JOB_STATUS_PRINTING).
    DangIn,
    /// Job đã in xong (cờ JOB_STATUS_PRINTED) — bằng chứng mạnh nhất.
    DaInXong,
    /// Job có cờ lỗi (JOB_STATUS_ERROR/OFFLINE/PAPEROUT/BLOCKED_DEVQ) tại lần poll này.
    LoiJob(&'static str),
    /// Không tìm thấy job này trong hàng đợi tại lần poll này (đã xong & bị dọn,
    /// hoặc chưa kịp xuất hiện, hoặc lỗi enum/không match được — spooler.rs
    /// KHÔNG phân biệt ở tầng quan sát, để `suy_ket_qua` quyết theo lịch sử).
    KhongThay,
    /// Máy in báo lỗi (PRINTER_INFO_2.Status) tại lần poll này, độc lập cờ job.
    MayInLoi(&'static str),
    /// Poll thất bại (OpenPrinter/EnumJobs lỗi) — observability failure, KHÔNG
    /// phải bằng chứng in lỗi. Coi như một lần "không quan sát được gì".
    LoiTruyVan,
}

/// Suy KetQuaIn từ chuỗi quan sát theo thời gian (quan sát đầu = sớm nhất).
/// HÀM THUẦN — không Win32, test được trên mọi OS.
///
/// Quy tắc (đúng theo thiết kế ledger §21):
/// - Gặp DaInXong bất kỳ lúc nào → DaIn ngay (kể cả sau đó job biến mất).
/// - CHƯA từng thấy DangIn/DaInXong (chưa có bằng chứng đã bắt đầu in) và gặp
///   LoiJob/MayInLoi → Loi (chưa in tờ nào, retry an toàn).
///   KhongThay/LoiTruyVan ở giai đoạn này KHÔNG phải lỗi in — chỉ là chưa
///   quan sát được — tiếp tục poll; hết quan sát (hết thời gian) → KhongRo
///   (KHÔNG suy Loi vì có thể job in cực nhanh trước khi kịp thấy).
/// - ĐÃ từng thấy DangIn (đã bắt đầu in tờ vật lý) rồi sau đó gặp bất kỳ điều
///   mơ hồ nào (lỗi, mất dấu, hết thời gian) mà CHƯA thấy DaInXong → KhongRo
///   (không emit gì — chống in đôi tuyệt đối, không suy Loi vì có thể tờ đã
///   ra khỏi máy in trước khi lỗi được ghi nhận).
pub fn suy_ket_qua(quan_sat: &[TrangThaiJob]) -> KetQuaIn {
    let mut da_thay_dang_in = false;

    for ts in quan_sat {
        match ts {
            TrangThaiJob::DaInXong => return KetQuaIn::DaIn,
            TrangThaiJob::DangIn => {
                da_thay_dang_in = true;
            }
            TrangThaiJob::LoiJob(ly_do) | TrangThaiJob::MayInLoi(ly_do) => {
                if da_thay_dang_in {
                    return KetQuaIn::KhongRo(format!("loi sau khi da bat dau in: {}", ly_do));
                } else {
                    return KetQuaIn::Loi(format!("loi truoc khi in: {}", ly_do));
                }
            }
            TrangThaiJob::DangCho | TrangThaiJob::KhongThay | TrangThaiJob::LoiTruyVan => {
                // Chưa có bằng chứng gì mới — tiếp tục xét quan sát kế tiếp.
            }
        }
    }

    // Hết chuỗi quan sát mà không có DaInXong hay lỗi rõ ràng: hết thời gian
    // hoặc job không bao giờ xuất hiện trong hàng đợi (sumatra exit 0 nhưng
    // in quá nhanh để bắt được, hoặc job biến mất giữa chừng).
    if da_thay_dang_in {
        KetQuaIn::KhongRo("da bat dau in nhung khong xac nhan duoc luc in xong".into())
    } else {
        KetQuaIn::KhongRo("khong quan sat duoc job trong hang doi spooler (het thoi gian)".into())
    }
}

// ===================== Phần Win32 thật (chỉ Windows) =====================

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::{Instant, SystemTime};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Printing::{
        ClosePrinter, EnumJobsW, GetPrinterW, OpenPrinterW, JOB_INFO_2W, JOB_STATUS_BLOCKED_DEVQ,
        JOB_STATUS_ERROR, JOB_STATUS_OFFLINE, JOB_STATUS_PAPEROUT, JOB_STATUS_PRINTED,
        JOB_STATUS_PRINTING, PRINTER_INFO_2W, PRINTER_STATUS_ERROR, PRINTER_STATUS_OFFLINE,
        PRINTER_STATUS_PAPER_JAM, PRINTER_STATUS_PAPER_OUT,
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

    fn mo_may_in(printer: &str) -> Option<PrinterHandle> {
        let wide = to_wide(printer);
        let mut handle = HANDLE::default();
        let ok = unsafe { OpenPrinterW(PCWSTR(wide.as_ptr()), &mut handle, None) };
        if ok.is_ok() {
            Some(PrinterHandle(handle))
        } else {
            None
        }
    }

    /// Đọc PRINTER_INFO_2W.Status → rút gọn TrangThaiMayIn (None nếu query lỗi).
    fn doc_trang_thai_may_in(h: &PrinterHandle) -> Option<TrangThaiMayIn> {
        let mut needed: u32 = 0;
        // Lần gọi 1: chỉ để lấy kích thước buffer cần (luôn lỗi INSUFFICIENT_BUFFER).
        unsafe {
            let _ = GetPrinterW(h.0, 2, None, &mut needed);
        }
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = unsafe { GetPrinterW(h.0, 2, Some(&mut buf), &mut needed) };
        if ok.is_err() {
            return None;
        }
        let info = unsafe { &*(buf.as_ptr() as *const PRINTER_INFO_2W) };
        let status = info.Status;
        if status & PRINTER_STATUS_OFFLINE != 0 {
            Some(TrangThaiMayIn::Loi("may in offline"))
        } else if status & PRINTER_STATUS_PAPER_OUT != 0 {
            Some(TrangThaiMayIn::Loi("het giay"))
        } else if status & PRINTER_STATUS_PAPER_JAM != 0 {
            Some(TrangThaiMayIn::Loi("ket giay"))
        } else if status & PRINTER_STATUS_ERROR != 0 {
            Some(TrangThaiMayIn::Loi("loi may in"))
        } else {
            Some(TrangThaiMayIn::BinhThuong)
        }
    }

    /// Một job JOB_INFO_2W đã rút gọn field cần dùng để match + đọc trạng thái.
    struct JobRutGon {
        document: String,
        status: u32,
        /// Giây kể từ UNIX epoch, quy đổi từ SYSTEMTIME UTC (Submitted) — CÙNG
        /// đơn vị với submit_after_ticks (system_time_to_ticks) để so sánh
        /// đúng nghĩa "job nộp sau lúc ta gửi lệnh in", không phải so bừa hai
        /// đại lượng khác đơn vị.
        submitted_epoch_secs: i64,
    }

    /// EnumJobs cấp độ 2 (JOB_INFO_2W) — trả None nếu query lỗi.
    fn doc_danh_sach_job(h: &PrinterHandle) -> Option<Vec<JobRutGon>> {
        let mut needed: u32 = 0;
        let mut returned: u32 = 0;
        unsafe {
            let _ = EnumJobsW(h.0, 0, u32::MAX, 2, None, &mut needed, &mut returned);
        }
        if needed == 0 {
            return Some(Vec::new()); // hàng đợi rỗng — hợp lệ, không phải lỗi
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = unsafe { EnumJobsW(h.0, 0, u32::MAX, 2, Some(&mut buf), &mut needed, &mut returned) };
        if ok.is_err() {
            return None;
        }
        let ptr = buf.as_ptr() as *const JOB_INFO_2W;
        let mut ra = Vec::with_capacity(returned as usize);
        for i in 0..returned as usize {
            let job = unsafe { &*ptr.add(i) };
            let document = unsafe { pwstr_to_string(job.pDocument) };
            // JOB_INFO_2W.Submitted là SYSTEMTIME UTC — quy đổi ra giây từ
            // UNIX epoch để so sánh ĐÚNG ĐƠN VỊ với submit_after_ticks (không
            // phải so bừa hai đại lượng khác nghĩa như bản nháp trước).
            let submitted_epoch_secs = systemtime_utc_to_epoch_secs(&job.Submitted);
            ra.push(JobRutGon { document, status: job.Status, submitted_epoch_secs });
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
            let yoe = (y - era * 400) as i64; // [0, 399]
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

    /// Tìm job của ta trong danh sách: DocumentName chứa job_id + submit sau
    /// mốc gửi lệnh in. Nhiều candidate khớp → ambiguous, coi như không thấy
    /// (KHÔNG chọn bừa — đúng yêu cầu chống nhầm job).
    fn tim_job<'a>(jobs: &'a [JobRutGon], job_id: &str, submit_after_epoch_secs: i64) -> Option<&'a JobRutGon> {
        // Trừ hao 2s: SYSTEMTIME.wSecond làm tròn giây, submit_after lấy ngay
        // trước khi gọi Sumatra nên job thật có thể "trước" vài trăm ms theo
        // đồng hồ hệ thống — biên an toàn nhỏ, KHÔNG rộng tới mức bắt nhầm job cũ.
        const BIEN_AN_TOAN_GIAY: i64 = 2;
        let ung_vien: Vec<&JobRutGon> = jobs
            .iter()
            .filter(|j| {
                j.document.contains(job_id)
                    && j.submitted_epoch_secs >= submit_after_epoch_secs - BIEN_AN_TOAN_GIAY
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
                let max_secs = ung_vien.iter().map(|j| j.submitted_epoch_secs).max().unwrap();
                let cung_moi_nhat: Vec<&&JobRutGon> =
                    ung_vien.iter().filter(|j| j.submitted_epoch_secs == max_secs).collect();
                if cung_moi_nhat.len() == 1 {
                    Some(*cung_moi_nhat[0])
                } else {
                    None // thật sự ambiguous — coi như không thấy
                }
            }
        }
    }

    fn status_to_trang_thai_job(status: u32) -> TrangThaiJob {
        if status & JOB_STATUS_PRINTED != 0 {
            TrangThaiJob::DaInXong
        } else if status & JOB_STATUS_ERROR != 0 {
            TrangThaiJob::LoiJob("loi job")
        } else if status & JOB_STATUS_OFFLINE != 0 {
            TrangThaiJob::LoiJob("may in offline")
        } else if status & JOB_STATUS_PAPEROUT != 0 {
            TrangThaiJob::LoiJob("het giay")
        } else if status & JOB_STATUS_BLOCKED_DEVQ != 0 {
            TrangThaiJob::LoiJob("hang doi bi chan")
        } else if status & JOB_STATUS_PRINTING != 0 {
            TrangThaiJob::DangIn
        } else {
            TrangThaiJob::DangCho
        }
    }

    /// Poll spooler tối đa POLL_TIMEOUT, mỗi POLL_INTERVAL, cho tới khi
    /// `suy_ket_qua` (thuần) cho ra DaIn hoặc hết thời gian.
    pub fn theo_doi_job(printer: &str, job_id: &str, submit_after: SystemTime) -> KetQuaIn {
        let submit_after_epoch_secs = system_time_to_epoch_secs(submit_after);
        let bat_dau = Instant::now();
        let mut lich_su: Vec<TrangThaiJob> = Vec::new();

        loop {
            let quan_sat = match mo_may_in(printer) {
                None => TrangThaiJob::LoiTruyVan,
                Some(h) => {
                    let trang_thai_may = doc_trang_thai_may_in(&h);
                    match doc_danh_sach_job(&h) {
                        None => TrangThaiJob::LoiTruyVan,
                        Some(jobs) => match tim_job(&jobs, job_id, submit_after_epoch_secs) {
                            None => {
                                // Không thấy job — nhưng nếu máy in đang báo lỗi
                                // vật lý ngay lúc này, vẫn đáng ghi nhận (job có
                                // thể đã bị gạt khỏi hàng đợi do lỗi).
                                match trang_thai_may {
                                    Some(TrangThaiMayIn::Loi(ly_do)) => TrangThaiJob::MayInLoi(ly_do),
                                    _ => TrangThaiJob::KhongThay,
                                }
                            }
                            Some(j) => status_to_trang_thai_job(j.status),
                        },
                    }
                }
            };

            lich_su.push(quan_sat);

            // Dừng sớm nếu đã có bằng chứng mạnh nhất (đã in xong).
            if matches!(quan_sat, TrangThaiJob::DaInXong) {
                return suy_ket_qua(&lich_su);
            }
            // Dừng sớm nếu lỗi rõ ràng TRƯỚC khi từng thấy đang in (an toàn Loi).
            if let KetQuaIn::Loi(_) = suy_ket_qua(&lich_su) {
                return suy_ket_qua(&lich_su);
            }

            if bat_dau.elapsed() >= POLL_TIMEOUT {
                return suy_ket_qua(&lich_su);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn system_time_to_epoch_secs(t: SystemTime) -> i64 {
        t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }
}

#[cfg(windows)]
pub fn theo_doi_job(printer: &str, job_id: &str, submit_after: std::time::SystemTime) -> KetQuaIn {
    win::theo_doi_job(printer, job_id, submit_after)
}

/// Mac/dev (không có spooler Windows) — stub chỉ để compile; hành vi in thật
/// LUÔN chạy trên Windows qua nhánh #[cfg(windows)] ở trên.
#[cfg(not(windows))]
pub fn theo_doi_job(_printer: &str, _job_id: &str, _submit_after: std::time::SystemTime) -> KetQuaIn {
    KetQuaIn::DaIn
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

    // --- B: lỗi TRƯỚC khi thấy đang in → Loi (chưa in gì, an toàn retry) ---
    #[test]
    fn b_offline_truoc_khi_in_tra_loi() {
        let kq = suy_ket_qua(&[DangCho, LoiJob("may in offline")]);
        assert!(matches!(kq, KetQuaIn::Loi(_)));
    }

    #[test]
    fn b2_may_in_loi_ngay_dau_tra_loi() {
        let kq = suy_ket_qua(&[MayInLoi("het giay")]);
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
        let kq = suy_ket_qua(&[DangIn, LoiJob("ket giay")]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)), "expect KhongRo, got {:?}", kq);
    }

    #[test]
    fn c3_dang_in_roi_job_bien_mat_khong_ro() {
        let kq = suy_ket_qua(&[DangIn, KhongThay, KhongThay]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)));
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

    // --- H: lỗi máy in xen giữa lúc đang chờ (chưa in) → vẫn Loi (chưa có DangIn) ---
    #[test]
    fn h_loi_may_in_khi_dang_cho_tra_loi() {
        let kq = suy_ket_qua(&[DangCho, MayInLoi("offline")]);
        assert!(matches!(kq, KetQuaIn::Loi(_)));
    }

    // --- I: chuỗi rỗng (không poll được lần nào) → KhongRo, không panic ---
    #[test]
    fn i_chuoi_rong_tra_khong_ro_khong_panic() {
        let kq = suy_ket_qua(&[]);
        assert!(matches!(kq, KetQuaIn::KhongRo(_)));
    }
}
