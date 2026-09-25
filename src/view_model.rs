// SPDX-License-Identifier: AGPL-3.0-or-later
//! Logic THUẦN map trạng thái/config → dữ liệu hiển thị. Tách khỏi Slint để test
//! không cần render. ui.rs gọi build_view_model rồi bơm vào Slint properties.
//!
//! LUẬT CỦA DẢI CẢNH BÁO (R1, giám sát 25/09): KHÔNG câu nào bảo NV "in lại".
//! Bản trước ghi "nạp giấy rồi in lại" / "chưa thì in lại tay": NV in tay đúng
//! lúc job còn chờ trong máy in (hoặc backend đang giữ để tự gửi lại) = HAI tờ.
//! Câu viết theo KẾT QUẢ, và luôn nói rõ ai in lại: hệ thống, không phải NV.
use crate::config::Config;
use crate::job;
use crate::printing;
use crate::state::{DaiHien, DaiJob, JobLog, LoaiDai, TrangThaiChung};
use crate::su_co::{MaSuCo, MucDo};
use std::time::{Duration, Instant};

pub struct JobRow {
    /// Dòng chính: "INV_2026_030045 · Anh_Loc", hoặc id job đã cắt token.
    pub so_hoa_don: String,
    pub khach: String,
    /// "Đã in" / "Lỗi — sẽ tự in lại: <nhãn>" / "Đang chờ trong máy in: <nhãn>"
    /// / "Không rõ: <nhãn>" / "Đã in (sau khi khắc phục)".
    pub badge: String,
    pub da_in: bool,
    /// Không rõ / đang chờ trong máy in — tô màu riêng (vàng), khác "Lỗi" (đỏ).
    pub khong_ro: bool,
    /// Đang xử lý (gửi xuống / chờ in ra) — tô xanh dương.
    pub dang_xu_ly: bool,
    pub luc: String,
}

/// Dải cảnh báo đầu cửa sổ: tiêu đề (dòng đậm) + dòng dưới.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanhBao {
    /// Mã §1 của dải (None = lỗi không rõ mã).
    pub ma: Option<MaSuCo>,
    /// Vd "⚠ Hết giấy — hoá đơn INV_2026_030045 chưa in". Cũng là KHOÁ so
    /// "cảnh báo mới" (`nen_bat_cua_so`, icon khay).
    pub tieu_de: String,
    /// Vd "Nạp giấy vào khay. Hệ thống sẽ TỰ gửi in lại khi máy in hết lỗi — KHÔNG in tay."
    pub chi_tiet: String,
    /// Mức `loi` (đỏ, nháy khay, tự mở cửa sổ); `false` = cảnh báo (vàng, im).
    pub loi: bool,
}

pub struct ViewModel {
    pub trang_thai_text: String,
    pub da_noi: bool,
    pub server: String,
    pub may_in: String,
    pub jobs: Vec<JobRow>,
    pub canh_bao: Option<CanhBao>,
    /// Dòng nhỏ (không phải dải đỏ) khi server không gửi `cau-hinh` (R12).
    pub thong_bao_phu: Option<String>,
}

/// Dòng nhỏ khi server là bản cũ (R12).
pub const CHU_SERVER_BAN_CU: &str = "Server ZaloCRM bản cũ — cần cập nhật server trước khi dùng app này";

/// Dòng nhỏ khi server từ chối kết nối (R-I).
pub fn chu_tu_choi_ket_noi(ly_do: &str) -> String {
    if ly_do.contains("unauthorized") {
        "Server từ chối token máy in (sai hoặc đã bị thu hồi) — kiểm tra token trong Cấu hình".to_string()
    } else {
        format!("Server từ chối kết nối: {}", ly_do)
    }
}

fn nhan_va_viec(ma: Option<MaSuCo>) -> (&'static str, &'static str) {
    match ma {
        Some(m) => (m.nhan(), m.huong_dan()),
        None => ("Không in được", "Báo kỹ thuật"),
    }
}

