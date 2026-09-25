// SPDX-License-Identifier: AGPL-3.0-or-later
//! Trạng thái chia sẻ giữa thread net (socket.io) và UI (Slint).
//!
//! VÌ SAO Arc<Mutex<..>>: State chia sẻ giữa net thread (ghi khi có job) và
//! UI timer thread (đọc mỗi 300ms để cập nhật giao diện). Mutex đơn giản, khoá
//! ngắn (chỉ trong lúc đọc/ghi struct), không giữ khoá qua I/O nên không đáng lo tranh chấp.

use crate::hang_doi::HangDoiApp;
use crate::job;
use crate::su_co::{MaSuCo, MucDo};

/// Một dòng log job in gần đây, hiển thị trong UI "In gần đây".
#[derive(Debug, Clone, Default)]
pub struct JobLog {
    /// Id job backend gửi — CHỈ để luồng theo dõi tiếp (R3) tìm lại dòng này;
    /// KHÔNG BAO GIỜ hiện ra giao diện (backend cũ nhét token vào id, §0.3).
    pub job_id: String,
    /// Số hoá đơn bóc từ `name` server gửi (`job::nhan_hien_thi`); server cũ
    /// không gửi `name` thì là id job ĐÃ CẮT TOKEN.
    pub so_hoa_don: String,
    /// Tên khách bóc từ `name` (None nếu server không gửi/không đọc được).
    pub khach: Option<String>,
    /// "da_in" | "loi" | "khong_ro" — khớp trang_thai trong job::KetQua.
    pub trang_thai: String,
    /// Mã §1 của lỗi / không rõ, để hiện "Lỗi — sẽ tự in lại: Hết giấy".
    pub loai: Option<MaSuCo>,
    /// Job `khong_ro` được luồng theo dõi tiếp xác nhận đã in sau đó (R3) —
    /// hiện "Đã in (sau khi khắc phục)".
    pub sau_khac_phuc: bool,
    /// copies > 1 mà chỉ in được (k, n) bản (T9) — hiện "Đã in k/n bản — …".
    pub ban_da_in: Option<(u32, u32)>,
    /// Thời điểm xử lý, định dạng giờ:phút:giây cho gọn UI (không cần chính xác ms).
    pub luc: String,
}

/// Dải cảnh báo gắn với MỘT hoá đơn đang có chuyện.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaiDai {
    /// Đang theo dõi job và đã thấy sự cố — chưa có kết quả. Hiện như "đang
    /// chờ trong máy in" (đúng cả khi rốt cuộc là `loi`: không ai được in tay).
    DangTheoDoi,
    /// Kết quả `loi`: app đã xoá job khỏi hàng đợi, backend sẽ tự gửi lại.
    Loi,
    /// Kết quả `khong_ro`.
    KhongRo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaiJob {
    pub loai_dai: LoaiDai,
    pub ma: Option<MaSuCo>,
    pub so_hoa_don: String,
    /// Để luồng theo dõi tiếp tắt đúng dải của job đã in (R3).
    pub job_id: String,
    /// `khong_ro` mà job KHÔNG còn trong hàng đợi Windows (R-D,
    /// `conTrongHangDoi:false`): hoá đơn có thể nằm trong bộ nhớ máy in — câu
    /// dải khác "đang chờ trong máy in" (không ai theo dõi tiếp nó).
    pub ngoai_hang_doi: bool,
    /// copies > 1 mà chỉ in được (k, n) bản, bản còn lại đã gỡ khỏi hàng đợi
    /// (T9): dải nói đúng chuyện đó, và KHÔNG tự tắt khi một job khác in xong
    /// (bản thiếu vẫn thiếu).
    pub ban_da_in: Option<(u32, u32)>,
}

impl DaiJob {
    /// Dải mức `loi` (đỏ). Chỉ mã mức cảnh báo (mực yếu) là không.
    pub fn la_muc_loi(&self) -> bool {
        self.ma.is_none_or(|m| m.muc() == MucDo::Loi)
    }
}

/// Dải đang hiện trên cửa sổ (T6) — MỘT nguồn quyết cho cả giao diện
/// (`view_model::canh_bao`) lẫn nút "Đã hiểu" (`TrangThaiChung::da_hieu`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaiHien<'a> {
    Job(&'a DaiJob),
    MayIn(MaSuCo),
    KhongCo,
}

/// Trần số dải hoá đơn CHƯA xử lý giữ cùng lúc (T6) — vượt thì bỏ dải cũ nhất.
pub const SO_DAI_TOI_DA: usize = 5;

