// SPDX-License-Identifier: AGPL-3.0-or-later
//! Xử lý một job in: parse payload server gửi → giải mã PDF → in → dựng kết quả.
//! Tách thuần (không đụng socket.io) để test được không cần server.

use crate::config::Config;
use crate::su_co::MaSuCo;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

/// Payload server gửi qua event "job": {loai:"in", job:{...}}.
#[derive(Debug, Deserialize)]
pub struct JobEnvelope {
    pub job: JobIn,
}

#[derive(Debug, Deserialize)]
pub struct JobIn {
    pub id: String,
    /// Tên file PDF server dựng sẵn: "AI-INV_2026_030045-<Ten_Khach>-<id>.pdf"
    /// (backend may-in/ten-file-in.ts). Server bản cũ không gửi → None →
    /// `printing::ten_file_in` tự đặt tên như trước. Không bao giờ bắt buộc.
    pub name: Option<String>,
    #[serde(rename = "pdfBase64")]
    pub pdf_base64: String,
    #[serde(rename = "paperSize")]
    pub paper_size: Option<String>,
    pub tray: Option<String>,
    pub copies: Option<u32>,
}

pub const DA_IN: &str = "da_in";
pub const LOI: &str = "loi";
pub const KHONG_RO: &str = "khong_ro";
/// Trạng thái TRUNG GIAN chỉ để hiện trên app ("In gần đây") — không bao giờ
/// gửi backend (0.2.5, chủ yêu cầu 25/09: "lúc gửi xuống máy in không hiển thị").
pub const DANG_GUI: &str = "dang_gui";
/// Job đã rời hàng đợi Windows, app đang chờ máy in xác nhận (trung gian, chỉ app).
pub const CHO_MAY_IN: &str = "cho_may_in";

/// Kết quả emit về server qua event "ket-qua" (hợp đồng v2 §2):
/// `{jobId, trangThai: "da_in"|"loi"|"khong_ro", loiCuoi?, loai?}`.
///
/// `khong_ro` CHỈ được gửi khi backend của kết nối đã báo hỗ trợ — xem `hop_thu_di::CanHoTro::cua_ket_qua`.
#[derive(Debug, Serialize, PartialEq)]
pub struct KetQua {
    #[serde(rename = "jobId")]
    pub job_id: String,
    #[serde(rename = "trangThai")]
    pub trang_thai: String, // DA_IN | LOI | KHONG_RO
    #[serde(rename = "loiCuoi", skip_serializing_if = "Option::is_none")]
    pub loi_cuoi: Option<String>,
    /// Mã §1 khi biết nguyên nhân (`het_giay`, `khong_xac_nhan`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loai: Option<MaSuCo>,
    /// CHỈ với `khong_ro` (R-D, hợp đồng v4 §2): `true` = lúc kết luận job VẪN
    /// trong hàng đợi Windows và đã giao cho theo dõi tiếp (sẽ tự in, app báo
    /// `da_in` trễ); `false` = không còn trong hàng đợi, có thể nằm trong bộ
    /// nhớ máy in. Backend chọn câu cho quản lý theo trường này.
    #[serde(rename = "conTrongHangDoi", skip_serializing_if = "Option::is_none")]
    pub con_trong_hang_doi: Option<bool>,
    /// copies > 1 mà chỉ in được (k, n) bản — bản còn lại đã gỡ khỏi hàng đợi
    /// (T9). CHỈ cho giao diện/nhật ký, không gửi backend (không có trong hợp đồng).
    #[serde(skip)]
    pub ban_da_in: Option<(u32, u32)>,
}

impl KetQua {
    /// Cũng dùng cho `da_in` MUỘN của luồng theo dõi tiếp (R3).
    pub fn da_in(job_id: String) -> Self {
        Self { job_id, trang_thai: DA_IN.into(), loi_cuoi: None, loai: None, con_trong_hang_doi: None, ban_da_in: None }
    }
    fn loi(job_id: String, ly_do: LyDo) -> Self {
        Self { job_id, trang_thai: LOI.into(), loi_cuoi: Some(ly_do.chu), loai: ly_do.loai, con_trong_hang_doi: None, ban_da_in: None }
    }
    fn khong_ro(job_id: String, ly_do: LyDo) -> Self {
        let ban_da_in = ly_do.ban_da_in;
        Self { job_id, trang_thai: KHONG_RO.into(), loi_cuoi: Some(ly_do.chu), loai: ly_do.loai, con_trong_hang_doi: None, ban_da_in }
    }
}