/// Nhãn trạng thái một job ở "In gần đây" (R1).
///
/// Bản trước chỉ có "Đã in"/"Lỗi" — "không rõ" cũng hiện thành "Lỗi" (handoff
/// §13.3): NV tưởng hệ thống sẽ tự in lại, thực ra không ai in lại cả. Nay
/// "Lỗi" nói luôn "sẽ tự in lại", còn hoá đơn kẹt trong máy in thì "đang chờ".
///
/// T4 (giám sát vòng 3): "sẽ tự in lại" CHỈ với mã không tiêu lượt thử
/// (`MaSuCo::khong_tieu_luot`) — mã khác backend thử vài lần rồi `that_bai`.
pub fn nhan_job(trang_thai: &str, loai: Option<MaSuCo>, sau_khac_phuc: bool) -> String {
    let dau = match trang_thai {
        job::DA_IN if sau_khac_phuc => return "Đã in (sau khi khắc phục)".into(),
        job::DA_IN => return "Đã in".into(),
        job::DANG_GUI => return "Đang gửi xuống máy in…".into(),
        job::CHO_MAY_IN => return "Đã gửi xuống máy in — đang chờ in ra…".into(),
        // Từ chối gửi vì khay trống (0.2.5): chưa gửi gì, có giấy là tự in — nói
        // thẳng việc cần làm, không "Lỗi"/"Không rõ".
        job::LOI if loai == Some(MaSuCo::HetGiay) => return "Chờ giấy — nạp giấy vào khay là tự in".into(),
        job::KHONG_RO if loai.is_some_and(MaSuCo::la_su_co_may_in) => "Đang chờ trong máy in",
        job::KHONG_RO => "Không rõ",
        _ if loai.is_some_and(MaSuCo::khong_tieu_luot) => "Lỗi — sẽ tự in lại",
        _ => "Lỗi — hệ thống thử lại",
    };
    match loai {
        Some(ma) => format!("{}: {}", dau, ma.nhan()),
        None => dau.into(),
    }
}

fn dong_job(j: &JobLog) -> JobRow {
    let so_hoa_don = match &j.khach {
        Some(k) if !k.is_empty() => format!("{} · {}", j.so_hoa_don, k),
        _ => j.so_hoa_don.clone(),
    };
    JobRow {
        so_hoa_don,
        khach: j.khach.clone().unwrap_or_default(),
        badge: match j.ban_da_in {
            // T9: in thiếu bản — nói đúng số bản, không "đang chờ trong máy in".
            Some((k, n)) if j.trang_thai == job::KHONG_RO => printing::chu_thieu_ban(k, n),
            _ => nhan_job(&j.trang_thai, j.loai, j.sau_khac_phuc),
        },
        da_in: j.trang_thai == job::DA_IN,
        khong_ro: j.trang_thai == job::KHONG_RO || (j.trang_thai == job::LOI && j.loai == Some(MaSuCo::HetGiay)),
        dang_xu_ly: j.trang_thai == job::DANG_GUI || j.trang_thai == job::CHO_MAY_IN,
        luc: j.luc.clone(),
    }
}

/// Câu dòng dưới cho `loi` mà backend TIÊU LƯỢT thử (T4): không hứa "TỰ in
/// lại" — quá 5 lượt backend báo thất bại.
pub const CHU_THU_LAI_VAI_LAN: &str = "Hệ thống sẽ thử gửi in lại vài lần; nếu vẫn lỗi sẽ báo thất bại — KHÔNG in tay.";

/// Dải cho kết quả `loi`: app đã tự xoá job khỏi hàng đợi (hoặc chưa gửi
/// xuống), backend gửi lại.
///
/// T4 (giám sát vòng 3): chỉ hứa "Hệ thống sẽ TỰ gửi in lại" với mã KHÔNG tiêu
/// lượt thử (`MaSuCo::khong_tieu_luot` — backend giữ hoá đơn chờ máy hết lỗi).
/// Mã tiêu lượt (`loi_may_in` của riêng một job, `loi_pdf`, `loi_sumatra`,
/// không mã): backend thử vài lần rồi `that_bai` — hứa "tự in lại" là nói sai.
pub fn dai_loi(ma: Option<MaSuCo>, so_hoa_don: &str) -> CanhBao {
    let (nhan, viec) = nhan_va_viec(ma);
    let sau = match ma {
        Some(MaSuCo::KhongTimThayMayIn) => "Hệ thống sẽ TỰ gửi in lại sau khi chọn đúng máy in — KHÔNG in tay.".to_string(),
        Some(m) if m.khong_tieu_luot() => "Hệ thống sẽ TỰ gửi in lại khi máy in hết lỗi — KHÔNG in tay.".to_string(),
        _ => CHU_THU_LAI_VAI_LAN.to_string(),
    };
    CanhBao {
        ma,
        tieu_de: format!("⚠ {} — hoá đơn {} chưa in", nhan, so_hoa_don),
        chi_tiet: format!("{}. {}", viec, sau),
        loi: ma.is_none_or(|m| m.muc() == MucDo::Loi),
    }
}

/// Dải khi copies > 1 mà chỉ in được k/n bản (T9): bản còn lại đã gỡ khỏi
/// hàng đợi, backend nhận `khong_ro` (không gửi lại — gửi lại là thừa tờ của
/// bản đã ra). Nói đúng chuyện đó, không "có thể đang nằm trong máy in".
pub fn dai_thieu_ban(ma: Option<MaSuCo>, so_hoa_don: &str, da_in: u32, tong: u32) -> CanhBao {
    let viec = match ma.filter(|m| m.la_su_co_may_in()) {
        Some(m) => format!("{}: {}. ", m.nhan(), m.huong_dan()),
        None => String::new(),
    };
    CanhBao {
        ma,
        tieu_de: format!("⚠ Hoá đơn {}: {}", so_hoa_don, printing::chu_thieu_ban(da_in, tong)),
        chi_tiet: format!(
            "{}Bản còn lại đã gỡ khỏi hàng đợi — hệ thống KHÔNG tự in bù; cần đủ bản thì báo quản lý.",
            viec
        ),
        loi: true,
    }
}