/// Trạng thái chung: thread net ghi, UI đọc mỗi frame.
#[derive(Debug, Clone, Default)]
pub struct TrangThaiChung {
    pub da_noi: bool,
    /// Job gần đây, MỚI NHẤT Ở ĐẦU (index 0). Giới hạn ~20 dòng để UI không phình.
    pub jobs: Vec<JobLog>,
    /// Thông báo lỗi/log ngắn gọn gần nhất (vd "nối thất bại: ...") — hiện ở UI
    /// cho người dùng biết lý do mất kết nối thay vì chỉ thấy chip đỏ mù mờ.
    pub thong_bao_cuoi: Option<String>,
    /// Trạng thái máy in đọc GẦN NHẤT (GetPrinterW — lúc rảnh hoặc trong lúc theo
    /// dõi job) kèm chiTiet. `None` = chưa đọc được lần nào. Đây cũng là mốc
    /// "đổi trạng thái" để gửi `trang-thai-may-in` (xem `ghi_may_in`).
    pub may_in: Option<(MaSuCo, Option<String>)>,
    /// Dải cảnh báo của các hoá đơn có chuyện CHƯA xử lý (R1), CŨ NHẤT TRƯỚC,
    /// tối đa `SO_DAI_TOI_DA`; mỗi hoá đơn một dải. Cần riêng vì nhiều driver
    /// KHÔNG bật cờ nào ở máy in, chỉ bật trên job — chỉ nhìn `may_in` thì máy
    /// hết giấy mà dải cảnh báo không hiện.
    ///
    /// NHIỀU Ô (T6, giám sát vòng 3): bản trước chỉ có MỘT ô — dải "báo quản lý
    /// kiểm" của hoá đơn A bị sự cố/kết quả của B đè, rồi MẤT HẲN khi B in
    /// xong; NV không bao giờ biết A cần kiểm. Nay mỗi dải tự tắt theo đúng
    /// luật R1/R-M của CHÍNH nó; giao diện hiện dải mới nhất + "(+N cảnh báo khác)".
    pub dai_jobs: Vec<DaiJob>,
    /// NV đã bấm "Đã hiểu" khi máy in đang báo mã này — ẩn dải sự cố máy in
    /// tới khi mã đổi.
    pub may_in_da_hieu: Option<MaSuCo>,
    /// Kết nối hiện tại mở đã 10 s mà server không gửi `cau-hinh` (R12).
    pub server_ban_cu: bool,
    /// Server từ chối kết nối (CONNECT_ERROR — token sai / bị thu hồi / DB
    /// backend lỗi, R-I). Hiện dòng nhỏ tới khi nối được; nhật ký ghi một lần.
    pub tu_choi_ket_noi: Option<String>,
    /// Hàng đợi server giữ + huỷ / bỏ theo dõi (hợp đồng v5.1 §8, 0.2.6).
    pub hang_doi: HangDoiApp,
}

/// `print_jobs.id` của một job id backend gửi (`<id>-<13 chữ số ms>`, 25/09).
/// Id kiểu khác (backend cũ nhét token) → `None`.
pub fn print_job_id(job_id: &str) -> Option<&str> {
    let (dau, duoi) = job_id.rsplit_once('-')?;
    (duoi.len() == 13 && duoi.bytes().all(|b| b.is_ascii_digit()) && !dau.is_empty()).then_some(dau)
}

/// Số job log tối đa giữ trong bộ nhớ — tránh Vec phình vô hạn khi agent chạy lâu ngày.
pub const MAX_JOB_LOG: usize = 20;

impl TrangThaiChung {
    /// Thêm một job log mới vào đầu danh sách, cắt bớt nếu vượt MAX_JOB_LOG.
    /// Cùng `job_id` đã có (dòng trung gian "Đang gửi…") thì THAY dòng đó.
    pub fn them_job(&mut self, log: JobLog) {
        if !log.job_id.is_empty() {
            self.jobs.retain(|j| j.job_id != log.job_id);
        }
        self.jobs.insert(0, log);
        self.jobs.truncate(MAX_JOB_LOG);
    }

    /// Job vừa nhận: hiện NGAY dòng "Đang gửi xuống máy in…" (0.2.5).
    pub fn bat_dau_job(&mut self, job_id: &str, so_hoa_don: &str, khach: Option<String>, luc: String) {
        self.them_job(JobLog {
            job_id: job_id.to_string(),
            so_hoa_don: so_hoa_don.to_string(),
            khach,
            trang_thai: job::DANG_GUI.into(),
            luc,
            ..Default::default()
        });
    }

    /// Hoá đơn vừa được huỷ CHẮC CHẮN từ app (ack `ok:true`): "In gần đây" thay
    /// mọi dòng cũ của cùng hoá đơn (vd "Chờ giấy…" lúc bị từ chối) bằng MỘT
    /// dòng "Đã huỷ" — không để câu "nạp giấy là tự in" nằm cạnh hoá đơn đã huỷ.
    pub fn ghi_da_huy(&mut self, id_print_job: &str, so_hoa_don: &str, khach: Option<String>, luc: String) {
        self.jobs.retain(|j| print_job_id(&j.job_id) != Some(id_print_job));
        self.them_job(JobLog {
            job_id: format!("huy:{}", id_print_job),
            so_hoa_don: so_hoa_don.to_string(),
            khach,
            trang_thai: job::DA_HUY.into(),
            luc,
            ..Default::default()
        });
    }

    /// Đổi trạng thái TRUNG GIAN của job đang xử lý (vd rời hàng đợi Windows →
    /// "đang chờ in ra"). Dòng đã có kết quả cuối thì không đụng.
    pub fn doi_trang_thai_job(&mut self, job_id: &str, trang_thai: &str) {
        for j in self.jobs.iter_mut().filter(|j| j.job_id == job_id) {
            if j.trang_thai == job::DANG_GUI || j.trang_thai == job::CHO_MAY_IN {
                j.trang_thai = trang_thai.to_string();
            }
        }
    }

