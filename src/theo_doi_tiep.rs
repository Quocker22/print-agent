// SPDX-License-Identifier: AGPL-3.0-or-later
//! Theo dõi tiếp job `khong_ro` còn nằm trong hàng đợi Windows (R3, giám sát 25/09).
//!
//! VÌ SAO: từ bản này, job gặp sự cố mà byte có thể đã rời máy (cờ lỗi trên
//! job, đã PRINTING…) KHÔNG bị xoá nữa — nó nằm yên trong hàng đợi và tự in
//! khi NV xử lý xong máy (spooler.rs, R2). Backend đã ghi `khong_ro` sau 15 s
//! theo dõi; không ai báo lại thì hoá đơn in ra rồi mà ZaloCRM vẫn "không rõ"
//! mãi. Luồng này canh những job đó và gửi `ket-qua da_in` MUỘN (backend mới
//! nhận kết quả đến trễ cho job `khong_ro`).
//!
//! MỘT luồng, MỘT danh sách (không mỗi job một luồng): đọc EnumJobs mỗi
//! `CHU_KY`, giữ tối đa `GIU_TOI_DA`/job và `SO_JOB_TOI_DA` job (vượt thì bỏ
//! job cũ nhất — người gọi ghi file nhật ký).
//!
//! Kết luận bằng ĐÚNG luật của `spooler::suy_ket_qua`: phải có bằng chứng in
//! (PRINTING / PagesPrinted > 0 — kể cả thấy từ lúc theo dõi đầu) rồi rời hàng
//! đợi sạch `SO_LAN_VANG_LA_XONG` lần liên tiếp; đã thấy DELETING/DELETED/
//! RESTART thì biến mất KHÔNG tính là in; rời đi lúc máy in đang báo sự cố chặn
//! in MỚI (ngoài cờ nền, R-B) cũng không (job có thể đang trong bộ nhớ máy in,
//! R5c). Thiếu bằng chứng → `Mat`: net.rs báo `su-co khong_xac_nhan` (R-C).
//!
//! Giám sát vòng 2 thêm:
//! - Danh sách nằm trong `KhoTheoDoiTiep` (Arc) SỐNG QUA lần bấm Lưu (R-E(1)):
//!   luồng của lần chạy mạng mới NHẬN QUYỀN chủ (`nhan_chu`), luồng cũ thấy
//!   mất quyền thì tự thoát — không bao giờ hai luồng cùng đếm vắng một job.
//! - Mỗi job nhớ MÁY IN của nó: NV đổi máy in rồi bấm Lưu thì job cũ vẫn được
//!   đọc ở máy in cũ — đọc nhầm máy mới là thấy "vắng" → `Mat` → quản lý in
//!   lại trong khi job vẫn chờ ở máy cũ = hai tờ.
//! - Chu kỳ 500 ms (R-J): PRINTING có thể chỉ ~150 ms, đọc 1 s dễ lỡ → `Mat` oan.
//! - Nhớ mã kẹt của lần đọc gần nhất (R-A): worker hỏi trước khi in hoá đơn mới
//!   — chỉ khi lần đọc đó chưa quá `KET_CU_NHAT` (60 s, T2 giám sát vòng 3).
//! - Nhận lại lúc khởi động chỉ job của CHÍNH máy này, id đúng dạng backend (T8).
//!
//! Luồng này CHỈ ĐỌC — không bao giờ tạm dừng/xoá job.

#![cfg_attr(not(windows), allow(dead_code))]

use crate::job;
#[cfg(test)]
use crate::spooler::Spooler;
use crate::spooler::{self, BangChungJob, JobHangDoi, VongDoc, SO_LAN_VANG_LA_XONG};
use crate::su_co::{co, MaSuCo, TapMa};
use crate::usb_may_in::{DocUsb, TinhTrangUsb};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Chu kỳ đọc hàng đợi (R-J: 1 s dễ lỡ PRINTING ngắn).
pub const CHU_KY: Duration = Duration::from_millis(500);
/// Giữ một job tối đa 12 giờ — qua đêm mà máy vẫn chưa in thì thôi.
pub const GIU_TOI_DA: Duration = Duration::from_secs(12 * 3600);
/// Tối đa 200 job cùng lúc (máy in hỏng cả ngày ở shop đông khách).
pub const SO_JOB_TOI_DA: usize = 200;
/// Khi danh sách rỗng: nghỉ chừng này rồi xem lại (cờ dừng, job mới).
const CHO_KHI_RANH: Duration = Duration::from_millis(500);
/// Mã kẹt của lần đọc gần nhất chỉ dùng để TỪ CHỐI in (R-A, khi không đọc
/// được hàng đợi) nếu lần đọc đó mới hơn chừng này (T2, giám sát vòng 3):
/// spooler lỗi lâu thì "kẹt" của một giờ trước không còn là bằng chứng.
pub const KET_CU_NHAT: Duration = Duration::from_secs(60);

/// Một job đang được theo dõi tiếp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobTheoDoiTiep {
    /// Id job backend gửi (ĐẦY ĐỦ — để gửi `ket-qua`). Không ghi thẳng vào nhật ký.
    pub job_id: String,
    /// Số hoá đơn (hoặc id đã cắt token) — cho nhật ký/giao diện.
    pub so_hoa_don: String,
    /// Mã §1 của kết quả `khong_ro` ban đầu — ghi nhật ký.
    pub loai: Option<MaSuCo>,
    /// Máy in Windows chứa job. Rỗng = máy in hiện tại của luồng (test cũ).
    pub may_in: String,
    pub bat_dau: Instant,
    pub bang_chung: BangChungJob,
    vang_lien_tiep: usize,
    /// Trong chuỗi vắng hiện tại, máy in có lúc nào báo sự cố chặn in MỚI.
    vang_luc_may_in_loi: Option<MaSuCo>,
    /// Lần đọc gần nhất thấy job còn trong hàng đợi VỚI cờ kẹt (`CO_JOB_KET`)
    /// → mã của nó (R-A). `None` = sạch / đã rời đi.
    ket_lan_cuoi: Option<MaSuCo>,
    /// Lúc của lần đọc ĐỌC ĐƯỢC hàng đợi gần nhất (mốc tuổi của `ket_lan_cuoi`, T2).
    ket_luc: Option<Instant>,
    /// Mã kẹt ở lần CUỐI còn thấy job (không xoá khi job vắng). Kiểm cuối
    /// 25/09: job kẹt rồi biến mất mà không lần đọc nào thấy in SẠCH có HAI
    /// nghĩa spooler không phân biệt được — NV nạp giấy và máy in nốt rất nhanh
    /// (thường gặp), hoặc NV xoá tay job mà 500 ms không kịp thấy DELETING
    /// (hiếm). Vẫn kết luận `DaIn` (không báo động giả mỗi lần nạp giấy) nhưng
    /// `ghi_chu_da_in` đi kèm `da_in` lên ZaloCRM — ca xoá tay tìm được trong nhật ký.
    ket_khi_con_thay: Option<MaSuCo>,
    /// `Some` = job KHÔNG còn trong hàng đợi Windows mà nằm trong BỘ NHỚ máy in
    /// USB (U3) — kết luận theo trạng thái thiết bị, không theo hàng đợi.
    usb: Option<TheoDoiUsb>,
}

/// Theo dõi một hoá đơn nằm trong bộ nhớ máy in USB (U3). Máy HP Laser 107 ở
/// HCM (đo 25/09): hết giấy thì GIỮ các hoá đơn đã nhận, nạp giấy vào tự in
/// hết (3 lần gửi → 3 tờ liên tiếp). Chuỗi đúng: lỗi → BUSY (in) → IDLE.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TheoDoiUsb {
    /// Đã thấy máy báo lỗi (lúc giao theo dõi hoặc sau đó).
    pub da_thay_loi: bool,
    /// Từ lần lỗi GẦN NHẤT, đã thấy máy đang in (BUSY, không lỗi).
    pub da_thay_in: bool,
    /// Mã lỗi gần nhất — cho ghi chú đi kèm `da_in`.
    pub ma_loi: Option<MaSuCo>,
    /// Máy đã từng báo STATUS đo được — `KhongLoi` sau đó là THIẾU TIN.
    co_status: bool,
    /// Số lần đọc sạch liên tiếp mà chưa thấy máy in (từ lần lỗi/giao gần nhất).
    sach_chua_thay_in: usize,
    /// Số lần LIÊN TIẾP không hỏi được thiết bị (máy tắt / rút dây).
    mat_lien_tiep: usize,
    /// Thiết bị đã biến mất ≥ `SO_LAN_MAT_THIET_BI` lần lúc đang giữ hoá đơn.
    da_mat_thiet_bi: bool,
}