/// Câu dòng dưới khi hoá đơn có thể đang nằm trong BỘ NHỚ máy in (R-D) — đây
/// là câu DUY NHẤT được nhắc tới "in lại", và chỉ có điều kiện: job đã rời
/// hàng đợi Windows nên không ai theo dõi nó nữa; nạp giấy xong mà vẫn không
/// ra thì nó đã mất thật.
pub const CHU_CO_THE_TRONG_MAY_IN: &str = "Khắc phục xong đợi vài phút — chỉ in lại nếu vẫn không thấy ra.";

/// Dải cho job còn chờ / chưa xác nhận được (`khong_ro`, hoặc đang theo dõi mà
/// đã thấy sự cố).
#[cfg(test)]
pub fn dai_khong_ro(ma: Option<MaSuCo>, so_hoa_don: &str) -> CanhBao {
    dai_khong_ro_theo(ma, so_hoa_don, false)
}

/// Như `dai_khong_ro`; `ngoai_hang_doi` = job KHÔNG còn trong hàng đợi Windows
/// (`conTrongHangDoi:false`, R-D) — với sự cố máy in, hoá đơn "có thể đang nằm
/// trong máy in" chứ không phải "đang chờ" (không ai theo dõi để nó tự in).
pub fn dai_khong_ro_theo(ma: Option<MaSuCo>, so_hoa_don: &str, ngoai_hang_doi: bool) -> CanhBao {
    match ma.filter(|m| m.la_su_co_may_in()) {
        Some(m) if ngoai_hang_doi => CanhBao {
            ma,
            tieu_de: format!("⚠ {} — hoá đơn {} có thể đang nằm trong máy in", m.nhan(), so_hoa_don),
            chi_tiet: format!("{}. {}", m.huong_dan(), CHU_CO_THE_TRONG_MAY_IN),
            loi: m.muc() == MucDo::Loi,
        },
        Some(m) => CanhBao {
            ma,
            tieu_de: format!("⚠ {} — hoá đơn {} đang chờ trong máy in", m.nhan(), so_hoa_don),
            chi_tiet: format!("{}. Hoá đơn sẽ TỰ in ra sau khi khắc phục — KHÔNG in lại.", m.huong_dan()),
            loi: m.muc() == MucDo::Loi,
        },
        None => {
            let m = ma.unwrap_or(MaSuCo::KhongXacNhan);
            CanhBao {
                ma: Some(m),
                tieu_de: format!("Chưa xác nhận được hoá đơn {} đã in", so_hoa_don),
                chi_tiet: format!(
                    "{}. Nếu 5 phút không thấy ra, báo quản lý kiểm trên ZaloCRM (Cài đặt › Máy in) — KHÔNG tự in lại.",
                    m.huong_dan()
                ),
                loi: true,
            }
        }
    }
}

/// Dải cho sự cố máy in lúc rảnh (không gắn hoá đơn nào).
pub fn dai_may_in(ma: MaSuCo, ten_may_in: &str) -> CanhBao {
    let chi_tiet = if ma.chan_in() {
        format!("{}. Hoá đơn gửi tới sẽ chờ và tự in khi máy in hết lỗi.", ma.huong_dan())
    } else {
        // Mực yếu: máy vẫn in — nói "sẽ chờ" là sai.
        format!("{}. Máy in vẫn in bình thường.", ma.huong_dan())
    };
    CanhBao {
        ma: Some(ma),
        tieu_de: format!("⚠ {} (máy in \"{}\")", ma.nhan(), ten_may_in),
        chi_tiet,
        loi: ma.muc() == MucDo::Loi,
    }
}

fn dai_cua_job(d: &DaiJob) -> CanhBao {
    match (d.loai_dai, d.ban_da_in) {
        (LoaiDai::KhongRo, Some((k, n))) => dai_thieu_ban(d.ma, &d.so_hoa_don, k, n),
        (LoaiDai::Loi, _) => dai_loi(d.ma, &d.so_hoa_don),
        (LoaiDai::KhongRo | LoaiDai::DangTheoDoi, _) => dai_khong_ro_theo(d.ma, &d.so_hoa_don, d.ngoai_hang_doi),
    }
}