/// Lý do của một kết quả không phải DaIn: câu chữ (đi vào `loiCuoi`) + mã §1
/// nếu biết. Có `From<String>`/`From<&str>` để chỗ không biết mã vẫn viết
/// `KetQuaIn::Loi("…".into())` như trước.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LyDo {
    pub chu: String,
    pub loai: Option<MaSuCo>,
    /// copies > 1: (số bản ĐÃ in, tổng số bản) khi bản sau bị gỡ/không gửi
    /// được mà bản trước đã ra giấy (T9, `printing::sau_ban_da_in`).
    pub ban_da_in: Option<(u32, u32)>,
}

impl LyDo {
    pub fn co_loai(chu: impl Into<String>, loai: MaSuCo) -> Self {
        Self { chu: chu.into(), loai: Some(loai), ban_da_in: None }
    }
}

impl From<String> for LyDo {
    fn from(chu: String) -> Self {
        Self { chu, ..Default::default() }
    }
}

impl From<&str> for LyDo {
    fn from(chu: &str) -> Self {
        Self { chu: chu.to_string(), ..Default::default() }
    }
}

impl std::fmt::Display for LyDo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.loai {
            Some(l) => write!(f, "{} [{}]", self.chu, l.ma()),
            None => f.write_str(&self.chu),
        }
    }
}

/// Kết quả in 3 nhánh — TRỌNG TÂM chống in đôi. Xem thiết kế đầy đủ ở spooler.rs.
#[derive(Debug, Clone, PartialEq)]
pub enum KetQuaIn {
    /// Quan sát trực tiếp job đã in xong (bằng chứng mạnh nhất từ spooler),
    /// hoặc dry-run (test/không có máy in thật).
    DaIn,
    /// CHẮC CHẮN không còn gì của job trong hàng đợi Windows và chưa in tờ nào
    /// — lỗi trước khi in mà job không vào hàng đợi, hoặc đã XOÁ job khỏi hàng
    /// đợi và kiểm lại là hết (§0.1). An toàn để server retry.
    Loi(LyDo),
    /// Không chắc — hoặc chưa từng quan sát được job trong hàng đợi, hoặc đã
    /// bắt đầu in rồi mất dấu/lỗi/timeout mà chưa thấy in xong, hoặc có lỗi mà
    /// KHÔNG xoá được job khỏi hàng đợi. KHÔNG được suy DaIn hay Loi (Loi ở đây
    /// rủi ro in đôi nếu server retry mà tờ giấy thực ra đã/sẽ ra khỏi máy in).
    KhongRo(LyDo),
}

/// Hàm in — tiêm được để test (thật = printing::in_pdf). Trả KetQuaIn (3
/// nhánh) thay vì anyhow::Result — "lỗi" và "không chắc" là hai tình huống
/// XỬ LÝ KHÁC NHAU (Loi retry được, KhongRo thì không).
/// Tham số cuối = `name` server gửi (tên file gợi ý), có thể None.
/// `+ 'a`: hàm in thật mượn kênh báo sự cố của worker (net.rs), không 'static.
pub type HamIn<'a> = dyn Fn(&[u8], &str, &str, &str, u32, &str, Option<&str>) -> KetQuaIn + 'a;

