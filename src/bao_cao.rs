// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phần THUẦN của giao thức báo cáo mới (hợp đồng v2 §2): đọc `cau-hinh`, dựng
//! payload `su-co` / `trang-thai-may-in` / `thong-tin-app`, lọc `su-co` một lần
//! mỗi (job, loai). Không đụng socket — net.rs quyết khi nào gửi, qua kết nối nào.

use crate::config::Config;
use crate::su_co::MaSuCo;
use crate::thoi_gian;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::SystemTime;

/// Những gì backend của KẾT NỐI HIỆN TẠI hiểu (event `cau-hinh`).
///
/// Mặc định = tất cả `false` = backend cũ: KHÔNG gửi `khong_ro`, `su-co`,
/// `trang-thai-may-in` (bất biến §0.4 — backend cũ hiểu mọi `trangThai` khác
/// `loi` là đã in, gửi `khong_ro` cho nó là báo sai "đã in").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoTro {
    pub khong_ro: bool,
    pub su_co: bool,
    pub trang_thai_may_in: bool,
    /// Backend nhận toàn bộ nhật ký cục bộ của app (`nhat-ky-app`, 0.2.4).
    pub nhat_ky_app: bool,
}

/// Payload `cau-hinh` `{hoTro: [...]}` → `HoTro`. Payload lạ/thiếu/sai kiểu →
/// mặc định (không hỗ trợ gì): đọc sai theo hướng "hỗ trợ" là gửi `khong_ro`
/// cho backend không hiểu nó; đọc sai theo hướng ngược lại chỉ mất báo cáo.
pub fn doc_cau_hinh(payload: &Value) -> HoTro {
    let mut ho_tro = HoTro::default();
    let Some(ds) = payload.get("hoTro").and_then(Value::as_array) else {
        return ho_tro;
    };
    for muc in ds.iter().filter_map(Value::as_str) {
        match muc {
            "khong_ro" => ho_tro.khong_ro = true,
            "su_co" => ho_tro.su_co = true,
            "trang_thai_may_in" => ho_tro.trang_thai_may_in = true,
            "nhat_ky_app" => ho_tro.nhat_ky_app = true,
            _ => {} // tính năng backend mới hơn app — bỏ qua, không lỗi
        }
    }
    ho_tro
}

/// `su-co`: `{jobId, loai, chiTiet?, mayIn, luc}`.
pub fn su_co(job_id: &str, loai: MaSuCo, chi_tiet: Option<&str>, may_in: &str, luc: SystemTime) -> Value {
    let mut v = json!({ "jobId": job_id, "loai": loai, "mayIn": may_in, "luc": thoi_gian::iso_utc(luc) });
    if let Some(ct) = chi_tiet {
        v["chiTiet"] = json!(ct);
    }
    v
}

/// `trang-thai-may-in`: `{trangThai, chiTiet?, mayIn, luc}`.
pub fn trang_thai_may_in(ma: MaSuCo, chi_tiet: Option<&str>, may_in: &str, luc: SystemTime) -> Value {
    let mut v = json!({ "trangThai": ma, "mayIn": may_in, "luc": thoi_gian::iso_utc(luc) });
    if let Some(ct) = chi_tiet {
        v["chiTiet"] = json!(ct);
    }
    v
}

/// `thong-tin-app`: `{phienBan, mayIn, khay, khoGiay, may}`. KHÔNG có token.
pub fn thong_tin_app(cfg: &Config, may: &str) -> Value {
    json!({
        "phienBan": env!("CARGO_PKG_VERSION"),
        "mayIn": cfg.printer_name,
        "khay": cfg.tray,
        "khoGiay": cfg.paper_size,
        "may": may,
    })
}

/// Tên máy tính: `COMPUTERNAME` trên Windows (luôn có trong phiên người dùng);
/// `HOSTNAME` cho máy dev. Không có thì chuỗi rỗng — không chặn kết nối vì nó.
pub fn ten_may_tinh() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