/// Cảnh báo cần hiện, nếu có (quyết ở `TrangThaiChung::dai_hien`): dải của
/// hoá đơn MỚI NHẤT có chuyện (nói rõ hoá đơn nào, ai in lại) THẮNG dải sự cố
/// máy in — trừ khi dải hoá đơn chỉ là cảnh báo vàng mà máy in đang báo lỗi
/// chặn in. Còn cảnh báo khác chưa xử lý (T6) → dòng dưới thêm "(+N cảnh báo
/// khác)"; tiêu đề giữ nguyên (nó là khoá "cảnh báo mới").
pub fn canh_bao(t: &TrangThaiChung, ten_may_in: &str) -> Option<CanhBao> {
    let mut cb = match t.dai_hien() {
        DaiHien::Job(d) => dai_cua_job(d),
        DaiHien::MayIn(m) => dai_may_in(m, ten_may_in),
        DaiHien::KhongCo => return None,
    };
    let khac = t.so_canh_bao_khac();
    if khac > 0 {
        cb.chi_tiet = format!("{} (+{} cảnh báo khác)", cb.chi_tiet, khac);
    }
    Some(cb)
}

/// Khoảng nghỉ tối thiểu giữa hai lần TỰ MỞ cửa sổ — máy in chập chờn
/// (WSD báo offline/online liên tục, handoff §14) không được làm cửa sổ bật
/// lên mỗi 20 giây trước mặt NV. Trong khoảng nghỉ, dải cảnh báo + icon khay
/// vẫn cập nhật bình thường; chỉ không giật cửa sổ lên nữa.
pub const KHOANG_NGHI_BAT_CUA_SO: Duration = Duration::from_secs(600);

/// Có nên tự mở cửa sổ + nháy cho NV thấy không. Chỉ khi xuất hiện một cảnh
/// báo mức `loi` MỚI (tiêu đề khác cảnh báo đang hiện) và đã qua khoảng nghỉ.
/// App chỉ sống ở khay — Windows 10/11 mặc định giấu icon khay mới vào "^",
/// nên nháy icon thôi thì NV có thể không bao giờ thấy.
pub fn nen_bat_cua_so(
    truoc: Option<&str>,
    nay: Option<&CanhBao>,
    lan_bat_cuoi: Option<Instant>,
    bay_gio: Instant,
) -> bool {
    let Some(cb) = nay else { return false };
    if !cb.loi || truoc == Some(cb.tieu_de.as_str()) {
        return false;
    }
    lan_bat_cuoi.is_none_or(|t| bay_gio.saturating_duration_since(t) >= KHOANG_NGHI_BAT_CUA_SO)
}