/// Không hỏi được thiết bị chừng này lần liên tiếp (2 s) = máy in bị tắt/rút
/// dây — HP Laser xoá bộ nhớ khi tắt: hoá đơn đang giữ có thể đã MẤT. Một lần
/// hỏi trục trặc lẻ không tính.
pub const SO_LAN_MAT_THIET_BI: usize = 4;

/// Máy USB CÓ STATUS rảnh (IDLE, không lỗi) chừng này lần liên tiếp (60 s) mà
/// chưa thấy in hoá đơn đang theo dõi → máy đã bỏ lệnh: `Mat`.
pub const SO_LAN_RANH_KHONG_IN: usize = 120;

/// Thông tin USB của một vòng đọc, cho `xet_mot_job`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UsbVong<'a> {
    pub la_may_usb: bool,
    pub doc: Option<&'a DocUsb>,
    pub khong_doc_duoc: bool,
}

impl<'a> UsbVong<'a> {
    pub fn tu(vong: &'a VongDoc) -> Self {
        Self { la_may_usb: vong.la_may_usb, doc: vong.usb.as_ref(), khong_doc_duoc: vong.usb_khong_doc_duoc }
    }
}

impl JobTheoDoiTiep {
    pub fn moi(job_id: String, so_hoa_don: String, loai: Option<MaSuCo>, bang_chung: BangChungJob, bat_dau: Instant) -> Self {
        Self {
            job_id,
            so_hoa_don,
            loai,
            may_in: String::new(),
            bat_dau,
            bang_chung,
            vang_lien_tiep: 0,
            vang_luc_may_in_loi: None,
            ket_lan_cuoi: None,
            ket_luc: None,
            ket_khi_con_thay: None,
            usb: None,
        }
    }

    /// Job nằm trong bộ nhớ máy in USB (U3) — `da_thay_loi`: máy đang báo lỗi
    /// lúc giao; `da_thay_in`: máy đã BUSY lúc giao (bận quá hạn).
    pub fn qua_usb(mut self, da_thay_loi: bool, da_thay_in: bool) -> Self {
        self.usb = Some(TheoDoiUsb { da_thay_loi, da_thay_in, ..TheoDoiUsb::default() });
        self
    }

    /// Đang theo dõi qua USB (U3)?
    pub fn la_qua_usb(&self) -> bool {
        self.usb.is_some()
    }

    /// Câu đi kèm `da_in` khi job còn KẸT ở lần cuối thấy nó (xem `ket_khi_con_thay`),
    /// hoặc khi máy USB từng báo lỗi (U3).
    pub fn ghi_chu_da_in(&self) -> Option<String> {
        if let Some(u) = self.usb {
            let ma = u.ma_loi.map_or("lỗi", MaSuCo::nhan);
            return match (u.da_thay_loi, u.da_thay_in) {
                (true, true) => Some(format!("In xong sau khi máy in hết lỗi ({}) — app xác nhận qua USB", ma)),
                (true, false) => Some(format!(
                    "Máy in hết lỗi ({}) nhưng app không thấy bước in — nếu cửa hàng đã huỷ lệnh trên máy in thì kiểm lại hoá đơn này",
                    ma
                )),
                (false, true) => None,
                (false, false) => Some(
                    "Máy in USB rảnh mà app không thấy bước in — nếu cửa hàng đã huỷ lệnh trên máy in thì kiểm lại hoá đơn này"
                        .to_string(),
                ),
            };
        }
        self.ket_khi_con_thay.map(|ma| {
            format!(
                "In xong sau khi hết sự cố ({}) — app không thấy bước in cuối; nếu cửa hàng đã xoá tay hàng đợi máy in thì kiểm lại hoá đơn này",
                ma.nhan()
            )
        })
    }

    /// Gắn máy in chứa job.
    pub fn tren_may_in(mut self, may_in: &str) -> Self {
        self.may_in = may_in.to_string();
        self
    }

    /// `Mat` vì job rời hàng đợi (đã có bằng chứng in) ĐÚNG LÚC máy in báo sự
    /// cố chặn in mới → mã đó: hoá đơn có thể đang nằm trong BỘ NHỚ máy in và
    /// tự ra khi khắc phục — câu gửi quản lý không được bảo "in lại" ngay.
    pub fn co_the_trong_may_in(&self) -> Option<MaSuCo> {
        (self.bang_chung.da_thay_in && !self.bang_chung.da_thay_huy).then_some(self.vang_luc_may_in_loi).flatten()
    }

    fn may_in_that<'a>(&'a self, mac_dinh: &'a str) -> &'a str {
        if self.may_in.is_empty() {
            mac_dinh
        } else {
            &self.may_in
        }
    }
}

/// Kết luận cho một job trong danh sách.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KetLuanTiep {
    /// Có bằng chứng in rồi rời hàng đợi sạch (hoặc thấy PRINTED) → gửi `da_in`.
    DaIn,
    /// Biến mất mà không đủ bằng chứng → `su-co khong_xac_nhan` (R-C).
    Mat(String),
    /// Quá `GIU_TOI_DA` mà job vẫn nằm đó → `su-co khong_xac_nhan` (R-C).
    HetHan,
}

/// Danh sách job đang theo dõi tiếp.
#[derive(Debug, Default)]
pub struct DanhSachTheoDoiTiep {
    ds: VecDeque<JobTheoDoiTiep>,
}

impl DanhSachTheoDoiTiep {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.ds.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.ds.is_empty()
    }

    /// Thêm job; trả job CŨ NHẤT bị bỏ nếu vượt trần. Cùng `job_id` đã có thì
    /// thay bản cũ (backend gửi lại cùng id — không theo dõi hai lần).
    pub fn them(&mut self, job: JobTheoDoiTiep) -> Option<JobTheoDoiTiep> {
        self.ds.retain(|j| j.job_id != job.job_id);
        self.ds.push_back(job);
        if self.ds.len() > SO_JOB_TOI_DA {
            self.ds.pop_front()
        } else {
            None
        }
    }

    /// Xử lý MỘT vòng đọc spooler cho MỌI job (một máy in); trả các job đã có
    /// kết luận (đã rút khỏi danh sách).
    #[cfg(test)]
    pub fn mot_vong(&mut self, vong: &VongDoc, bay_gio: Instant) -> Vec<(JobTheoDoiTiep, KetLuanTiep)> {
        self.xu_ly(vong, bay_gio, |_| true)
    }

    /// Như `mot_vong` nhưng chỉ các job nằm trên máy in `may_in`.
    pub fn mot_vong_cua(&mut self, may_in: &str, mac_dinh: &str, vong: &VongDoc, bay_gio: Instant) -> Vec<(JobTheoDoiTiep, KetLuanTiep)> {
        self.xu_ly(vong, bay_gio, |j| j.may_in_that(mac_dinh) == may_in)
    }

    fn xu_ly(&mut self, vong: &VongDoc, bay_gio: Instant, cua: impl Fn(&JobTheoDoiTiep) -> bool) -> Vec<(JobTheoDoiTiep, KetLuanTiep)> {
        let tap = spooler::tap_may_in(vong);
        let mut xong = Vec::new();
        let mut con = VecDeque::with_capacity(self.ds.len());
        for mut job in self.ds.drain(..) {
            if !cua(&job) {
                con.push_back(job);
                continue;
            }
            match xet_mot_job(&mut job, vong.hang_doi.as_deref(), tap, UsbVong::tu(vong), bay_gio) {
                Some(kl) => xong.push((job, kl)),
                None => con.push_back(job),
            }
        }
        self.ds = con;
        xong
    }

    /// Các máy in cần đọc ở vòng này (mỗi máy một lần).
    pub fn cac_may_in(&self, mac_dinh: &str) -> Vec<String> {
        let mut ra: Vec<String> = Vec::new();
        for j in &self.ds {
            let m = j.may_in_that(mac_dinh);
            if !ra.iter().any(|x| x == m) {
                ra.push(m.to_string());
            }
        }
        ra
    }

    /// Job của ta trên máy `may_in` mà lần đọc gần nhất (chưa quá
    /// `KET_CU_NHAT` tính tới `bay_gio`, T2) còn KẸT (R-A) → (số hoá đơn, mã
    /// ưu tiên cao nhất).
    pub fn job_dang_ket(&self, may_in: &str, mac_dinh: &str, bay_gio: Instant) -> Option<(String, MaSuCo)> {
        self.ds
            .iter()
            .filter(|j| j.may_in_that(mac_dinh) == may_in)
            .filter(|j| j.ket_luc.is_some_and(|t| bay_gio.saturating_duration_since(t) < KET_CU_NHAT))
            .filter_map(|j| j.ket_lan_cuoi.map(|m| (j.so_hoa_don.clone(), m)))
            .fold(None, |tot, (so, m)| match tot {
                Some((_, mt)) if crate::su_co::uu_tien_hon(mt, m) == mt => tot,
                _ => Some((so, m)),
            })
    }
}