/// Xử lý payload JSON của event "job" → `KetQua` (luôn có, kể cả `khong_ro`).
///
/// Hàm này KHÔNG quyết có gửi hay không: `khong_ro` vẫn được dựng (kèm lý do +
/// mã) để giao diện/nhật ký nêu đúng nguyên nhân, còn gửi hay im do `CanHoTro`
/// quyết theo `cau-hinh` của kết nối. Không bao giờ panic — job phía server
/// không được kẹt.
pub fn xu_ly_job(payload: &serde_json::Value, cfg: &Config, in_fn: &HamIn<'_>) -> KetQua {
    // Lấy job_id sớm để mọi nhánh lỗi đều báo đúng job.
    let job_id = payload
        .get("job")
        .and_then(|j| j.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let env: JobEnvelope = match serde_json::from_value(payload.clone()) {
        Ok(e) => e,
        // Lỗi parse payload: CHẮC CHẮN chưa in gì (chưa có PDF để in) → Loi,
        // server retry an toàn.
        Err(e) => return KetQua::loi(job_id, format!("payload sai: {}", e).into()),
    };
    let job = env.job;

    let pdf = match STANDARD.decode(job.pdf_base64.as_bytes()) {
        Ok(b) => b,
        // Tương tự: base64 hỏng → chưa in gì → Loi, retry an toàn.
        Err(e) => return KetQua::loi(job.id, LyDo::co_loai(format!("base64 lỗi: {}", e), MaSuCo::LoiPdf)),
    };

    let paper = job.paper_size.unwrap_or_else(|| cfg.paper_size.clone());
    let tray = job.tray.unwrap_or_else(|| cfg.tray.clone());
    let copies = job.copies.unwrap_or(1);

    match in_fn(&pdf, &cfg.printer_name, &paper, &tray, copies, &job.id, job.name.as_deref()) {
        KetQuaIn::DaIn => KetQua::da_in(job.id),
        KetQuaIn::Loi(ly_do) => KetQua::loi(job.id, ly_do),
        KetQuaIn::KhongRo(ly_do) => KetQua::khong_ro(job.id, ly_do),
    }
}

/// Id job đã CẮT TOKEN để hiện/ghi nhật ký (§0.3). Backend sinh id dạng
/// `<token>-<mili-giây>-<số thứ tự>` (handoff §5.4) → giữ `<mili-giây>-<số>`,
/// đúng cách backend ghi `agent_job_id`. Id không theo mẫu đó (id test, "in-thu")
/// giữ nguyên — nó không chứa token.
pub fn rut_gon_job_id(job_id: &str) -> String {
    let phan: Vec<&str> = job_id.rsplitn(3, '-').collect();
    let la_so = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match phan.as_slice() {
        [so, ms, token] if la_so(so) && la_so(ms) && !token.is_empty() => format!("{}-{}", ms, so),
        _ => job_id.to_string(),
    }
}

/// Bóc số hoá đơn + tên khách từ `name` server gửi:
/// `"AI-INV_2026_030045-Anh_Loc-<jobId>.pdf"` → `("INV_2026_030045", Some("Anh_Loc"))`.
///
/// `None` khi `name` không theo mẫu backend (không kết thúc bằng `-<jobId>`) —
/// thà hiện id job còn hơn hiện một mẩu tên đoán sai là số hoá đơn. Khách
/// "Khong_ro" (backend dùng khi không đọc được tên) coi như không có.
pub fn boc_hoa_don(name: &str, job_id: &str) -> Option<(String, Option<String>)> {
    if job_id.is_empty() {
        return None;
    }
    let ten = name.trim();
    let ten = match ten.len().checked_sub(4).and_then(|i| ten.get(i..)) {
        Some(duoi) if duoi.eq_ignore_ascii_case(".pdf") => &ten[..ten.len() - 4],
        _ => ten,
    };
    let ten = ten.strip_suffix(job_id)?.strip_suffix('-')?;
    let ten = ten.strip_prefix("AI-").unwrap_or(ten);
    let (so, khach) = match ten.split_once('-') {
        Some((so, khach)) => (so, Some(khach)),
        None => (ten, None),
    };
    if so.is_empty() {
        return None;
    }
    let khach = khach.filter(|k| !k.is_empty() && *k != "Khong_ro").map(str::to_string);
    Some((so.to_string(), khach))
}

/// (dòng chính, khách) hiện ở "In gần đây": số hoá đơn + khách bóc từ `name`,
/// không có thì id job ĐÃ CẮT TOKEN.
pub fn nhan_hien_thi(job_id: &str, name: Option<&str>) -> (String, Option<String>) {
    name.and_then(|n| boc_hoa_don(n, job_id))
        .unwrap_or_else(|| (rut_gon_job_id(job_id), None))
}


#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn cfg() -> Config {
        Config {
            server_url: "u".into(), token: "t".into(),
            printer_name: "HP".into(), tray: "tray-1".into(), paper_size: "A5".into(),
        }
    }

    #[test]
    fn job_hop_le_in_thanh_cong_tra_da_in() {
        let pdf_b64 = STANDARD.encode(b"%PDF-1.4");
        let payload = serde_json::json!({
            "loai": "in",
            "job": {"id": "j1", "pdfBase64": pdf_b64, "paperSize": "A5", "tray": "tray-2", "copies": 1}
        });
        // hàm in giả: nhận đúng bytes + tham số → DaIn
        let in_fn = move |pdf: &[u8], printer: &str, paper: &str, tray: &str, _c: u32, job_id: &str, _n: Option<&str>| {
            assert_eq!(pdf, b"%PDF-1.4");
            assert_eq!(printer, "HP");
            assert_eq!(paper, "A5");
            assert_eq!(tray, "tray-2");
            assert_eq!(job_id, "j1");
            KetQuaIn::DaIn
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq, KetQua::da_in("j1".into()));
    }

    #[test]
    fn name_server_gui_duoc_chuyen_nguyen_xuong_ham_in() {
        let pdf_b64 = STANDARD.encode(b"%PDF-1.4");
        let ten = "AI-INV_2026_030045-Anh_Loc_Beco-j5.pdf";
        let co_name = serde_json::json!({"loai": "in", "job": {"id": "j5", "name": ten, "pdfBase64": pdf_b64}});
        let khong_name = serde_json::json!({"loai": "in", "job": {"id": "j6", "pdfBase64": pdf_b64}});
        let nhan = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Option<String>>::new()));
        let ghi = nhan.clone();
        let in_fn = move |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str, n: Option<&str>| {
            ghi.lock().unwrap().push(n.map(|x| x.to_string()));
            KetQuaIn::DaIn
        };
        assert_eq!(xu_ly_job(&co_name, &cfg(), &in_fn), KetQua::da_in("j5".into()));
        assert_eq!(xu_ly_job(&khong_name, &cfg(), &in_fn), KetQua::da_in("j6".into()));
        assert_eq!(*nhan.lock().unwrap(), vec![Some(ten.to_string()), None]);
    }

    use crate::bao_cao::HoTro;
    use crate::hop_thu_di::CanHoTro;

    fn can_gui(kq: &KetQua, ho_tro: &HoTro) -> bool {
        CanHoTro::cua_ket_qua(kq).duoc_gui(*ho_tro)
    }

    #[test]
    fn in_loi_tra_ket_qua_loi_khong_panic() {
        let payload = serde_json::json!({
            "job": {"id": "j2", "pdfBase64": STANDARD.encode(b"x")}
        });
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str, _: Option<&str>| {
            KetQuaIn::Loi("máy in offline".into())
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert!(can_gui(&kq, &HoTro::default()), "Loi phai emit, ke ca backend cu");
        assert_eq!(kq.trang_thai, "loi");
        assert_eq!(kq.job_id, "j2");
        assert!(kq.loi_cuoi.unwrap().contains("offline"));
    }

    /// NGUYÊN TẮC CHỐNG IN ĐÔI (§0.4): `khong_ro` chỉ đi khi backend đã gửi
    /// `cau-hinh` có "khong_ro". Backend CŨ hiểu mọi `trangThai` khác `loi` là
    /// ĐÃ IN — gửi `khong_ro` cho nó là báo sai; im lặng thì backend cũ tự suy
    /// khong_ro và KHÔNG retry. `da_in`/`loi` luôn gửi như cũ. Cửa quyết:
    /// `hop_thu_di::CanHoTro::cua_ket_qua` (lúc gửi, theo kết nối hiện tại).
    ///
    /// ĐỔI HÌNH THỨC (hợp đồng v2, 24/09), bất biến giữ nguyên: bản trước
    /// `xu_ly_job` trả `None` cho KhongRo; nay nó luôn dựng `KetQua` (giao diện
    /// cần lý do + mã để hiện "Không rõ: <nhãn>") và `CanHoTro` là cửa duy nhất
    /// quyết gửi. Test khoá cả hai: không có hoTro ⇒ không gửi.
    #[test]
    fn khong_ro_khong_emit_khi_backend_khong_ho_tro() {
        let payload = serde_json::json!({
            "job": {"id": "j2b", "pdfBase64": STANDARD.encode(b"x")}
        });
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str, _: Option<&str>| {
            KetQuaIn::KhongRo("khong quan sat duoc spooler".into())
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq.trang_thai, "khong_ro");
        assert!(!can_gui(&kq, &HoTro::default()), "KhongRo + backend cũ phải IM LẶNG (không emit)");
        let chi_su_co = HoTro { su_co: true, trang_thai_may_in: true, khong_ro: false, nhat_ky_app: false };
        assert!(!can_gui(&kq, &chi_su_co), "thiếu đúng 'khong_ro' trong hoTro vẫn phải im lặng");
    }

    #[test]
    fn khong_ro_gui_kem_loai_khi_backend_ho_tro() {
        let payload = serde_json::json!({"job": {"id": "j2c", "pdfBase64": STANDARD.encode(b"x")}});
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str, _: Option<&str>| {
            KetQuaIn::KhongRo(LyDo::co_loai("loi sau khi da bat dau in: Kẹt giấy", MaSuCo::KetGiay))
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        let ho_tro = HoTro { khong_ro: true, ..HoTro::default() };
        assert!(can_gui(&kq, &ho_tro));
        let v = serde_json::to_value(&kq).unwrap();
        assert_eq!(v, serde_json::json!({
            "jobId": "j2c", "trangThai": "khong_ro",
            "loiCuoi": "loi sau khi da bat dau in: Kẹt giấy", "loai": "ket_giay"
        }));
    }

    #[test]
    fn loi_kem_loai_serialize_loai() {
        let kq = KetQua::loi("j".into(), LyDo::co_loai("loi truoc khi in", MaSuCo::HetGiay));
        let v = serde_json::to_value(&kq).unwrap();
        assert_eq!(v["trangThai"], "loi");
        assert_eq!(v["loai"], "het_giay");
        let kq = KetQua::loi("j".into(), "payload sai".into());
        assert!(serde_json::to_value(&kq).unwrap().get("loai").is_none(), "không biết mã thì bỏ hẳn loai");
    }

    #[test]
    fn base64_hong_tra_loi_giu_dung_job_id() {
        let payload = serde_json::json!({"job": {"id": "j3", "pdfBase64": "!!!khong-phai-base64!!!"}});
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str, _: Option<&str>| KetQuaIn::DaIn;
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq.trang_thai, "loi", "loi parse phai emit Loi (chua in gi)");
        assert_eq!(kq.job_id, "j3");
        assert_eq!(kq.loai, Some(MaSuCo::LoiPdf));
    }

    #[test]
    fn thieu_paper_tray_dung_mac_dinh_config() {
        let payload = serde_json::json!({"job": {"id": "j4", "pdfBase64": STANDARD.encode(b"p")}});
        let in_fn = |_: &[u8], _: &str, paper: &str, tray: &str, _: u32, _: &str, _: Option<&str>| {
            assert_eq!(paper, "A5");   // từ config
            assert_eq!(tray, "tray-1"); // từ config
            KetQuaIn::DaIn
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq.trang_thai, "da_in");
    }

    #[test]
    fn ket_qua_da_in_serialize_dung_field() {
        let j = serde_json::to_value(KetQua::da_in("j5".into())).unwrap();
        assert_eq!(j["jobId"], "j5");
        assert_eq!(j["trangThai"], "da_in");
        assert!(j.get("loiCuoi").is_none()); // skip khi None
        assert!(j.get("loai").is_none());
    }

    #[test]
    fn rut_gon_job_id_cat_token() {
        assert_eq!(rut_gon_job_id("tokHN8f2a-1727170000000-3"), "1727170000000-3");
        assert_eq!(rut_gon_job_id("tok-co-gach-1727-12"), "1727-12", "token có '-' vẫn cắt đúng từ bên phải");
        for id in ["j1", "in-thu", "a-b-c", "tok-1727-", "-1727-3"] {
            assert_eq!(rut_gon_job_id(id), id, "id không theo mẫu backend giữ nguyên");
        }
    }

    #[test]
    fn boc_hoa_don_tu_name_backend() {
        let id = "tokHN-1727170000000-3";
        let name = format!("AI-INV_2026_030045-Anh_Loc-{}.pdf", id);
        assert_eq!(boc_hoa_don(&name, id), Some(("INV_2026_030045".into(), Some("Anh_Loc".into()))));
        // khách có '-' giữ nguyên phần sau số hoá đơn; đuôi .PDF viết hoa vẫn nhận
        let name = format!("AI-INV_2026_030046-Chi-Muoi-{}.PDF", id);
        assert_eq!(boc_hoa_don(&name, id), Some(("INV_2026_030046".into(), Some("Chi-Muoi".into()))));
        // backend không đọc được tên khách → "Khong_ro" → coi như không có
        let name = format!("AI-INV_2026_030047-Khong_ro-{}.pdf", id);
        assert_eq!(boc_hoa_don(&name, id), Some(("INV_2026_030047".into(), None)));
    }

    #[test]
    fn boc_hoa_don_sai_mau_thi_none() {
        let id = "tok-1727-3";
        assert_eq!(boc_hoa_don("AI-INV_1-Khach-tok-9999-9.pdf", id), None, "không kết thúc bằng -<jobId>");
        assert_eq!(boc_hoa_don(&format!("{}.pdf", id), id), None);
        assert_eq!(boc_hoa_don(&format!("AI--{}.pdf", id), id), None, "số hoá đơn rỗng");
        assert_eq!(boc_hoa_don("x.pdf", ""), None);
        assert_eq!(boc_hoa_don("", id), None);
    }

    #[test]
    fn nhan_hien_thi_uu_tien_name_roi_id_da_cat_token() {
        let id = "tokHN-1727170000000-3";
        let name = format!("AI-INV_2026_030045-Anh_Loc-{}.pdf", id);
        assert_eq!(nhan_hien_thi(id, Some(&name)), ("INV_2026_030045".into(), Some("Anh_Loc".into())));
        assert_eq!(nhan_hien_thi(id, None), ("1727170000000-3".into(), None));
        assert_eq!(nhan_hien_thi(id, Some("rac.pdf")), ("1727170000000-3".into(), None));
    }
}