pub fn build_view_model(cfg: &Config, t: &TrangThaiChung) -> ViewModel {
    ViewModel {
        trang_thai_text: if t.da_noi { "Đã kết nối".into() } else { "Mất kết nối".into() },
        da_noi: t.da_noi,
        server: cfg.server_url.clone(),
        may_in: format!("{} · {}", cfg.printer_name, cfg.paper_size),
        jobs: t.jobs.iter().map(dong_job).collect(),
        canh_bao: canh_bao(t, &cfg.printer_name),
        thong_bao_phu: match &t.tu_choi_ket_noi {
            Some(ly_do) => Some(chu_tu_choi_ket_noi(ly_do)),
            None => t.server_ban_cu.then(|| CHU_SERVER_BAN_CU.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::{JobLog, TrangThaiChung};

    fn cfg() -> Config {
        Config { server_url: "zalocrm.incokit.com".into(), token: "".into(),
                 printer_name: "HP 4003".into(),
                 tray: "tray-1".into(), paper_size: "A5".into() }
    }

    fn dong(so: &str, khach: Option<&str>, trang_thai: &str, loai: Option<MaSuCo>) -> JobLog {
        JobLog { so_hoa_don: so.into(), khach: khach.map(Into::into), trang_thai: trang_thai.into(), loai,
                 luc: "10:00".into(), ..Default::default() }
    }

    #[test]
    fn da_noi_ra_text_xanh() {
        let t = TrangThaiChung { da_noi: true, ..Default::default() };
        let vm = build_view_model(&cfg(), &t);
        assert_eq!(vm.trang_thai_text, "Đã kết nối");
        assert!(vm.da_noi);
        assert_eq!(vm.may_in, "HP 4003 · A5");
        assert_eq!(vm.thong_bao_phu, None);
    }

    #[test]
    fn mat_noi_ra_text_do() {
        let t = TrangThaiChung { da_noi: false, ..Default::default() };
        assert_eq!(build_view_model(&cfg(), &t).trang_thai_text, "Mất kết nối");
    }

    #[test]
    fn server_ban_cu_hien_dong_nho_khong_phai_dai_do() {
        let t = TrangThaiChung { da_noi: true, server_ban_cu: true, ..Default::default() };
        let vm = build_view_model(&cfg(), &t);
        assert_eq!(vm.thong_bao_phu.as_deref(), Some(CHU_SERVER_BAN_CU));
        assert!(vm.canh_bao.is_none());
    }

    #[test]
    fn job_map_badge_dung() {
        let t = TrangThaiChung { da_noi: true, jobs: vec![
            dong("INV/1", Some("Anh A"), "da_in", None),
            dong("INV/2", None, "loi", None),
        ], ..Default::default() };
        let vm = build_view_model(&cfg(), &t);
        assert_eq!(vm.jobs.len(), 2);
        assert_eq!(vm.jobs[0].badge, "Đã in"); assert!(vm.jobs[0].da_in);
        assert_eq!(vm.jobs[0].khach, "Anh A");
        assert_eq!(vm.jobs[1].badge, "Lỗi — hệ thống thử lại"); assert!(!vm.jobs[1].da_in);
        assert_eq!(vm.jobs[1].khach, "");
    }

    #[test]
    fn nhan_job_theo_ket_qua() {
        assert_eq!(nhan_job("da_in", None, false), "Đã in");
        assert_eq!(nhan_job("da_in", Some(MaSuCo::HetGiay), false), "Đã in");
        assert_eq!(nhan_job("da_in", None, true), "Đã in (sau khi khắc phục)");
        // 0.2.5: từ chối gửi vì khay trống — nói việc cần làm, không "Lỗi".
        assert_eq!(nhan_job("loi", Some(MaSuCo::HetGiay), false), "Chờ giấy — nạp giấy vào khay là tự in");
        assert_eq!(nhan_job("dang_gui", None, false), "Đang gửi xuống máy in…");
        assert_eq!(nhan_job("cho_may_in", None, false), "Đã gửi xuống máy in — đang chờ in ra…");
        assert_eq!(nhan_job("loi", Some(MaSuCo::CanXuLy), false), "Lỗi — sẽ tự in lại: Máy in cần người xử lý");
        // T4: mã tiêu lượt thử — không hứa tự in lại
        assert_eq!(nhan_job("loi", Some(MaSuCo::LoiMayIn), false), "Lỗi — hệ thống thử lại: Máy in báo lỗi");
        assert_eq!(nhan_job("loi", None, false), "Lỗi — hệ thống thử lại");
        assert_eq!(nhan_job("khong_ro", Some(MaSuCo::KetGiay), false), "Đang chờ trong máy in: Kẹt giấy");
        assert_eq!(nhan_job("khong_ro", Some(MaSuCo::KhongXacNhan), false),
            "Không rõ: Đã gửi máy in nhưng không xác nhận được đã in");
        assert_eq!(nhan_job("khong_ro", Some(MaSuCo::LoiSumatra), false), "Không rõ: Không gọi được / SumatraPDF lỗi");
        assert_eq!(nhan_job("khong_ro", None, false), "Không rõ");
    }

    /// Bug handoff §13.3: "không rõ" từng hiện thành "Lỗi".
    #[test]
    fn khong_ro_khong_bao_gio_hien_thanh_loi() {
        let t = TrangThaiChung { jobs: vec![
            JobLog { luc: "10:02:03".into(), ..dong("INV_2026_030045", Some("Anh_Loc"), "khong_ro", Some(MaSuCo::KhongXacNhan)) },
        ], ..Default::default() };
        let vm = build_view_model(&cfg(), &t);
        let r = &vm.jobs[0];
        assert!(r.badge.starts_with("Không rõ"), "{}", r.badge);
        assert!(r.khong_ro && !r.da_in);
        assert_eq!(r.so_hoa_don, "INV_2026_030045 · Anh_Loc");
        assert_eq!(r.luc, "10:02:03");
    }

    fn dai(loai_dai: LoaiDai, ma: Option<MaSuCo>) -> TrangThaiChung {
        TrangThaiChung {
            dai_jobs: vec![DaiJob {
                loai_dai,
                ma,
                so_hoa_don: "INV_2026_030045".into(),
                job_id: "j".into(),
                ngoai_hang_doi: false,
                ban_da_in: None,
            }],
            ..Default::default()
        }
    }

    /// R1 — bốn câu theo KẾT QUẢ, nguyên văn.
    #[test]
    fn cau_canh_bao_theo_ket_qua() {
        let cb = canh_bao(&dai(LoaiDai::Loi, Some(MaSuCo::HetGiay)), "HP 4003").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_2026_030045 chưa in");
        assert_eq!(cb.chi_tiet, "Nạp giấy vào khay. Hệ thống sẽ TỰ gửi in lại khi máy in hết lỗi — KHÔNG in tay.");
        assert!(cb.loi);

        let cb = canh_bao(&dai(LoaiDai::KhongRo, Some(MaSuCo::KetGiay)), "HP 4003").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Kẹt giấy — hoá đơn INV_2026_030045 đang chờ trong máy in");
        assert_eq!(cb.chi_tiet, "Gỡ giấy kẹt rồi đóng nắp. Hoá đơn sẽ TỰ in ra sau khi khắc phục — KHÔNG in lại.");

        let cb = canh_bao(&dai(LoaiDai::KhongRo, Some(MaSuCo::KhongXacNhan)), "HP 4003").unwrap();
        assert_eq!(cb.tieu_de, "Chưa xác nhận được hoá đơn INV_2026_030045 đã in");
        assert_eq!(cb.chi_tiet,
            "Xem khay giấy. Nếu 5 phút không thấy ra, báo quản lý kiểm trên ZaloCRM (Cài đặt › Máy in) — KHÔNG tự in lại.");

        let t = TrangThaiChung { may_in: Some((MaSuCo::Offline, None)), ..Default::default() };
        let cb = canh_bao(&t, "HP 4003").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Máy in offline / mất kết nối máy in (máy in \"HP 4003\")");
        assert_eq!(cb.chi_tiet, "Bật máy in, kiểm dây mạng/USB. Hoá đơn gửi tới sẽ chờ và tự in khi máy in hết lỗi.");
    }

    /// R1 (NẶNG): không câu nào, ở bất kỳ loại dải nào, với bất kỳ mã nào,
    /// được bảo NV "in lại" — chỉ được nói KHÔNG in lại / hệ thống TỰ in lại.
    #[test]
    fn khong_dai_nao_bao_nv_in_lai() {
        const TAT_CA: [MaSuCo; 12] = [
            MaSuCo::HetGiay, MaSuCo::KetGiay, MaSuCo::Offline, MaSuCo::MoNap, MaSuCo::HetMuc,
            MaSuCo::CanXuLy, MaSuCo::LoiMayIn, MaSuCo::KhongTimThayMayIn, MaSuCo::LoiSumatra,
            MaSuCo::LoiPdf, MaSuCo::KhongXacNhan, MaSuCo::BinhThuong,
        ];
        let mut cac_dai = Vec::new();
        for ma in TAT_CA.into_iter().map(Some).chain([None]) {
            cac_dai.push((true, dai_loi(ma, "X")));
            cac_dai.push((true, dai_khong_ro(ma, "X")));
            cac_dai.push((true, dai_khong_ro_theo(ma, "X", true)));
            cac_dai.push((true, dai_thieu_ban(ma, "X", 1, 2)));
            if let Some(m) = ma {
                cac_dai.push((false, dai_may_in(m, "HP")));
            }
        }
        for (cua_hoa_don, cb) in cac_dai {
            let chu = format!("{} {}", cb.tieu_de, cb.chi_tiet);
            // Bỏ các cụm phủ định/hệ thống, và câu có ĐIỀU KIỆN duy nhất của R-D
            // (hoá đơn có thể trong bộ nhớ máy in — chỉ in lại nếu nạp giấy
            // xong vẫn không ra); phần còn lại không được có "in lại".
            let con = chu
                .replace("KHÔNG in lại", "")
                .replace("KHÔNG tự in lại", "")
                .replace("TỰ gửi in lại", "")
                .replace("thử gửi in lại", "")
                .replace(CHU_CO_THE_TRONG_MAY_IN, "");
            assert!(!con.to_lowercase().contains("in lại"), "{}", chu);
            if cua_hoa_don {
                assert!(
                    chu.contains("KHÔNG in") || chu.contains("KHÔNG tự in") || chu.contains(CHU_CO_THE_TRONG_MAY_IN),
                    "dải hoá đơn phải cấm in tay (hoặc đúng câu có điều kiện R-D): {}",
                    chu
                );
            }
        }
    }

    /// R-D: `khong_ro` có mã máy mà job KHÔNG còn trong hàng đợi — câu nói
    /// "có thể đang nằm trong máy in", chỉ in lại khi khắc phục xong vẫn không ra.
    #[test]
    fn r_d_khong_ro_ngoai_hang_doi_co_the_trong_may_in() {
        let mut t = dai(LoaiDai::KhongRo, Some(MaSuCo::HetGiay));
        t.dai_jobs[0].ngoai_hang_doi = true;
        let cb = canh_bao(&t, "HP").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_2026_030045 có thể đang nằm trong máy in");
        assert_eq!(cb.chi_tiet, "Nạp giấy vào khay. Khắc phục xong đợi vài phút — chỉ in lại nếu vẫn không thấy ra.");
        assert!(cb.loi);
        // không có mã máy (khong_xac_nhan) → câu cũ, dù ngoài hàng đợi
        let cb = dai_khong_ro_theo(Some(MaSuCo::KhongXacNhan), "X", true);
        assert_eq!(cb.tieu_de, "Chưa xác nhận được hoá đơn X đã in");
    }

    /// R-I: server từ chối token → dòng nhỏ nói rõ, thắng dòng "bản cũ".
    #[test]
    fn r_i_tu_choi_ket_noi_hien_dong_nho() {
        let t = TrangThaiChung {
            server_ban_cu: true,
            tu_choi_ket_noi: Some("Received an ConnectError frame: {\"message\":\"unauthorized\"}".into()),
            ..Default::default()
        };
        let vm = build_view_model(&cfg(), &t);
        assert_eq!(
            vm.thong_bao_phu.as_deref(),
            Some("Server từ chối token máy in (sai hoặc đã bị thu hồi) — kiểm tra token trong Cấu hình")
        );
        assert_eq!(chu_tu_choi_ket_noi("db down"), "Server từ chối kết nối: db down");
    }

    /// T4: `loi` chỉ hứa "TỰ gửi in lại" với mã không tiêu lượt thử; mã tiêu
    /// lượt nói "thử lại vài lần; nếu vẫn lỗi sẽ báo thất bại". T5: máy in
    /// không tồn tại → tự in lại SAU KHI chọn đúng máy in.
    #[test]
    fn t4_t5_cau_dai_loi_theo_tieu_luot() {
        for ma in [MaSuCo::HetGiay, MaSuCo::KetGiay, MaSuCo::Offline, MaSuCo::MoNap, MaSuCo::CanXuLy] {
            let cb = dai_loi(Some(ma), "X");
            assert!(cb.chi_tiet.ends_with("Hệ thống sẽ TỰ gửi in lại khi máy in hết lỗi — KHÔNG in tay."), "{:?}: {}", ma, cb.chi_tiet);
        }
        for ma in [Some(MaSuCo::LoiMayIn), Some(MaSuCo::LoiPdf), Some(MaSuCo::LoiSumatra), None] {
            let cb = dai_loi(ma, "X");
            assert!(cb.chi_tiet.ends_with(CHU_THU_LAI_VAI_LAN), "{:?}: {}", ma, cb.chi_tiet);
            assert!(!cb.chi_tiet.contains("TỰ"), "{:?}: không hứa tự in lại", ma);
        }
        let cb = dai_loi(Some(MaSuCo::LoiMayIn), "X");
        assert_eq!(cb.chi_tiet, "Xem màn hình máy in, tắt/bật lại máy in. Hệ thống sẽ thử gửi in lại vài lần; nếu vẫn lỗi sẽ báo thất bại — KHÔNG in tay.");
        let cb = dai_loi(Some(MaSuCo::KhongTimThayMayIn), "INV_1");
        assert_eq!(cb.tieu_de, "⚠ Không tìm thấy máy in trong Windows — hoá đơn INV_1 chưa in");
        assert_eq!(cb.chi_tiet, "Chọn lại máy in trong app. Hệ thống sẽ TỰ gửi in lại sau khi chọn đúng máy in — KHÔNG in tay.");
    }

    /// T9: in thiếu bản — dải + "In gần đây" nói đúng số bản, không "có thể
    /// đang nằm trong máy in".
    #[test]
    fn t9_dai_va_nhan_in_thieu_ban() {
        let mut t = TrangThaiChung::default();
        t.them_job(JobLog { ban_da_in: Some((1, 2)), ..dong("INV_1", None, "khong_ro", Some(MaSuCo::HetGiay)) });
        t.ghi_ket_qua("j", "INV_1", "khong_ro", Some(MaSuCo::HetGiay), true, Some((1, 2)));
        let vm = build_view_model(&cfg(), &t);
        assert_eq!(vm.jobs[0].badge, "Đã in 1/2 bản — bản còn lại CHƯA in");
        let cb = vm.canh_bao.unwrap();
        assert_eq!(cb.tieu_de, "⚠ Hoá đơn INV_1: Đã in 1/2 bản — bản còn lại CHƯA in");
        assert_eq!(cb.chi_tiet, "Hết giấy: Nạp giấy vào khay. Bản còn lại đã gỡ khỏi hàng đợi — hệ thống KHÔNG tự in bù; cần đủ bản thì báo quản lý.");
        assert!(!cb.tieu_de.contains("có thể đang nằm") && cb.loi);
    }

    /// T6: còn cảnh báo khác → "(+N cảnh báo khác)" ở dòng dưới, tiêu đề giữ nguyên.
    #[test]
    fn t6_hien_dai_moi_nhat_kem_so_canh_bao_khac() {
        let mut t = TrangThaiChung::default();
        t.ghi_ket_qua_job("jA", "INV_A", "khong_ro", Some(MaSuCo::KhongXacNhan));
        t.ghi_ket_qua_job("jB", "INV_B", "loi", Some(MaSuCo::HetGiay));
        let cb = canh_bao(&t, "HP").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_B chưa in");
        assert!(cb.chi_tiet.ends_with("KHÔNG in tay. (+1 cảnh báo khác)"), "{}", cb.chi_tiet);
        t.ghi_ket_qua_job("jB", "INV_B", "da_in", None);
        let cb = canh_bao(&t, "HP").unwrap();
        assert_eq!(cb.tieu_de, "Chưa xác nhận được hoá đơn INV_A đã in", "dải của A còn sau khi B in xong");
        assert!(!cb.chi_tiet.contains("cảnh báo khác"));
    }

    #[test]
    fn dang_theo_doi_hien_nhu_dang_cho_trong_may_in() {
        let cb = canh_bao(&dai(LoaiDai::DangTheoDoi, Some(MaSuCo::HetGiay)), "HP").unwrap();
        assert_eq!(cb.tieu_de, "⚠ Hết giấy — hoá đơn INV_2026_030045 đang chờ trong máy in");
    }

    #[test]
    fn loi_khong_phai_may_in_khong_hua_doi_may_in() {
        let cb = dai_loi(Some(MaSuCo::LoiPdf), "X");
        assert_eq!(cb.chi_tiet, "Báo kỹ thuật: file PDF hỏng. Hệ thống sẽ thử gửi in lại vài lần; nếu vẫn lỗi sẽ báo thất bại — KHÔNG in tay.");
        let cb = dai_loi(None, "X");
        assert_eq!(cb.tieu_de, "⚠ Không in được — hoá đơn X chưa in");
        let cb = dai_khong_ro(Some(MaSuCo::LoiSumatra), "X");
        assert_eq!(cb.tieu_de, "Chưa xác nhận được hoá đơn X đã in");
        assert!(cb.chi_tiet.starts_with("Báo kỹ thuật: SumatraPDF lỗi. Nếu 5 phút"));
    }

    #[test]
    fn canh_bao_tu_may_in_hoac_job() {
        let mut t = TrangThaiChung::default();
        assert_eq!(canh_bao(&t, "HP"), None, "chưa biết gì thì không cảnh báo");
        t.may_in = Some((MaSuCo::BinhThuong, None));
        assert_eq!(canh_bao(&t, "HP"), None);
        t.may_in = Some((MaSuCo::HetGiay, None));
        assert_eq!(canh_bao(&t, "HP").map(|c| c.ma), Some(Some(MaSuCo::HetGiay)));
        // máy in báo bình thường nhưng job vừa kẹt giấy → vẫn cảnh báo theo job
        t.may_in = Some((MaSuCo::BinhThuong, None));
        t.dai_jobs = dai(LoaiDai::KhongRo, Some(MaSuCo::KetGiay)).dai_jobs;
        assert_eq!(canh_bao(&t, "HP").map(|c| c.ma), Some(Some(MaSuCo::KetGiay)));
    }

    #[test]
    fn canh_bao_dai_hoa_don_thang_tru_khi_chi_la_vang() {
        let t = TrangThaiChung { may_in: Some((MaSuCo::HetMuc, None)), ..dai(LoaiDai::KhongRo, Some(MaSuCo::HetGiay)) };
        assert_eq!(canh_bao(&t, "HP").map(|c| c.ma), Some(Some(MaSuCo::HetGiay)), "mực yếu không được che mất hết giấy");
        let t = TrangThaiChung { may_in: Some((MaSuCo::HetMuc, None)), ..Default::default() };
        let cb = canh_bao(&t, "HP").unwrap();
        assert!(!cb.loi, "mực yếu là cảnh báo vàng, không nháy");
        assert_eq!(cb.chi_tiet, "Chuẩn bị thay mực. Máy in vẫn in bình thường.");
        let t = TrangThaiChung { may_in: Some((MaSuCo::Offline, None)), ..dai(LoaiDai::KhongRo, Some(MaSuCo::HetGiay)) };
        assert_eq!(canh_bao(&t, "HP").map(|c| c.ma), Some(Some(MaSuCo::HetGiay)), "dải hoá đơn nói rõ hơn");
        let t = TrangThaiChung { may_in: Some((MaSuCo::Offline, None)), ..dai(LoaiDai::KhongRo, Some(MaSuCo::HetMuc)) };
        assert_eq!(canh_bao(&t, "HP").map(|c| c.ma), Some(Some(MaSuCo::Offline)), "dải vàng nhường lỗi máy in");
    }

    #[test]
    fn da_hieu_an_dai_may_in() {
        let mut t = TrangThaiChung { may_in: Some((MaSuCo::HetGiay, None)), ..Default::default() };
        t.da_hieu();
        assert_eq!(canh_bao(&t, "HP"), None);
    }

    #[test]
    fn tu_mo_cua_so_chi_khi_su_co_moi_va_qua_khoang_nghi() {
        let t0 = Instant::now();
        let het_giay = dai_may_in(MaSuCo::HetGiay, "HP");
        assert!(nen_bat_cua_so(None, Some(&het_giay), None, t0));
        assert!(!nen_bat_cua_so(Some(het_giay.tieu_de.as_str()), Some(&het_giay), None, t0), "cảnh báo không đổi thì thôi");
        assert!(!nen_bat_cua_so(None, None, None, t0));
        assert!(!nen_bat_cua_so(None, Some(&dai_may_in(MaSuCo::HetMuc, "HP")), None, t0), "mực yếu không bật cửa sổ");
        // chập chờn: vừa bật xong, tắt rồi lại có → chờ đủ khoảng nghỉ
        assert!(!nen_bat_cua_so(None, Some(&het_giay), Some(t0), t0 + Duration::from_secs(30)));
        assert!(nen_bat_cua_so(None, Some(&het_giay), Some(t0), t0 + KHOANG_NGHI_BAT_CUA_SO));
    }
}