fn xet_mot_job(
    job: &mut JobTheoDoiTiep,
    hang_doi: Option<&[JobHangDoi]>,
    tap_may_in: Option<TapMa>,
    usb: UsbVong<'_>,
    bay_gio: Instant,
) -> Option<KetLuanTiep> {
    if bay_gio.saturating_duration_since(job.bat_dau) >= GIU_TOI_DA {
        return Some(KetLuanTiep::HetHan);
    }
    if let Some(u) = job.usb.as_mut() {
        return xet_qua_usb(u, usb);
    }
    // Không đọc được hàng đợi (R5b) — không biết gì mới; ngắt chuỗi vắng. Mã
    // kẹt lần trước GIỮ NGUYÊN (R-A dựa vào nó đúng lúc hàng đợi không đọc được).
    let Some(hang_doi) = hang_doi else {
        job.vang_lien_tiep = 0;
        job.vang_luc_may_in_loi = None;
        return None;
    };
    // Mọi job của lần in này (copies > 1 là nhiều job cùng tên) — gộp cờ.
    let cua_ta: Vec<&JobHangDoi> = hang_doi.iter().filter(|j| spooler::la_cua_job(j, &job.job_id)).collect();
    if !cua_ta.is_empty() {
        let status = cua_ta.iter().fold(0, |c, j| c | j.status);
        if status & co::JOB_STATUS_PRINTED != 0 {
            return Some(KetLuanTiep::DaIn);
        }
        if cua_ta.iter().any(|j| spooler::da_bat_dau_in(j)) {
            job.bang_chung.da_thay_in = true;
        }
        if status & (co::JOB_STATUS_DELETING | co::JOB_STATUS_DELETED | co::JOB_STATUS_RESTART) != 0 {
            job.bang_chung.da_thay_huy = true;
        }
        job.ket_lan_cuoi = cua_ta.iter().filter_map(|j| spooler::ma_job_ket(j)).reduce(crate::su_co::uu_tien_hon);
        job.ket_khi_con_thay = job.ket_lan_cuoi;
        job.ket_luc = Some(bay_gio);
        job.vang_lien_tiep = 0;
        job.vang_luc_may_in_loi = None;
        return None;
    }
    job.ket_lan_cuoi = None;
    job.ket_luc = Some(bay_gio);
    job.vang_lien_tiep += 1;
    if job.vang_luc_may_in_loi.is_none() {
        // Chỉ mã chặn in MỚI so với cờ nền của job (R-B).
        job.vang_luc_may_in_loi = tap_may_in.and_then(|t| t.tru(job.bang_chung.nen).chan_in_uu_tien());
    }
    if job.vang_lien_tiep < SO_LAN_VANG_LA_XONG {
        return None;
    }
    // U3 (giám sát 25/09): job rời hàng đợi Windows (đủ số lần vắng) trên máy
    // USB cục bộ — máy in tắt rồi bật lại lúc hết giấy, bộ nhớ máy đầy… Không
    // bị xoá thì byte đã vào BỘ NHỚ máy in: hàng đợi rỗng không còn nói gì,
    // chuyển sang theo dõi qua USB. Luật cũ bên dưới ra `da_in` (đúng sự cố
    // HCM) hoặc `Mat` (quản lý in lại → hai tờ khi nạp giấy).
    // Chỉ khi ĐANG đọc được USB: không đọc được (máy tắt, không quyền…) mà vẫn
    // chuyển thì job treo 12 giờ rồi bị nhắc "in lại" (giám sát vòng 2).
    if usb.la_may_usb && usb.doc.is_some() && !job.bang_chung.da_thay_huy {
        let u = job.usb.insert(TheoDoiUsb {
            da_thay_loi: job.ket_khi_con_thay.is_some(),
            ma_loi: job.ket_khi_con_thay,
            ..TheoDoiUsb::default()
        });
        return xet_qua_usb(u, usb);
    }
    Some(if job.bang_chung.da_thay_huy {
        KetLuanTiep::Mat(spooler::CHU_BI_HUY.into())
    } else if !job.bang_chung.da_thay_in {
        KetLuanTiep::Mat("job roi hang doi ma chua tung thay in".into())
    } else if let Some(ma) = job.vang_luc_may_in_loi {
        KetLuanTiep::Mat(format!("job roi hang doi luc may in bao {} — co the con trong bo nho may in", ma.ma()))
    } else {
        KetLuanTiep::DaIn
    })
}

/// Một job trong bộ nhớ máy in USB (U3) — THUẦN.
///
/// - Không hỏi thiết bị (hàng đợi đang gửi, tạm ngừng) → không biết gì mới.
/// - Không hỏi ĐƯỢC ≥ `SO_LAN_MAT_THIET_BI` lần liên tiếp = máy tắt/rút dây:
///   thiết bị quay lại thì `Mat` — tắt máy xoá bộ nhớ, hoá đơn có thể đã mất;
///   báo `da_in` ở đây chính là sự cố gốc (giám sát 25/09).
/// - Lỗi → nhớ, xoá dấu "đã thấy in" (máy lỗi lại giữa chừng).
/// - BUSY không lỗi → đã thấy in. IDLE không lỗi → `DaIn` nếu đã thấy in;
///   chưa thấy mà rảnh `SO_LAN_RANH_KHONG_IN` lần (60 s) → `Mat` (máy bỏ lệnh).
/// - Máy KHÔNG có STATUS: đủ `SO_LAN_USB_KHONG_THAY_IN` lần "không lỗi" → `DaIn`
///   kèm ghi chú kiểm lại. STATUS thiếu ở máy có STATUS = thiếu tin.
fn xet_qua_usb(u: &mut TheoDoiUsb, usb: UsbVong<'_>) -> Option<KetLuanTiep> {
    if usb.khong_doc_duoc {
        u.mat_lien_tiep += 1;
        if u.mat_lien_tiep >= SO_LAN_MAT_THIET_BI {
            u.da_mat_thiet_bi = true;
        }
        return None;
    }
    let doc = usb.doc?;
    u.mat_lien_tiep = 0;
    if u.da_mat_thiet_bi {
        return Some(KetLuanTiep::Mat(
            "may in USB bi tat/rut day luc dang giu hoa don — bo nho may in co the da bi xoa".into(),
        ));
    }
    let sach = match doc.tinh_trang() {
        TinhTrangUsb::Loi(ma) => {
            u.da_thay_loi = true;
            u.da_thay_in = false;
            u.ma_loi = Some(ma);
            u.sach_chua_thay_in = 0;
            return None;
        }
        TinhTrangUsb::DangIn => {
            u.co_status = true;
            u.da_thay_in = true;
            u.sach_chua_thay_in = 0;
            return None;
        }
        TinhTrangUsb::Ranh => {
            u.co_status = true;
            true
        }
        TinhTrangUsb::KhongLoi => !u.co_status,
    };
    if !sach {
        return None;
    }
    if u.da_thay_in {
        return Some(KetLuanTiep::DaIn);
    }
    u.sach_chua_thay_in += 1;
    if !u.co_status {
        // Máy không có STATUS: chỉ biết "không lỗi" — đủ số lần là xong (kèm ghi chú).
        return (u.sach_chua_thay_in >= spooler::SO_LAN_USB_KHONG_THAY_IN).then_some(KetLuanTiep::DaIn);
    }
    // Máy có STATUS rảnh LÂU mà chưa từng in hoá đơn này (giám sát vòng 2): máy
    // laser in một tờ ≥ 5 s, đọc 500 ms không lỡ được — máy đã bỏ lệnh (huỷ trên
    // máy, xoá hàng đợi mà không kịp thấy DELETING, tắt máy…). `Mat`, KHÔNG `da_in`.
    (u.sach_chua_thay_in >= SO_LAN_RANH_KHONG_IN).then(|| {
        KetLuanTiep::Mat("may in USB ranh lau ma khong thay in hoa don nay — co the da bi huy tren may in / hang doi".into())
    })
}

/// Một vòng trên spooler (thật/giả) cho MỌI job: danh sách rỗng thì không đọc gì.
#[cfg(test)]
pub fn vong_theo_doi_tiep(
    sp: &mut dyn Spooler,
    ds: &mut DanhSachTheoDoiTiep,
    bay_gio: Instant,
) -> Vec<(JobTheoDoiTiep, KetLuanTiep)> {
    if ds.is_empty() {
        return Vec::new();
    }
    let vong = sp.doc_vong();
    ds.mot_vong(&vong, bay_gio)
}