/// Nhớ các `loai` đã gửi `su-co` cho MỘT job — hợp đồng: mỗi (job, loai) một
/// lần. Poll spooler 500 ms một lần, máy hết giấy 15 giây sẽ quan sát ~30 lần;
/// không lọc là 30 dòng nhật ký giống nhau ở backend.
#[derive(Debug, Default)]
pub struct BoLocSuCo {
    da_gui: HashSet<MaSuCo>,
}

impl BoLocSuCo {
    /// `true` đúng MỘT lần cho mỗi `loai` (lần đầu thấy).
    pub fn lan_dau(&mut self, loai: MaSuCo) -> bool {
        self.da_gui.insert(loai)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn cau_hinh_du_ba_tinh_nang() {
        assert!(doc_cau_hinh(&json!({"hoTro": ["nhat_ky_app"]})).nhat_ky_app);
        assert!(!doc_cau_hinh(&json!({"hoTro": ["su_co"]})).nhat_ky_app);
        let h = doc_cau_hinh(&json!({"hoTro": ["khong_ro", "su_co", "trang_thai_may_in"]}));
        assert_eq!(h, HoTro { khong_ro: true, su_co: true, trang_thai_may_in: true, nhat_ky_app: false });
    }

    #[test]
    fn cau_hinh_mot_phan_va_muc_la_bo_qua() {
        let h = doc_cau_hinh(&json!({"hoTro": ["su_co", "tinh_nang_tuong_lai", 42]}));
        assert_eq!(h, HoTro { khong_ro: false, su_co: true, trang_thai_may_in: false, nhat_ky_app: false });
    }

    #[test]
    fn cau_hinh_hong_thi_khong_ho_tro_gi() {
        for p in [json!(null), json!({}), json!({"hoTro": "khong_ro"}), json!(["khong_ro"])] {
            assert_eq!(doc_cau_hinh(&p), HoTro::default(), "{}", p);
        }
    }

    #[test]
    fn payload_su_co_dung_hop_dong() {
        let luc = UNIX_EPOCH + Duration::from_secs(1_727_170_000);
        let v = su_co("tok-1-2", MaSuCo::HetGiay, Some("JOB_STATUS PAPEROUT (0x00000040)"), "HP 4003", luc);
        assert_eq!(v, json!({
            "jobId": "tok-1-2", "loai": "het_giay", "chiTiet": "JOB_STATUS PAPEROUT (0x00000040)",
            "mayIn": "HP 4003", "luc": "2024-09-24T09:26:40.000Z"
        }));
        let v = su_co("j", MaSuCo::Offline, None, "HP", luc);
        assert!(v.get("chiTiet").is_none(), "chiTiet là tuỳ chọn — không có thì bỏ hẳn");
    }

    #[test]
    fn payload_trang_thai_may_in_dung_hop_dong() {
        let luc = UNIX_EPOCH;
        let v = trang_thai_may_in(MaSuCo::BinhThuong, None, "HP", luc);
        assert_eq!(v, json!({"trangThai": "binh_thuong", "mayIn": "HP", "luc": "1970-01-01T00:00:00.000Z"}));
    }

    #[test]
    fn thong_tin_app_khong_co_token() {
        let cfg = Config {
            server_url: "u".into(), token: "BI-MAT-TOKEN".into(),
            printer_name: "HP 4003".into(), tray: "tray-2".into(), paper_size: "A5".into(),
        };
        let v = thong_tin_app(&cfg, "SHOP-HN");
        assert_eq!(v["phienBan"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["mayIn"], "HP 4003");
        assert_eq!(v["khay"], "tray-2");
        assert_eq!(v["khoGiay"], "A5");
        assert_eq!(v["may"], "SHOP-HN");
        assert!(!v.to_string().contains("BI-MAT-TOKEN"), "§0.3: không lộ token");
    }

    #[test]
    fn bo_loc_su_co_moi_loai_mot_lan() {
        let mut b = BoLocSuCo::default();
        assert!(b.lan_dau(MaSuCo::HetGiay));
        assert!(!b.lan_dau(MaSuCo::HetGiay));
        assert!(b.lan_dau(MaSuCo::Offline), "loai khác vẫn gửi");
        assert!(!b.lan_dau(MaSuCo::HetGiay));
    }
}