    /// Ghi một lần đọc trạng thái máy in. Trả `true` khi MÃ đổi so với lần
    /// trước (lần đọc đầu tiên cũng tính là đổi) — chiTiet đổi mà mã giữ nguyên
    /// KHÔNG tính (backend §3.3 chống ồn cũng so theo mã).
    ///
    /// `luc_ranh`: lần đọc của luồng theo dõi máy in (không phải worker đọc giữa
    /// lúc in một job). Máy in lúc rảnh báo `binh_thuong` thì tắt dải loại
    /// `Loi` — backend chỉ chờ đúng tín hiệu này để tự gửi lại hoá đơn (R1).
    /// Dải `KhongRo` KHÔNG tắt ở đây: hoá đơn đó còn chờ trong máy in, tắt khi
    /// theo dõi tiếp xác nhận đã in, khi một job sau in xong, hoặc NV bấm "Đã hiểu".
    pub fn ghi_may_in(&mut self, ma: MaSuCo, chi_tiet: Option<String>, luc_ranh: bool) -> bool {
        let truoc = self.may_in.as_ref().map(|(m, _)| *m);
        if luc_ranh && ma == MaSuCo::BinhThuong {
            self.dai_jobs.retain(|d| d.loai_dai != LoaiDai::Loi);
        }
        if self.may_in_da_hieu.is_some_and(|m| m != ma) {
            self.may_in_da_hieu = None;
        }
        self.may_in = Some((ma, chi_tiet));
        truoc != Some(ma)
    }

    /// Đặt dải của MỘT hoá đơn: thay dải cũ của chính HOÁ ĐƠN đó (nếu có) rồi
    /// đưa lên mới nhất; vượt trần thì bỏ dải `Loi` cũ nhất trước (hoá đơn đó hệ
    /// thống tự gửi lại), không có thì dải cũ nhất.
    ///
    /// VÌ SAO khoá theo SỐ HOÁ ĐƠN, không theo id job (kiểm cuối 25/09): mỗi lượt
    /// backend gửi thử lại dùng id job MỚI — máy hết giấy 15 phút là 5 dải của
    /// cùng một hoá đơn, đẩy mất dải "báo quản lý kiểm" của hoá đơn khác, và
    /// "(+4 cảnh báo khác)" toàn là một hoá đơn.
    fn dat_dai(&mut self, dai: DaiJob) {
        let khoa = |d: &DaiJob| if d.so_hoa_don.is_empty() { d.job_id.clone() } else { d.so_hoa_don.clone() };
        let k = khoa(&dai);
        self.dai_jobs.retain(|d| khoa(d) != k);
        self.dai_jobs.push(dai);
        if self.dai_jobs.len() > SO_DAI_TOI_DA {
            let cu = self.dai_jobs.len() - 1; // không bỏ chính dải vừa đặt
            let bo = self.dai_jobs[..cu].iter().position(|d| d.loai_dai == LoaiDai::Loi).unwrap_or(0);
            self.dai_jobs.remove(bo);
        }
    }

    /// Dải hoá đơn mới nhất (đang hiện nếu không bị dải máy in lấn).
    pub fn dai_moi_nhat(&self) -> Option<&DaiJob> {
        self.dai_jobs.last()
    }

    /// Dải sự cố máy in lúc rảnh (không gắn hoá đơn) còn hiện được: có sự cố
    /// và NV chưa bấm "Đã hiểu" cho đúng mã đó.
    pub fn ma_dai_may_in(&self) -> Option<MaSuCo> {
        self.may_in.as_ref().map(|(m, _)| *m).filter(|m| *m != MaSuCo::BinhThuong && Some(*m) != self.may_in_da_hieu)
    }