#[derive(Debug, Default)]
struct TrongKho {
    ds: DanhSachTheoDoiTiep,
    /// Thế hệ luồng đang giữ quyền xử lý danh sách.
    chu: u64,
}

/// Danh sách theo dõi tiếp DÙNG CHUNG — sống qua lần bấm Lưu (R-E(1)). Worker
/// in đưa job vào (`them`) và hỏi job kẹt (`job_dang_ket`); đúng MỘT luồng
/// theo dõi tiếp (luồng giữ quyền chủ mới nhất) đọc spooler và kết luận.
#[derive(Debug, Default)]
pub struct KhoTheoDoiTiep {
    trong: Mutex<TrongKho>,
}

impl KhoTheoDoiTiep {
    fn khoa(&self) -> std::sync::MutexGuard<'_, TrongKho> {
        self.trong.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Thêm job; trả job bị bỏ vì vượt trần (người gọi ghi nhật ký).
    pub fn them(&self, job: JobTheoDoiTiep) -> Option<JobTheoDoiTiep> {
        self.khoa().ds.them(job)
    }

    pub fn job_dang_ket(&self, may_in: &str, bay_gio: Instant) -> Option<(String, MaSuCo)> {
        self.khoa().ds.job_dang_ket(may_in, may_in, bay_gio)
    }

    pub fn so_job(&self) -> usize {
        self.khoa().ds.ds.len()
    }

    /// Luồng mới nhận quyền chủ — luồng cũ (lần chạy mạng trước) mất quyền và tự thoát.
    fn nhan_chu(&self) -> u64 {
        let mut k = self.khoa();
        k.chu += 1;
        k.chu
    }

    fn cac_may_in(&self, the_he: u64, mac_dinh: &str) -> Option<Vec<String>> {
        let k = self.khoa();
        (k.chu == the_he).then(|| k.ds.cac_may_in(mac_dinh))
    }

    /// Xử lý một vòng đọc của máy `may_in` — CHỈ khi còn quyền chủ (kiểm trong
    /// cùng khoá: không có khe nào để hai luồng cùng đếm vắng một job).
    fn mot_vong_neu_chu(
        &self,
        the_he: u64,
        may_in: &str,
        mac_dinh: &str,
        vong: &VongDoc,
        bay_gio: Instant,
    ) -> Option<Vec<(JobTheoDoiTiep, KetLuanTiep)>> {
        let mut k = self.khoa();
        (k.chu == the_he).then(|| k.ds.mot_vong_cua(may_in, mac_dinh, vong, bay_gio))
    }
}

/// Vòng lặp của luồng theo dõi tiếp — tới khi `dung` bật hoặc mất quyền chủ
/// (lần chạy mạng mới đã nhận danh sách). `doc(may_in)` đọc một vòng spooler
/// của máy đó (thật: `mo_spooler(may_in).doc_vong()`); I/O nằm NGOÀI khoá.
pub fn chay_vong_lap(
    kho: &KhoTheoDoiTiep,
    may_in_mac_dinh: &str,
    dung: &AtomicBool,
    doc: &mut dyn FnMut(&str) -> VongDoc,
    ngu: &mut dyn FnMut(Duration),
    bay_gio: &dyn Fn() -> Instant,
    xu_ly: &mut dyn FnMut(JobTheoDoiTiep, KetLuanTiep),
) {
    let toi = kho.nhan_chu();
    loop {
        if dung.load(Ordering::SeqCst) {
            return;
        }
        let Some(cac_may) = kho.cac_may_in(toi, may_in_mac_dinh) else { return };
        if cac_may.is_empty() {
            ngu(CHO_KHI_RANH);
            continue;
        }
        for may in &cac_may {
            let vong = doc(may);
            let Some(xong) = kho.mot_vong_neu_chu(toi, may, may_in_mac_dinh, &vong, bay_gio()) else { return };
            for (job, kl) in xong {
                xu_ly(job, kl);
            }
        }
        if dung.load(Ordering::SeqCst) {
            return;
        }
        ngu(CHU_KY);
    }
}

/// Kết quả nhận lại job lúc khởi động (R-E(2)).
#[derive(Debug, Default, PartialEq)]
pub struct NhanLai {
    /// Job đưa vào theo dõi tiếp.
    pub nhan: Vec<JobTheoDoiTiep>,
    /// Tên tài liệu `AI-…`/`print-agent-…` KHÔNG tách được jobId đúng dạng
    /// backend — người gọi ghi nhật ký.
    pub bo_qua: Vec<String>,
    /// Số job của app do MÁY KHÁC nộp (hàng đợi chia sẻ, T8) — bỏ qua.
    pub may_khac: usize,
}