    /// Dải nào đang hiện: dải hoá đơn mới nhất THẮNG dải máy in (nói rõ hoá
    /// đơn nào, ai in lại) — trừ khi dải hoá đơn chỉ là cảnh báo vàng mà máy
    /// in đang báo lỗi chặn in.
    pub fn dai_hien(&self) -> DaiHien<'_> {
        match (self.dai_moi_nhat(), self.ma_dai_may_in()) {
            (Some(j), Some(m)) if !j.la_muc_loi() && m.muc() == MucDo::Loi => DaiHien::MayIn(m),
            (Some(j), _) => DaiHien::Job(j),
            (None, Some(m)) => DaiHien::MayIn(m),
            (None, None) => DaiHien::KhongCo,
        }
    }

    /// Số cảnh báo CHƯA xử lý ngoài dải đang hiện — "(+N cảnh báo khác)" (T6).
    /// Dải máy in cùng mã với dải hoá đơn đang hiện không đếm (cùng một chuyện).
    pub fn so_canh_bao_khac(&self) -> usize {
        let may = self.ma_dai_may_in();
        match self.dai_hien() {
            DaiHien::KhongCo => 0,
            DaiHien::MayIn(_) => self.dai_jobs.len(),
            DaiHien::Job(j) => self.dai_jobs.len() - 1 + usize::from(may.is_some_and(|m| Some(m) != j.ma)),
        }
    }

    /// Sự cố quan sát được trong lúc đang in một job (chưa có kết quả).
    pub fn ghi_su_co_dang_in(&mut self, ma: MaSuCo, so_hoa_don: &str, job_id: &str) {
        self.dat_dai(DaiJob {
            loai_dai: LoaiDai::DangTheoDoi,
            ma: Some(ma),
            so_hoa_don: so_hoa_don.to_string(),
            job_id: job_id.to_string(),
            ngoai_hang_doi: false,
            ban_da_in: None,
        });
    }

    /// Kết quả cuối của một job (job còn trong hàng đợi / không rõ vị trí).
    #[cfg(test)]
    pub fn ghi_ket_qua_job(&mut self, job_id: &str, so_hoa_don: &str, trang_thai: &str, loai: Option<MaSuCo>) {
        self.ghi_ket_qua(job_id, so_hoa_don, trang_thai, loai, false, None);
    }

    /// Kết quả cuối của một job: `loi`/`khong_ro` → dải cho hoá đơn này; in
    /// xong → tắt dải CỦA CHÍNH NÓ, và tắt dải của hoá đơn KHÁC chỉ khi dải đó
    /// là sự cố máy in hoặc loại `Loi` (R-M): máy vừa in được một tờ thì sự
    /// cố máy in đã hết, còn backend tự gửi lại hoá đơn `loi`. Dải "chưa xác
    /// nhận được" (`khong_xac_nhan`/`loi_sumatra`/`loi_pdf`…) của hoá đơn khác
    /// thì GIỮ tới khi NV bấm "Đã hiểu" — hoá đơn B in xong không nói gì về A.
    /// Dải "đã in k/n bản" (T9) cũng giữ: bản thiếu vẫn thiếu.
    ///
    /// `ban_da_in` (T9): copies > 1, chỉ in được (k, n) bản.
    pub fn ghi_ket_qua(
        &mut self,
        job_id: &str,
        so_hoa_don: &str,
        trang_thai: &str,
        loai: Option<MaSuCo>,
        ngoai_hang_doi: bool,
        ban_da_in: Option<(u32, u32)>,
    ) {
        let loai_dai = match trang_thai {
            job::DA_IN => {
                self.dai_jobs.retain(|d| {
                    let tat = d.job_id == job_id
                        || (d.ban_da_in.is_none()
                            && (d.loai_dai == LoaiDai::Loi || d.ma.is_some_and(MaSuCo::la_su_co_may_in)));
                    !tat
                });
                return;
            }
            job::LOI => LoaiDai::Loi,
            _ => LoaiDai::KhongRo,
        };
        let khong_ro = loai_dai == LoaiDai::KhongRo;
        self.dat_dai(DaiJob {
            loai_dai,
            ma: loai,
            so_hoa_don: so_hoa_don.to_string(),
            job_id: job_id.to_string(),
            ngoai_hang_doi: ngoai_hang_doi && khong_ro,
            ban_da_in: ban_da_in.filter(|_| khong_ro),
        });
    }

    /// Luồng theo dõi tiếp xác nhận job `khong_ro` đã in (R3): dòng "In gần
    /// đây" thành "Đã in (sau khi khắc phục)", dải của job đó tắt.
    pub fn xac_nhan_in_sau(&mut self, job_id: &str) {
        for j in self.jobs.iter_mut().filter(|j| j.job_id == job_id) {
            j.trang_thai = job::DA_IN.into();
            j.loai = None;
            j.sau_khac_phuc = true;
        }
        self.dai_jobs.retain(|d| d.job_id != job_id);
    }

    /// Luồng theo dõi tiếp mất dấu job (biến mất không đủ bằng chứng in, hoặc
    /// quá 12 giờ): câu "đang chờ trong máy in" không còn đúng → "chưa xác
    /// nhận được". Dải BẬT LẠI cho hoá đơn này dù NV đã tắt dải cũ (R-C): đây
    /// là tin MỚI — hoá đơn có thể đã mất, NV phải biết để báo quản lý kiểm.
    #[cfg(test)]
    pub fn mat_dau_job(&mut self, job_id: &str, so_hoa_don: &str) {
        self.mat_dau_job_theo(job_id, so_hoa_don, None);
    }

    /// Như `mat_dau_job`; `co_the_trong_may_in` = job rời hàng đợi lúc máy in
    /// báo sự cố mã đó (R-C/R-D): dải "có thể đang nằm trong máy in" thay vì
    /// "chưa xác nhận được".
    pub fn mat_dau_job_theo(&mut self, job_id: &str, so_hoa_don: &str, co_the_trong_may_in: Option<MaSuCo>) {
        let ma = co_the_trong_may_in.unwrap_or(MaSuCo::KhongXacNhan);
        for j in self.jobs.iter_mut().filter(|j| j.job_id == job_id && j.trang_thai == job::KHONG_RO) {
            j.loai = Some(ma);
        }
        self.dat_dai(DaiJob {
            loai_dai: LoaiDai::KhongRo,
            ma: Some(ma),
            so_hoa_don: so_hoa_don.to_string(),
            job_id: job_id.to_string(),
            ngoai_hang_doi: co_the_trong_may_in.is_some(),
            ban_da_in: None,
        });
    }

    /// NV bấm "Đã hiểu": tắt DẢI ĐANG HIỆN (T6) — dải hoá đơn đang hiện thì bỏ
    /// dải đó (dải kế tiếp hiện lên); dải sự cố máy in thì ẩn tới khi mã đổi.
    pub fn da_hieu(&mut self) {
        match self.dai_hien() {
            DaiHien::Job(j) => {
                let id = j.job_id.clone();
                self.dai_jobs.retain(|d| d.job_id != id);
            }
            DaiHien::MayIn(m) => self.may_in_da_hieu = Some(m),
            DaiHien::KhongCo => {}
        }
    }

    /// Bấm Lưu cấu hình (T7, giám sát vòng 3): GIỮ "In gần đây" và các dải
    /// hoá đơn — worker cũ đang in dở và luồng theo dõi tiếp (sống qua Lưu)
    /// vẫn ghi kết quả vào ĐÚNG trạng thái này; bản trước dựng trạng thái MỚI
    /// nên kết quả đó rơi vào trạng thái cũ không ai hiện. Chỉ quên những gì
    /// gắn với kết nối/máy in cũ.
    pub fn doi_cau_hinh(&mut self) {
        self.da_noi = false;
        self.thong_bao_cuoi = None;
        self.may_in = None;
        self.may_in_da_hieu = None;
        self.server_ban_cu = false;
        self.tu_choi_ket_noi = None;
        // Server / token mới có thể là máy in KHÁC — hàng đợi cũ không còn của máy này.
        self.hang_doi = HangDoiApp::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(so: &str, trang_thai: &str, luc: &str) -> JobLog {
        JobLog { so_hoa_don: so.into(), trang_thai: trang_thai.into(), luc: luc.into(), ..Default::default() }
    }

    #[test]
    fn them_job_moi_nhat_len_dau() {
        let mut t = TrangThaiChung::default();
        t.them_job(log("1", "da_in", "10:00:00"));
        t.them_job(log("2", "da_in", "10:00:01"));
        assert_eq!(t.jobs[0].so_hoa_don, "2");
        assert_eq!(t.jobs[1].so_hoa_don, "1");
    }

    #[test]
    fn them_job_cat_bot_khi_vuot_gioi_han() {
        let mut t = TrangThaiChung::default();
        for i in 0..(MAX_JOB_LOG + 5) {
            t.them_job(log(&i.to_string(), "da_in", "x"));
        }
        assert_eq!(t.jobs.len(), MAX_JOB_LOG);
        // mới nhất (index cuối cùng thêm vào) phải ở đầu
        assert_eq!(t.jobs[0].so_hoa_don, (MAX_JOB_LOG + 4).to_string());
    }

    #[test]
    fn ghi_may_in_chi_bao_doi_khi_ma_doi() {
        let mut t = TrangThaiChung::default();
        assert!(t.ghi_may_in(MaSuCo::BinhThuong, None, true), "lần đọc đầu là đổi (từ chưa biết)");
        assert!(!t.ghi_may_in(MaSuCo::BinhThuong, None, true));
        assert!(t.ghi_may_in(MaSuCo::HetGiay, Some("a".into()), true));
        assert!(!t.ghi_may_in(MaSuCo::HetGiay, Some("b".into()), true), "chiTiet đổi, mã giữ → không gửi lại");
        assert_eq!(t.may_in, Some((MaSuCo::HetGiay, Some("b".into()))));
        assert!(t.ghi_may_in(MaSuCo::BinhThuong, None, true));
    }

    /// pb4 (giám sát 25/09): bản trước chỉ tắt dải khi máy in ĐI TỪ sự cố VỀ
    /// bình thường — driver chỉ bật cờ trên job thì máy in luôn "bình thường",
    /// 100 lần đọc mà dải vẫn còn. Luật mới: dải loại `Loi` tắt ở lần đọc lúc
    /// rảnh đầu tiên thấy `binh_thuong`.
    #[test]
    fn pb4_dai_loi_tat_khi_doc_luc_ranh_thay_binh_thuong() {
        let mut t = TrangThaiChung::default();
        t.ghi_may_in(MaSuCo::BinhThuong, None, true);
        t.ghi_ket_qua_job("j1", "INV_1", "loi", Some(MaSuCo::HetGiay));
        t.ghi_may_in(MaSuCo::BinhThuong, None, false);
        assert!(t.dai_moi_nhat().is_some(), "worker đọc giữa lúc in không phải lúc rảnh");
        t.ghi_may_in(MaSuCo::BinhThuong, None, true);
        assert_eq!(t.dai_moi_nhat(), None, "máy in bình thường lúc rảnh → backend sẽ tự gửi lại → tắt dải");
    }

    /// pb4, nửa còn lại: dải `KhongRo` (hoá đơn còn chờ trong máy in) KHÔNG tắt
    /// vì máy in báo bình thường — tắt khi xác nhận in / job sau in xong / Đã hiểu.
    #[test]
    fn pb4_dai_khong_ro_giu_qua_100_lan_binh_thuong_roi_tat_dung_cach() {
        let mut t = TrangThaiChung::default();
        t.ghi_ket_qua_job("j1", "INV_1", "khong_ro", Some(MaSuCo::HetGiay));
        for _ in 0..100 {
            t.ghi_may_in(MaSuCo::BinhThuong, None, true);
        }
        assert!(t.dai_moi_nhat().is_some());
        t.xac_nhan_in_sau("j-khac");
        assert!(t.dai_moi_nhat().is_some(), "xác nhận job khác không tắt dải này");
        t.xac_nhan_in_sau("j1");
        assert_eq!(t.dai_moi_nhat(), None);

        t.ghi_ket_qua_job("j2", "INV_2", "khong_ro", Some(MaSuCo::KetGiay));
        t.ghi_ket_qua_job("j3", "INV_3", "da_in", None);
        assert_eq!(t.dai_moi_nhat(), None, "job sau in xong → tắt");

        t.ghi_ket_qua_job("j4", "INV_4", "khong_ro", Some(MaSuCo::KhongXacNhan));
        t.da_hieu();
        assert_eq!(t.dai_moi_nhat(), None, "NV bấm Đã hiểu → tắt");
    }

    #[test]
    fn da_hieu_an_su_co_may_in_toi_khi_ma_doi() {
        let mut t = TrangThaiChung::default();
        t.ghi_may_in(MaSuCo::HetGiay, None, true);
        t.da_hieu();
        assert_eq!(t.may_in_da_hieu, Some(MaSuCo::HetGiay));
        t.ghi_may_in(MaSuCo::HetGiay, None, true);
        assert_eq!(t.may_in_da_hieu, Some(MaSuCo::HetGiay));
        t.ghi_may_in(MaSuCo::BinhThuong, None, true);
        assert_eq!(t.may_in_da_hieu, None, "mã đổi → lần sau có sự cố lại hiện");
        t.da_hieu();
        assert_eq!(t.may_in_da_hieu, None, "bình thường thì không có gì để ẩn");
    }

    #[test]
    fn ket_qua_job_bat_tat_canh_bao() {
        let mut t = TrangThaiChung::default();
        t.ghi_su_co_dang_in(MaSuCo::HetGiay, "INV_1", "j1");
        assert_eq!(t.dai_moi_nhat().map(|d| d.loai_dai), Some(LoaiDai::DangTheoDoi));
        t.ghi_ket_qua_job("j1", "INV_1", "khong_ro", Some(MaSuCo::KetGiay));
        assert_eq!(t.dai_moi_nhat().map(|d| (d.loai_dai, d.ma)), Some((LoaiDai::KhongRo, Some(MaSuCo::KetGiay))));
        t.ghi_ket_qua_job("j2", "INV_2", "loi", None);
        assert_eq!(t.dai_moi_nhat().map(|d| (d.loai_dai, d.ma, d.so_hoa_don.as_str())), Some((LoaiDai::Loi, None, "INV_2")));
        t.ghi_ket_qua_job("j3", "INV_3", "da_in", None);
        assert_eq!(t.dai_moi_nhat(), None);
    }

    /// R-M: hoá đơn B in xong chỉ tắt dải của hoá đơn A khi dải A là sự cố
    /// MÁY IN hoặc loại `Loi`; dải "chưa xác nhận được" của A giữ tới "Đã hiểu".
    #[test]
    fn r_m_job_sau_in_xong_khong_tat_dai_chua_xac_nhan_cua_job_khac() {
        for ma in [MaSuCo::KhongXacNhan, MaSuCo::LoiSumatra, MaSuCo::LoiPdf] {
            let mut t = TrangThaiChung::default();
            t.ghi_ket_qua_job("jA", "INV_A", "khong_ro", Some(ma));
            t.ghi_ket_qua_job("jB", "INV_B", "da_in", None);
            assert_eq!(t.dai_moi_nhat().map(|d| d.job_id.as_str()), Some("jA"), "{:?}: B in xong không nói gì về A", ma);
            t.da_hieu();
            assert_eq!(t.dai_moi_nhat(), None);
        }
        // sự cố máy in / loại Loi của A → B in xong là tắt
        for (tt, ma) in [("khong_ro", Some(MaSuCo::HetGiay)), ("loi", Some(MaSuCo::LoiPdf)), ("loi", None)] {
            let mut t = TrangThaiChung::default();
            t.ghi_ket_qua_job("jA", "INV_A", tt, ma);
            t.ghi_ket_qua_job("jB", "INV_B", "da_in", None);
            assert_eq!(t.dai_moi_nhat(), None, "{} {:?}", tt, ma);
        }
        // dải của CHÍNH job đó thì in xong là tắt
        let mut t = TrangThaiChung::default();
        t.ghi_ket_qua_job("jA", "INV_A", "khong_ro", Some(MaSuCo::KhongXacNhan));
        t.ghi_ket_qua_job("jA", "INV_A", "da_in", None);
        assert_eq!(t.dai_moi_nhat(), None);
    }

    /// T6: dải "báo quản lý kiểm" của A KHÔNG bị B đè rồi mất khi B in xong;
    /// mỗi dải tự tắt theo luật của CHÍNH nó; "Đã hiểu" tắt dải đang hiện.
    #[test]
    fn t6_nhieu_dai_moi_dai_tu_tat_theo_luat_cua_no() {
        let mut t = TrangThaiChung::default();
        t.ghi_ket_qua_job("jA", "INV_A", "khong_ro", Some(MaSuCo::KhongXacNhan));
        t.ghi_su_co_dang_in(MaSuCo::HetGiay, "INV_B", "jB");
        assert_eq!(t.dai_moi_nhat().map(|d| d.job_id.as_str()), Some("jB"), "dải mới nhất hiện");
        assert_eq!(t.so_canh_bao_khac(), 1);
        t.ghi_ket_qua_job("jB", "INV_B", "loi", Some(MaSuCo::HetGiay));
        assert_eq!(t.dai_jobs.len(), 2, "B thay dải CỦA NÓ, không đè A");
        t.ghi_ket_qua_job("jB", "INV_B", "da_in", None);
        assert_eq!(t.dai_moi_nhat().map(|d| d.job_id.as_str()), Some("jA"), "A còn nguyên sau khi B in xong");
        // Loi của C tắt khi máy in rảnh báo bình thường; A giữ
        t.ghi_ket_qua_job("jC", "INV_C", "loi", Some(MaSuCo::Offline));
        t.ghi_may_in(MaSuCo::BinhThuong, None, true);
        assert_eq!(t.dai_jobs.iter().map(|d| d.job_id.as_str()).collect::<Vec<_>>(), vec!["jA"]);
        // "Đã hiểu" tắt DẢI ĐANG HIỆN, dải kế tiếp hiện lên
        t.ghi_ket_qua_job("jD", "INV_D", "khong_ro", Some(MaSuCo::KetGiay));
        t.da_hieu();
        assert_eq!(t.dai_moi_nhat().map(|d| d.job_id.as_str()), Some("jA"));
        t.da_hieu();
        assert_eq!(t.dai_moi_nhat(), None);
    }

    #[test]
    fn kiem_cuoi_dai_gop_theo_so_hoa_don_luot_thu_lai_khong_day_mat_dai_khac() {
        let mut t = TrangThaiChung::default();
        // A: "báo quản lý kiểm" (khong_xac_nhan) — phải còn tới khi NV bấm "Đã hiểu".
        t.ghi_ket_qua_job("jA", "INV_A", "khong_ro", Some(MaSuCo::KhongXacNhan));
        // INV_2 bị gửi thử lại 6 lần (mỗi lần id job MỚI), lần nào cũng hết giấy.
        for i in 0..6 {
            t.ghi_ket_qua_job(&format!("j2-{i}"), "INV_2", "loi", Some(MaSuCo::HetGiay));
        }
        assert_eq!(t.dai_jobs.len(), 2, "một dải cho INV_2, dải của A còn nguyên");
        assert!(t.dai_jobs.iter().any(|d| d.so_hoa_don == "INV_A"));
        assert_eq!(t.so_canh_bao_khac(), 1);
    }

    #[test]
    fn t6_tran_5_dai_bo_cu_nhat_va_dem_canh_bao_khac() {
        let mut t = TrangThaiChung::default();
        for i in 0..7 {
            t.ghi_ket_qua_job(&format!("j{}", i), &format!("INV_{}", i), "khong_ro", Some(MaSuCo::KhongXacNhan));
        }
        assert_eq!(t.dai_jobs.len(), SO_DAI_TOI_DA);
        assert_eq!(t.dai_jobs[0].job_id, "j2", "bỏ dải CŨ NHẤT");
        assert_eq!(t.so_canh_bao_khac(), 4);
        // dải máy in khác mã → đếm thêm; cùng mã với dải đang hiện → không
        t.ghi_may_in(MaSuCo::Offline, None, true);
        assert_eq!(t.so_canh_bao_khac(), 5);
        t.ghi_ket_qua_job("j9", "INV_9", "khong_ro", Some(MaSuCo::Offline));
        assert_eq!(t.so_canh_bao_khac(), 4);
        // dải máy in đang hiện (không có dải hoá đơn) + "Đã hiểu" → ẩn tới khi mã đổi
        let mut t = TrangThaiChung::default();
        t.ghi_may_in(MaSuCo::HetGiay, None, true);
        assert_eq!(t.dai_hien(), DaiHien::MayIn(MaSuCo::HetGiay));
        t.da_hieu();
        assert_eq!(t.dai_hien(), DaiHien::KhongCo);
        // dải vàng (mực yếu) của hoá đơn nhường dải đỏ của máy in
        let mut t = TrangThaiChung::default();
        t.ghi_ket_qua_job("j1", "INV_1", "khong_ro", Some(MaSuCo::HetMuc));
        t.ghi_may_in(MaSuCo::Offline, None, true);
        assert_eq!(t.dai_hien(), DaiHien::MayIn(MaSuCo::Offline));
        t.da_hieu();
        assert_eq!(t.dai_moi_nhat().map(|d| d.job_id.as_str()), Some("j1"), "chỉ tắt dải đang hiện");
    }

    /// T9: dải "đã in k/n bản" không tắt khi hoá đơn khác in xong (bản thiếu vẫn thiếu).
    #[test]
    fn t9_dai_thieu_ban_khong_tat_khi_job_khac_in_xong() {
        let mut t = TrangThaiChung::default();
        t.ghi_ket_qua("jA", "INV_A", "khong_ro", Some(MaSuCo::HetGiay), true, Some((1, 2)));
        t.ghi_ket_qua_job("jB", "INV_B", "da_in", None);
        let d = t.dai_moi_nhat().expect("dải thiếu bản còn");
        assert_eq!((d.job_id.as_str(), d.ban_da_in), ("jA", Some((1, 2))));
        t.ghi_may_in(MaSuCo::BinhThuong, None, true);
        assert!(t.dai_moi_nhat().is_some());
        // loi không bao giờ mang ban_da_in
        t.ghi_ket_qua("jC", "INV_C", "loi", Some(MaSuCo::HetGiay), false, Some((1, 2)));
        assert_eq!(t.dai_moi_nhat().map(|d| d.ban_da_in), Some(None));
    }

    /// T7: bấm Lưu giữ "In gần đây" + dải; quên kết nối/máy in cũ.
    #[test]
    fn t7_doi_cau_hinh_giu_job_va_dai_quen_ket_noi() {
        let mut t = TrangThaiChung {
            da_noi: true,
            server_ban_cu: true,
            tu_choi_ket_noi: Some("x".into()),
            thong_bao_cuoi: Some("y".into()),
            ..Default::default()
        };
        t.ghi_may_in(MaSuCo::HetGiay, None, true);
        t.da_hieu();
        t.them_job(log("1", "khong_ro", "10:00:00"));
        t.ghi_ket_qua_job("j1", "INV_1", "khong_ro", Some(MaSuCo::KetGiay));
        t.doi_cau_hinh();
        assert_eq!(t.jobs.len(), 1);
        assert_eq!(t.dai_jobs.len(), 1);
        assert!(!t.da_noi && !t.server_ban_cu && t.tu_choi_ket_noi.is_none() && t.thong_bao_cuoi.is_none());
        assert_eq!((t.may_in.clone(), t.may_in_da_hieu), (None, None), "máy in mới: lần đọc sau là 'đổi' → gửi ngay");
    }

    /// R-C: theo dõi tiếp mất dấu → dải bật lại "chưa xác nhận" dù NV đã tắt.
    #[test]
    fn r_c_mat_dau_bat_lai_dai_chua_xac_nhan() {
        let mut t = TrangThaiChung::default();
        t.them_job(JobLog { job_id: "j1".into(), trang_thai: "khong_ro".into(), loai: Some(MaSuCo::HetGiay), ..Default::default() });
        t.ghi_ket_qua_job("j1", "INV_1", "khong_ro", Some(MaSuCo::HetGiay));
        t.da_hieu();
        t.mat_dau_job("j1", "INV_1");
        let d = t.dai_moi_nhat().expect("dải phải bật lại");
        assert_eq!((d.loai_dai, d.ma, d.so_hoa_don.as_str()), (LoaiDai::KhongRo, Some(MaSuCo::KhongXacNhan), "INV_1"));
        assert_eq!(t.jobs[0].loai, Some(MaSuCo::KhongXacNhan));
    }

    #[test]
    fn theo_doi_tiep_xac_nhan_va_mat_dau_cap_nhat_dong_in_gan_day() {
        let mut t = TrangThaiChung::default();
        t.them_job(JobLog { job_id: "j1".into(), trang_thai: "khong_ro".into(), loai: Some(MaSuCo::HetGiay), ..Default::default() });
        t.them_job(JobLog { job_id: "j2".into(), trang_thai: "khong_ro".into(), loai: Some(MaSuCo::KetGiay), ..Default::default() });
        t.ghi_ket_qua_job("j2", "INV_2", "khong_ro", Some(MaSuCo::KetGiay));
        t.xac_nhan_in_sau("j1");
        let j1 = t.jobs.iter().find(|j| j.job_id == "j1").unwrap();
        assert!(j1.trang_thai == "da_in" && j1.sau_khac_phuc);
        assert!(t.dai_moi_nhat().is_some(), "dải của j2 không bị tắt nhầm");
        t.mat_dau_job("j2", "INV_2");
        let j2 = t.jobs.iter().find(|j| j.job_id == "j2").unwrap();
        assert_eq!((j2.trang_thai.as_str(), j2.loai), ("khong_ro", Some(MaSuCo::KhongXacNhan)));
        assert_eq!(t.dai_moi_nhat().map(|d| d.ma), Some(Some(MaSuCo::KhongXacNhan)));
    }

    #[test]
    fn print_job_id_tu_job_id() {
        assert_eq!(print_job_id("cmg0abc123-1790000000123"), Some("cmg0abc123"));
        assert_eq!(print_job_id("a-b-1790000000123"), Some("a-b"));
        assert_eq!(print_job_id("token_INV_2026-17900"), None);
        assert_eq!(print_job_id("-1790000000123"), None);
        assert_eq!(print_job_id("abc"), None);
    }

    /// Huỷ từ app: dòng "Chờ giấy" cũ của cùng hoá đơn nhường chỗ cho "Đã huỷ".
    #[test]
    fn ghi_da_huy_thay_dong_cu_cung_hoa_don() {
        let mut t = TrangThaiChung::default();
        t.them_job(JobLog { job_id: "p1-1790000000001".into(), so_hoa_don: "INV/1".into(), trang_thai: job::LOI.into(), ..Default::default() });
        t.them_job(JobLog { job_id: "p2-1790000000002".into(), so_hoa_don: "INV/2".into(), trang_thai: job::DA_IN.into(), ..Default::default() });
        t.ghi_da_huy("p1", "INV/1", None, "18:50:00".into());
        assert_eq!(t.jobs.len(), 2);
        assert_eq!((t.jobs[0].so_hoa_don.as_str(), t.jobs[0].trang_thai.as_str()), ("INV/1", job::DA_HUY));
        assert_eq!(t.jobs[1].so_hoa_don, "INV/2");
    }
}