/// Lúc app khởi động (R-E(2)): job CỦA APP còn nằm trong hàng đợi (đã qua
/// bước RESUME R11b), do CHÍNH máy này nộp (T8 — hàng đợi chia sẻ có job của
/// máy khác), KHÔNG bị tạm dừng, không đang bị xoá, nộp chưa quá 12 giờ → đưa
/// vào theo dõi tiếp, để in xong thì báo `da_in` trễ (backend tra được job
/// theo printJobId trong jobId kể cả khi nó cũng vừa khởi động lại).
///
/// jobId tách từ tên file (`spooler::tach_job_id_tu_ten`, chỉ nhận id đúng
/// dạng backend). Job "In thử" không phải hoá đơn của backend — bỏ qua im lặng.
pub fn nhan_lai_khi_khoi_dong(
    vong: &VongDoc,
    may_in: &str,
    ten_may: &str,
    bay_gio: Instant,
    bay_gio_epoch_secs: i64,
) -> NhanLai {
    let mut nhan = DanhSachTheoDoiTiep::default();
    let mut ra = NhanLai::default();
    let Some(jobs) = vong.hang_doi.as_deref() else { return ra };
    let nen = spooler::chup_nen(vong);
    for j in jobs {
        let ten = j.document.rsplit(['\\', '/']).next().unwrap_or(&j.document);
        let bo = co::JOB_STATUS_PAUSED | co::JOB_STATUS_DELETING | co::JOB_STATUS_DELETED;
        let mau_ten_app = ten.starts_with("AI-") || ten.starts_with("print-agent-");
        if !mau_ten_app || spooler::la_job_in_thu(&j.document) || j.status & bo != 0 {
            continue;
        }
        if !spooler::cung_may(&j.may_tinh, ten_may) {
            ra.may_khac += 1;
            continue;
        }
        let tuoi = Duration::from_secs(bay_gio_epoch_secs.saturating_sub(j.submitted_epoch_secs).max(0) as u64);
        if tuoi >= GIU_TOI_DA {
            continue;
        }
        let Some(job_id) = spooler::tach_job_id_tu_ten(&j.document) else {
            ra.bo_qua.push(j.document.clone());
            continue;
        };
        let so = job::boc_hoa_don(ten, &job_id).map_or_else(|| job::rut_gon_job_id(&job_id), |(so, _)| so);
        let bang_chung = BangChungJob {
            da_thay_in: spooler::da_bat_dau_in(j),
            da_thay_huy: j.status & co::JOB_STATUS_RESTART != 0,
            nen,
        };
        let bat_dau = bay_gio.checked_sub(tuoi).unwrap_or(bay_gio);
        nhan.them(JobTheoDoiTiep::moi(job_id, so, spooler::ma_su_co_job(j), bang_chung, bat_dau).tren_may_in(may_in));
    }
    ra.nhan = nhan.ds.into_iter().collect();
    ra
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spooler::gia::{job, job_khac, vong, vong_usb, SpoolerGia, ID, USB_DANG_IN, USB_HET_GIAY, USB_RANH};
    use crate::su_co::co::*;
    use std::cell::RefCell;

    fn moi(bang_chung: BangChungJob, luc: Instant) -> JobTheoDoiTiep {
        JobTheoDoiTiep::moi(ID.into(), "INV_2026_030045".into(), Some(MaSuCo::KetGiay), bang_chung, luc)
    }

    fn da_in() -> BangChungJob {
        BangChungJob { da_thay_in: true, ..Default::default() }
    }

    /// Chạy `n` vòng trên spooler giả, gom mọi kết luận.
    fn chay_vong(sp: &mut SpoolerGia, ds: &mut DanhSachTheoDoiTiep, n: usize, t0: Instant) -> Vec<(String, KetLuanTiep)> {
        let mut ra = Vec::new();
        for i in 0..n {
            for (j, kl) in vong_theo_doi_tiep(sp, ds, t0 + CHU_KY * i as u32) {
                ra.push((j.job_id, kl));
            }
        }
        ra
    }

    /// Ví dụ của giám sát: kẹt giấy 40 vòng → PRINTING → vắng 3 vòng → đúng MỘT da_in.
    #[test]
    fn ket_giay_40_vong_roi_in_roi_roi_hang_doi_thi_dung_mot_da_in() {
        let t0 = Instant::now();
        let mut vongs = vec![vong(0, vec![job(7, JOB_STATUS_ERROR | JOB_STATUS_PAPEROUT)]); 40];
        vongs.push(vong(0, vec![job(7, JOB_STATUS_PRINTING)]));
        vongs.extend([vong(0, vec![]), vong(0, vec![]), vong(0, vec![])]);
        let mut sp = SpoolerGia { vong: vongs, ..Default::default() };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        let kl = chay_vong(&mut sp, &mut ds, 60, t0);
        assert_eq!(kl, vec![(ID.to_string(), KetLuanTiep::DaIn)], "đúng MỘT kết luận da_in");
        assert!(ds.is_empty());
        assert!(sp.lenh.is_empty(), "theo dõi tiếp CHỈ ĐỌC — không bao giờ dừng/xoá job");
        assert_eq!(sp.so_vong_da_doc, 43, "danh sách rỗng thì không đọc spooler nữa");
    }

    /// NV huỷ job trong hàng đợi (DELETING) → biến mất KHÔNG phải in → không gửi gì.
    #[test]
    fn nv_huy_job_thi_mat_khong_gui_da_in() {
        let t0 = Instant::now();
        let mut sp = SpoolerGia {
            vong: vec![
                vong(0, vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT)]),
                vong(0, vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_DELETING)]),
                vong(0, vec![]),
            ],
            ..Default::default()
        };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(da_in(), t0));
        let kl = chay_vong(&mut sp, &mut ds, 10, t0);
        assert_eq!(kl.len(), 1);
        assert!(matches!(kl[0].1, KetLuanTiep::Mat(ref l) if l.contains("huỷ")), "{:?}", kl);
    }

    /// Bằng chứng đã in từ lúc theo dõi đầu (PRINTING|PAPEROUT) được mang sang:
    /// NV nạp giấy, máy in nốt rất nhanh (không kịp thấy PRINTING) → vẫn da_in.
    #[test]
    fn bang_chung_tu_luc_dau_mang_sang() {
        let t0 = Instant::now();
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PAPEROUT)]), vong(0, vec![])],
            ..Default::default()
        };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(da_in(), t0));
        assert_eq!(chay_vong(&mut sp, &mut ds, 5, t0), vec![(ID.to_string(), KetLuanTiep::DaIn)]);
    }

    #[test]
    fn kiem_cuoi_job_con_ket_ngay_truoc_khi_bien_mat_thi_da_in_kem_ghi_chu() {
        // Job kẹt rồi biến mất, không lần đọc nào thấy in sạch: nạp giấy in nốt
        // rất nhanh HOẶC bị xoá tay — da_in (không báo động giả) + ghi chú kiểm lại.
        let t0 = Instant::now();
        let mut j = moi(da_in(), t0);
        let ket = job(7, JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT);
        assert_eq!(xet_mot_job(&mut j, Some(std::slice::from_ref(&ket)), None, UsbVong::default(), t0), None);
        for _ in 0..SO_LAN_VANG_LA_XONG - 1 {
            assert_eq!(xet_mot_job(&mut j, Some(&[]), None, UsbVong::default(), t0), None);
        }
        assert_eq!(xet_mot_job(&mut j, Some(&[]), None, UsbVong::default(), t0), Some(KetLuanTiep::DaIn));
        assert!(j.ghi_chu_da_in().is_some_and(|c| c.contains("xoá tay") && c.contains("Hết giấy")), "{:?}", j.ghi_chu_da_in());
        // Đối chứng: thấy in SẠCH trước khi vắng → không ghi chú.
        let mut j = moi(da_in(), t0);
        xet_mot_job(&mut j, Some(std::slice::from_ref(&ket)), None, UsbVong::default(), t0);
        xet_mot_job(&mut j, Some(&[job(7, JOB_STATUS_PRINTING)]), None, UsbVong::default(), t0);
        for _ in 0..SO_LAN_VANG_LA_XONG {
            xet_mot_job(&mut j, Some(&[]), None, UsbVong::default(), t0);
        }
        assert_eq!(j.ghi_chu_da_in(), None);
    }

    #[test]
    fn chua_tung_thay_in_ma_bien_mat_thi_mat() {
        let t0 = Instant::now();
        let mut sp = SpoolerGia { vong: vec![vong(0, vec![job(7, 0)]), vong(0, vec![])], ..Default::default() };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        let kl = chay_vong(&mut sp, &mut ds, 5, t0);
        assert!(matches!(kl.as_slice(), [(_, KetLuanTiep::Mat(_))]), "{:?}", kl);
    }

    /// R5c: rời hàng đợi đúng lúc máy in báo hết giấy — có thể trong bộ nhớ máy.
    #[test]
    fn roi_hang_doi_luc_may_in_loi_thi_mat() {
        let t0 = Instant::now();
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong(PRINTER_STATUS_PAPER_OUT, vec![]), vong(0, vec![])],
            ..Default::default()
        };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        let mut ra = Vec::new();
        for i in 0..5 {
            ra.extend(vong_theo_doi_tiep(&mut sp, &mut ds, t0 + CHU_KY * i));
        }
        assert!(matches!(ra.as_slice(), [(_, KetLuanTiep::Mat(ref l))] if l.contains("het_giay")), "{:?}", ra);
        assert_eq!(ra[0].0.co_the_trong_may_in(), Some(MaSuCo::HetGiay));
    }

    /// R-B trong theo dõi tiếp (kịch bản S5 của giám sát): cờ ERROR cấp máy có
    /// TỪ TRƯỚC (nền) — job PRINTING rồi rời hàng đợi → da_in, không `Mat`.
    /// Cùng ca nhưng ERROR MỚI xuất hiện (không có trong nền) → vẫn `Mat`.
    #[test]
    fn r_b_co_nen_khong_tinh_co_moi_van_tinh() {
        let t0 = Instant::now();
        let e = PRINTER_STATUS_ERROR;
        let chay = |nen: TapMa| {
            let mut sp = SpoolerGia {
                vong: vec![vong(e, vec![job(7, JOB_STATUS_PRINTING)]), vong(e, vec![]), vong(e, vec![]), vong(e, vec![])],
                ..Default::default()
            };
            let mut ds = DanhSachTheoDoiTiep::default();
            ds.them(moi(BangChungJob { nen, ..Default::default() }, t0));
            chay_vong(&mut sp, &mut ds, 6, t0)
        };
        let nen_error = crate::su_co::cac_ma_may_in(e, 0);
        assert_eq!(chay(nen_error), vec![(ID.to_string(), KetLuanTiep::DaIn)]);
        let kl = chay(TapMa::default());
        assert!(matches!(kl.as_slice(), [(_, KetLuanTiep::Mat(ref l))] if l.contains("loi_may_in")), "{:?}", kl);
    }

    /// R5b: EnumJobs lỗi không phải "vắng".
    #[test]
    fn doc_loi_khong_tinh_la_vang() {
        let t0 = Instant::now();
        let loi = VongDoc { co_may_in: Some(0), ..VongDoc::default() };
        let mut sp = SpoolerGia {
            vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTING)]), vong(0, vec![]), loi.clone(), vong(0, vec![]), loi],
            ..Default::default()
        };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        assert!(chay_vong(&mut sp, &mut ds, 10, t0).is_empty(), "vắng-lỗi-vắng-lỗi không bao giờ đủ 2 lần vắng liên tiếp");
    }

    #[test]
    fn printed_la_da_in_ngay() {
        let t0 = Instant::now();
        let mut sp = SpoolerGia { vong: vec![vong(0, vec![job(7, JOB_STATUS_PRINTED)])], ..Default::default() };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        assert_eq!(chay_vong(&mut sp, &mut ds, 1, t0), vec![(ID.to_string(), KetLuanTiep::DaIn)]);
    }

    #[test]
    fn het_12_gio_thi_bo() {
        let t0 = Instant::now();
        let mut sp = SpoolerGia { vong: vec![vong(0, vec![job(7, JOB_STATUS_PAPEROUT)])], ..Default::default() };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        assert!(vong_theo_doi_tiep(&mut sp, &mut ds, t0 + GIU_TOI_DA - Duration::from_secs(1)).is_empty());
        let kl = vong_theo_doi_tiep(&mut sp, &mut ds, t0 + GIU_TOI_DA);
        assert_eq!(kl.len(), 1);
        assert_eq!(kl[0].1, KetLuanTiep::HetHan);
        assert!(ds.is_empty());
    }

    #[test]
    fn toi_da_200_job_bo_cu_nhat_va_khong_trung_id() {
        let t0 = Instant::now();
        let mut ds = DanhSachTheoDoiTiep::default();
        for i in 0..SO_JOB_TOI_DA {
            let j = JobTheoDoiTiep::moi(format!("{}-{}", 1_790_000_000_000u64, i), String::new(), None, BangChungJob::default(), t0);
            assert!(ds.them(j).is_none());
        }
        let bo = ds.them(JobTheoDoiTiep::moi("moi-1".into(), String::new(), None, BangChungJob::default(), t0));
        assert_eq!(bo.map(|j| j.job_id), Some("1790000000000-0".to_string()), "bỏ job CŨ NHẤT");
        assert_eq!(ds.len(), SO_JOB_TOI_DA);
        // cùng id → thay, không nhân đôi
        ds.them(JobTheoDoiTiep::moi("moi-1".into(), String::new(), None, BangChungJob::default(), t0));
        assert_eq!(ds.len(), SO_JOB_TOI_DA);
    }

    /// Nhiều job theo dõi cùng lúc trong MỘT lần đọc; job của người khác không ảnh hưởng.
    #[test]
    fn nhieu_job_mot_lan_doc() {
        let t0 = Instant::now();
        let id2 = "1790251200000-8";
        let j2 = |status| JobHangDoi { document: format!("AI-INV_2-Khach-{}.pdf", id2), ..job(8, status) };
        let mut sp = SpoolerGia {
            vong: vec![
                vong(0, vec![job(7, JOB_STATUS_PRINTING), j2(JOB_STATUS_PAPEROUT), job_khac(9)]),
                vong(0, vec![j2(JOB_STATUS_PAPEROUT), job_khac(9)]),
            ],
            ..Default::default()
        };
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        ds.them(JobTheoDoiTiep::moi(id2.into(), "INV_2".into(), None, BangChungJob::default(), t0));
        let kl = chay_vong(&mut sp, &mut ds, 6, t0);
        assert_eq!(kl, vec![(ID.to_string(), KetLuanTiep::DaIn)]);
        assert_eq!(ds.len(), 1, "job hết giấy vẫn được theo dõi");
        assert_eq!(sp.so_vong_da_doc, 6, "MỘT lần đọc mỗi vòng cho cả danh sách");
        // R-A: lần đọc gần nhất job INV_2 còn kẹt hết giấy
        assert_eq!(ds.job_dang_ket("", "", t0 + CHU_KY * 6), Some(("INV_2".to_string(), MaSuCo::HetGiay)));
    }

    /// R-A: mã kẹt của lần đọc gần nhất — BLOCKED_DEVQ một mình không tính,
    /// đọc lỗi thì giữ, job rời đi thì xoá.
    #[test]
    fn r_a_nho_ma_ket_lan_doc_cuoi() {
        let t0 = Instant::now();
        let mut ds = DanhSachTheoDoiTiep::default();
        ds.them(moi(BangChungJob::default(), t0));
        ds.mot_vong(&vong(0, vec![job(7, JOB_STATUS_BLOCKED_DEVQ)]), t0);
        assert_eq!(ds.job_dang_ket("", "", t0), None, "BLOCKED_DEVQ là lỗi riêng một job — không chặn hoá đơn khác");
        let jam = JobHangDoi { mo_ta_driver: "Paper jam".into(), ..job(7, JOB_STATUS_PAPEROUT) };
        ds.mot_vong(&vong(0, vec![jam.clone()]), t0);
        assert_eq!(ds.job_dang_ket("", "", t0), Some(("INV_2026_030045".to_string(), MaSuCo::KetGiay)), "R-F: chữ driver nói kẹt");
        ds.mot_vong(&VongDoc { co_may_in: Some(0), ..VongDoc::default() }, t0 + Duration::from_secs(30));
        assert!(ds.job_dang_ket("", "", t0 + Duration::from_secs(30)).is_some(), "đọc lỗi → giữ mã lần trước");
        // T2: lần đọc được gần nhất đã quá 60 s (spooler lỗi lâu) → không dùng để từ chối in
        assert_eq!(ds.job_dang_ket("", "", t0 + KET_CU_NHAT), None, "mã kẹt quá 60 s không còn là bằng chứng");
        ds.mot_vong(&vong(0, vec![jam]), t0 + KET_CU_NHAT);
        assert!(ds.job_dang_ket("", "", t0 + KET_CU_NHAT).is_some(), "đọc lại được → dùng lại");
        // T2: NV tạm dừng job kẹt → không chặn hoá đơn khác
        ds.mot_vong(&vong(0, vec![job(7, JOB_STATUS_PAPEROUT | JOB_STATUS_PAUSED)]), t0 + KET_CU_NHAT);
        assert_eq!(ds.job_dang_ket("", "", t0 + KET_CU_NHAT), None);
        ds.mot_vong(&vong(0, vec![]), t0 + KET_CU_NHAT);
        assert_eq!(ds.job_dang_ket("", "", t0 + KET_CU_NHAT), None, "job rời hàng đợi → hết kẹt");
    }

    /// Luồng thật: cờ dừng làm vòng lặp thoát; kết luận đi qua `xu_ly`; đọc đúng
    /// máy in CỦA JOB (NV đổi máy in rồi bấm Lưu — R-E(1)).
    #[test]
    fn vong_lap_doc_dung_may_in_cua_job_va_thoat_khi_co_dung() {
        let kho = KhoTheoDoiTiep::default();
        let t0 = Instant::now();
        kho.them(moi(da_in(), t0).tren_may_in("HP cu"));
        let dung = AtomicBool::new(false);
        let da_doc = RefCell::new(Vec::new());
        let da = RefCell::new(Vec::new());
        chay_vong_lap(
            &kho,
            "Canon moi",
            &dung,
            &mut |may| {
                da_doc.borrow_mut().push(may.to_string());
                vong(0, vec![])
            },
            &mut |_| {},
            &|| t0,
            &mut |j, kl| {
                da.borrow_mut().push((j.job_id, kl));
                dung.store(true, Ordering::SeqCst);
            },
        );
        assert_eq!(da.into_inner(), vec![(ID.to_string(), KetLuanTiep::DaIn)]);
        assert!(da_doc.borrow().iter().all(|m| m == "HP cu"), "không đọc nhầm máy in mới: {:?}", da_doc.borrow());
    }

    /// R-E(1): danh sách SỐNG QUA lần bấm Lưu — luồng của lần chạy mạng mới
    /// nhận quyền, luồng cũ thoát ngay vòng sau, không đếm vắng thêm lần nào.
    #[test]
    fn r_e_luong_moi_nhan_quyen_luong_cu_thoat_danh_sach_con_nguyen() {
        let kho = KhoTheoDoiTiep::default();
        let t0 = Instant::now();
        kho.them(moi(BangChungJob::default(), t0).tren_may_in("HP"));
        let dung_cu = AtomicBool::new(false);
        let so_doc_cu = RefCell::new(0);
        // Luồng cũ: vòng 1 đọc bình thường, rồi "bấm Lưu" — luồng mới nhận quyền.
        chay_vong_lap(
            &kho,
            "HP",
            &dung_cu,
            &mut |_| {
                *so_doc_cu.borrow_mut() += 1;
                vong(0, vec![job(7, JOB_STATUS_PAPEROUT)])
            },
            &mut |_| {
                kho.nhan_chu();
            },
            &|| t0,
            &mut |_, _| panic!("không được kết luận"),
        );
        assert_eq!(*so_doc_cu.borrow(), 1, "mất quyền → thoát, không đọc thêm");
        assert_eq!(kho.so_job(), 1, "danh sách còn nguyên cho luồng mới");
        assert!(!dung_cu.load(Ordering::SeqCst), "thoát vì mất quyền, không phải vì cờ dừng");
        // Luồng mới tiếp tục: job in xong → da_in.
        let dung = AtomicBool::new(false);
        let da = RefCell::new(Vec::new());
        let mut i = 0;
        chay_vong_lap(
            &kho,
            "HP",
            &dung,
            &mut |_| {
                i += 1;
                if i == 1 { vong(0, vec![job(7, JOB_STATUS_PRINTING)]) } else { vong(0, vec![]) }
            },
            &mut |_| {},
            &|| t0,
            &mut |j, kl| {
                da.borrow_mut().push((j.job_id, kl));
                dung.store(true, Ordering::SeqCst);
            },
        );
        assert_eq!(da.into_inner(), vec![(ID.to_string(), KetLuanTiep::DaIn)]);
    }

    #[test]
    fn vong_lap_ranh_van_xem_co_dung() {
        let kho = KhoTheoDoiTiep::default();
        let dung = AtomicBool::new(false);
        let mut so_nghi = 0;
        chay_vong_lap(
            &kho,
            "HP",
            &dung,
            &mut |_| panic!("danh sách rỗng thì không đọc spooler"),
            &mut |_| {
                so_nghi += 1;
                if so_nghi == 3 {
                    dung.store(true, Ordering::SeqCst);
                }
            },
            &Instant::now,
            &mut |_, _| {},
        );
        assert_eq!(so_nghi, 3);
    }

    /// R-E(2): nhận lại job của app lúc khởi động — tách jobId từ tên file.
    #[test]
    fn r_e_nhan_lai_job_khi_khoi_dong() {
        let t0 = Instant::now();
        let bay_gio_epoch = 1_000 + 60;
        let tai_lieu = |id: u32, ten: &str, status: u32| JobHangDoi { document: ten.into(), ..job(id, status) };
        let v = VongDoc {
            co_may_in: Some(PRINTER_STATUS_ERROR),
            hang_doi: Some(vec![
                tai_lieu(1, "AI-INV_2026_030045-Anh_Loc-clx0abc12345678-1727170000000.pdf", JOB_STATUS_PAPEROUT),
                tai_lieu(2, r"C:\Temp\AI-INV_1-Khong_ro-1790251200000-7.pdf", JOB_STATUS_PRINTING),
                tai_lieu(3, "print-agent-1790251200000-8-18a2f.pdf", 0),
                tai_lieu(4, "AI-INV_3-Khach-1790251200000-9.pdf", JOB_STATUS_PAUSED),
                tai_lieu(5, "print-agent-in-thu-18a.pdf", 0),
                tai_lieu(6, "AI-hong.pdf", 0),
                tai_lieu(7, "Microsoft Word - bao gia.docx", JOB_STATUS_ERROR),
                tai_lieu(8, "AI-INV_4-K-1790251200000-10.pdf", JOB_STATUS_DELETING),
                JobHangDoi { submitted_epoch_secs: bay_gio_epoch - 13 * 3600, ..tai_lieu(9, "AI-INV_5-K-1-2.pdf", 0) },
                // T8: tài liệu "AI-…" của chương trình khác — tên không mang id backend
                tai_lieu(10, "AI-Report-Q3-x.pdf", 0),
                // T8: hàng đợi chia sẻ — job của app do máy khác / không rõ máy nộp
                JobHangDoi { may_tinh: r"\\PC-KHO".into(), ..tai_lieu(11, "AI-INV_6-K-1790251200000-11.pdf", 0) },
                JobHangDoi { may_tinh: String::new(), ..tai_lieu(12, "AI-INV_7-K-1790251200000-12.pdf", 0) },
            ]),
            ..VongDoc::default()
        };
        let NhanLai { nhan: ds, bo_qua, may_khac } = nhan_lai_khi_khoi_dong(&v, "HP", crate::spooler::gia::MAY, t0, bay_gio_epoch);
        let ids: Vec<&str> = ds.iter().map(|j| j.job_id.as_str()).collect();
        assert_eq!(ids, vec!["clx0abc12345678-1727170000000", "1790251200000-7", "1790251200000-8"],
            "không nhận job Paused / đang xoá / In thử / của Word / quá 12 giờ / máy khác");
        assert_eq!(bo_qua, vec!["AI-hong.pdf".to_string(), "AI-Report-Q3-x.pdf".to_string()]);
        assert_eq!(may_khac, 2, "job máy khác và job không rõ máy");
        assert_eq!(ds[0].so_hoa_don, "INV_2026_030045");
        assert_eq!(ds[0].loai, Some(MaSuCo::HetGiay));
        assert_eq!(ds[1].so_hoa_don, "INV_1");
        assert!(ds[1].bang_chung.da_thay_in, "đang PRINTING là bằng chứng đã in");
        assert_eq!(ds[2].so_hoa_don, "1790251200000-8");
        assert!(ds.iter().all(|j| j.may_in == "HP" && j.bang_chung.nen.co(MaSuCo::LoiMayIn)), "cờ nền chụp lúc nhận lại");
        // không đọc được hàng đợi → không nhận gì
        assert_eq!(nhan_lai_khi_khoi_dong(&VongDoc::default(), "HP", crate::spooler::gia::MAY, t0, bay_gio_epoch), NhanLai::default());
    }

    // --- U3: hoá đơn nằm trong BỘ NHỚ máy in USB (máy HCM, đo 25/09) ---

    use crate::spooler::gia::{vong_may_usb, vong_usb_mat};

    fn moi_usb(da_thay_loi: bool, t0: Instant) -> JobTheoDoiTiep {
        moi(da_in(), t0).qua_usb(da_thay_loi, false)
    }

    /// Một bước của `xet_mot_job` với vòng đọc `v` (hàng đợi lấy từ vòng).
    fn buoc(j: &mut JobTheoDoiTiep, v: &VongDoc, t: Instant) -> Option<KetLuanTiep> {
        xet_mot_job(j, v.hang_doi.as_deref(), spooler::tap_may_in(v), UsbVong::tu(v), t)
    }

    /// Ca thật: hết giấy (máy giữ hoá đơn) → NV nạp giấy → máy in (BUSY) →
    /// IDLE → đúng MỘT `da_in` muộn, kèm ghi chú "xác nhận qua USB".
    #[test]
    fn u3_het_giay_nap_giay_in_roi_ranh_la_da_in() {
        let t0 = Instant::now();
        let mut vongs = vec![vong_usb(USB_HET_GIAY); 30];
        vongs.extend(vec![vong_usb(USB_DANG_IN); 12]);
        vongs.push(vong_usb(USB_RANH));
        let mut sp = SpoolerGia { vong: vongs, ..Default::default() };
        let mut ds = DanhSachTheoDoiTiep::default();
        assert!(ds.them(moi_usb(true, t0)).is_none());
        let mut ra = Vec::new();
        for i in 0..60 {
            ra.extend(vong_theo_doi_tiep(&mut sp, &mut ds, t0 + CHU_KY * i));
        }
        assert_eq!(ra.len(), 1, "đúng MỘT kết luận");
        assert_eq!(ra[0].1, KetLuanTiep::DaIn);
        assert_eq!(sp.so_vong_da_doc, 30 + 12 + 1, "kết luận đúng lần IDLE đầu tiên");
        let ghi_chu = ra[0].0.ghi_chu_da_in().unwrap();
        assert!(ghi_chu.contains("qua USB") && ghi_chu.contains("cần người xử lý"), "{}", ghi_chu);
        assert!(ds.is_empty());
    }

    /// Giám sát vòng 2: hết lỗi mà máy RẢNH mãi, không in hoá đơn này (NV huỷ
    /// lệnh trên máy): `Mat` sau `SO_LAN_RANH_KHONG_IN` lần, KHÔNG bao giờ `da_in`.
    #[test]
    fn u3_het_loi_ranh_lau_khong_in_la_mat() {
        let t0 = Instant::now();
        let mut j = moi_usb(true, t0);
        assert_eq!(buoc(&mut j, &vong_usb(USB_HET_GIAY), t0), None);
        for _ in 0..SO_LAN_RANH_KHONG_IN - 1 {
            assert_eq!(buoc(&mut j, &vong_usb(USB_RANH), t0), None);
        }
        let kl = buoc(&mut j, &vong_usb(USB_RANH), t0);
        assert!(matches!(kl, Some(KetLuanTiep::Mat(ref l)) if l.contains("khong thay in")), "{:?}", kl);
    }

    /// Máy KHÔNG có STATUS: đủ số lần "không lỗi" là `da_in` kèm ghi chú kiểm lại.
    #[test]
    fn u3_may_khong_status_du_lan_sach_la_da_in_kem_ghi_chu() {
        let t0 = Instant::now();
        let mut j = moi_usb(true, t0);
        let khong_status = |byte: u8| VongDoc { usb: Some(DocUsb { byte, status: None }), ..vong_usb(USB_RANH) };
        assert_eq!(buoc(&mut j, &khong_status(0x10), t0), None, "lỗi");
        for _ in 0..spooler::SO_LAN_USB_KHONG_THAY_IN - 1 {
            assert_eq!(buoc(&mut j, &khong_status(0x18), t0), None);
        }
        assert_eq!(buoc(&mut j, &khong_status(0x18), t0), Some(KetLuanTiep::DaIn));
        assert!(j.ghi_chu_da_in().is_some_and(|c| c.contains("không thấy bước in")));
    }

    /// Máy lỗi LẠI giữa lúc đang in (kẹt tờ thứ hai): dấu "đã thấy in" bị xoá.
    #[test]
    fn u3_loi_lai_giua_chung_xoa_dau_da_thay_in() {
        let t0 = Instant::now();
        let mut j = moi_usb(true, t0);
        for t in [USB_HET_GIAY, USB_DANG_IN, USB_HET_GIAY] {
            assert_eq!(buoc(&mut j, &vong_usb(t), t0), None);
        }
        assert_eq!(buoc(&mut j, &vong_usb(USB_RANH), t0), None, "chưa thấy in lại sau lỗi thứ hai");
        assert_eq!(buoc(&mut j, &vong_usb(USB_DANG_IN), t0), None);
        assert_eq!(buoc(&mut j, &vong_usb(USB_RANH), t0), Some(KetLuanTiep::DaIn));
        assert!(j.ghi_chu_da_in().is_some_and(|c| c.contains("qua USB")));
    }

    /// Không hỏi thiết bị (hàng đợi đang gửi job khác) → không kết luận gì,
    /// KHÔNG suy từ hàng đợi (job này vốn không còn trong hàng đợi); 12 giờ hết hạn.
    #[test]
    fn u3_khong_hoi_usb_thi_cho() {
        let t0 = Instant::now();
        let mut j = moi_usb(true, t0);
        for _ in 0..10 {
            assert_eq!(buoc(&mut j, &vong_may_usb(vec![job_khac(9)]), t0), None);
        }
        assert_eq!(buoc(&mut j, &vong_may_usb(vec![]), t0 + GIU_TOI_DA), Some(KetLuanTiep::HetHan));
    }

    /// Máy in bị TẮT/rút dây lúc đang giữ hoá đơn (giám sát 25/09): HP xoá bộ
    /// nhớ khi tắt — bật lại IDLE mà báo `da_in` là đúng sự cố gốc. Phải `Mat`.
    /// Một lần hỏi trục trặc lẻ không tính.
    #[test]
    fn u3_tat_may_luc_giu_hoa_don_la_mat_mot_lan_truc_trac_khong_tinh() {
        let t0 = Instant::now();
        let mut j = moi_usb(true, t0);
        assert_eq!(buoc(&mut j, &vong_usb_mat(), t0), None);
        assert_eq!(buoc(&mut j, &vong_usb(USB_HET_GIAY), t0), None, "trục trặc lẻ: vẫn đang giữ");
        for _ in 0..SO_LAN_MAT_THIET_BI {
            assert_eq!(buoc(&mut j, &vong_usb_mat(), t0), None);
        }
        let kl = buoc(&mut j, &vong_usb(USB_RANH), t0);
        assert!(matches!(kl, Some(KetLuanTiep::Mat(ref l)) if l.contains("tat/rut day")), "{:?}", kl);
    }

    /// STATUS thiếu (hỏi chuỗi 1284 trục trặc) ở máy ĐÃ từng báo STATUS = thiếu
    /// tin, không phải "rảnh" (giám sát 25/09).
    #[test]
    fn u3_status_thieu_o_may_co_status_khong_phai_ranh() {
        let t0 = Instant::now();
        let mut j = moi_usb(true, t0);
        assert_eq!(buoc(&mut j, &vong_usb(USB_DANG_IN), t0), None);
        let thieu = VongDoc { usb: Some(DocUsb { byte: 0x98, status: None }), ..vong_usb(USB_DANG_IN) };
        for _ in 0..20 {
            assert_eq!(buoc(&mut j, &thieu, t0), None);
        }
        assert_eq!(buoc(&mut j, &vong_usb(USB_RANH), t0), Some(KetLuanTiep::DaIn));
    }

    /// Giao vì BẬN quá hạn (đã thấy BUSY, chưa lỗi): IDLE → `da_in`, không ghi chú.
    #[test]
    fn u3_ban_qua_han_roi_in_xong_la_da_in_khong_ghi_chu() {
        let t0 = Instant::now();
        let mut j = moi(da_in(), t0).qua_usb(false, true);
        assert_eq!(buoc(&mut j, &vong_usb(USB_RANH), t0), Some(KetLuanTiep::DaIn));
        assert_eq!(j.ghi_chu_da_in(), None);
    }

    /// Nhiều hoá đơn cùng nằm trong máy (3 lần gửi lúc hết giấy): máy in hết
    /// rồi IDLE → CẢ BA cùng `da_in` trong cùng một vòng.
    #[test]
    fn u3_nhieu_hoa_don_trong_may_cung_da_in_khi_may_in_xong() {
        let t0 = Instant::now();
        let mut sp = SpoolerGia {
            vong: vec![vong_usb(USB_HET_GIAY), vong_usb(USB_DANG_IN), vong_usb(USB_RANH)],
            ..Default::default()
        };
        let mut ds = DanhSachTheoDoiTiep::default();
        for i in 1..=3 {
            let mut j = moi_usb(true, t0);
            j.job_id = format!("{}-{}", ID, i);
            ds.them(j);
        }
        let mut ra = Vec::new();
        for i in 0..3 {
            ra.push(vong_theo_doi_tiep(&mut sp, &mut ds, t0 + CHU_KY * i).len());
        }
        assert_eq!(ra, vec![0, 0, 3]);
    }

    /// R3 → U3 (giám sát 25/09): job kẹt trong hàng đợi Windows (máy in tắt lúc
    /// hết giấy) rồi rời hàng đợi trên máy USB — bản trước: hai lần vắng =
    /// `da_in` (sự cố gốc) hoặc `Mat` (in lại → hai tờ). Nay chuyển sang USB.
    #[test]
    fn u3_job_ket_hang_doi_roi_di_tren_may_usb_chuyen_sang_theo_doi_usb() {
        let t0 = Instant::now();
        let mut j = moi(da_in(), t0);
        let ket = vong_may_usb(vec![job(7, JOB_STATUS_PRINTING | JOB_STATUS_PAPEROUT)]);
        assert_eq!(buoc(&mut j, &ket, t0), None);
        // Rời hàng đợi, máy báo lỗi qua USB (giữ trong bộ nhớ).
        for _ in 0..SO_LAN_VANG_LA_XONG {
            assert_eq!(buoc(&mut j, &vong_usb(USB_HET_GIAY), t0), None);
        }
        assert!(j.la_qua_usb(), "đã chuyển sang theo dõi USB");
        for _ in 0..20 {
            assert_eq!(buoc(&mut j, &vong_usb(USB_HET_GIAY), t0), None, "máy còn lỗi: KHÔNG da_in, KHÔNG Mat");
        }
        assert_eq!(buoc(&mut j, &vong_usb(USB_DANG_IN), t0), None);
        assert_eq!(buoc(&mut j, &vong_usb(USB_RANH), t0), Some(KetLuanTiep::DaIn));
        // Đối chứng máy KHÔNG phải USB: luật cũ giữ nguyên.
        let mut j = moi(da_in(), t0);
        assert_eq!(buoc(&mut j, &vong(0, vec![job(7, JOB_STATUS_PRINTING)]), t0), None);
        assert_eq!(buoc(&mut j, &vong(0, vec![]), t0), None);
        assert_eq!(buoc(&mut j, &vong(0, vec![]), t0), Some(KetLuanTiep::DaIn));
        assert!(!j.la_qua_usb());
    }

    /// Giám sát vòng 2: job CHƯA in bị xoá tay khỏi hàng đợi (lỡ nhịp DELETING)
    /// trên máy USB rảnh — chuyển sang USB rồi rảnh mãi → `Mat` như bản 0.2.0,
    /// không `da_in` (0.2.2 đầu: 12 lần IDLE là `da_in`).
    #[test]
    fn u3_xoa_tay_chua_in_tren_may_usb_ranh_la_mat() {
        let t0 = Instant::now();
        let mut j = moi(BangChungJob::default(), t0);
        assert_eq!(buoc(&mut j, &vong_may_usb(vec![job(7, 0)]), t0), None);
        let mut kl = None;
        for _ in 0..SO_LAN_VANG_LA_XONG + SO_LAN_RANH_KHONG_IN + 1 {
            kl = buoc(&mut j, &vong_usb(USB_RANH), t0);
            if kl.is_some() {
                break;
            }
        }
        assert!(matches!(kl, Some(KetLuanTiep::Mat(_))), "{:?}", kl);
    }

    /// Không đọc được USB lúc job rời hàng đợi → KHÔNG chuyển sang USB (treo 12 giờ
    /// rồi nhắc in lại) — giữ luật cũ.
    #[test]
    fn u3_khong_doc_duoc_usb_luc_roi_hang_doi_giu_luat_cu() {
        let t0 = Instant::now();
        let mut j = moi(da_in(), t0);
        assert_eq!(buoc(&mut j, &vong_may_usb(vec![job(7, JOB_STATUS_PRINTING)]), t0), None);
        assert_eq!(buoc(&mut j, &vong_usb_mat(), t0), None);
        assert_eq!(buoc(&mut j, &vong_usb_mat(), t0), Some(KetLuanTiep::DaIn));
        assert!(!j.la_qua_usb());
    }
}
