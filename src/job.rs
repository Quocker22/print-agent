// SPDX-License-Identifier: AGPL-3.0-or-later
//! Xử lý một job in: parse payload server gửi → giải mã PDF → in → dựng kết quả.
//! Tách thuần (không đụng socket.io) để test được không cần server.

use crate::config::Config;
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
    #[serde(rename = "pdfBase64")]
    pub pdf_base64: String,
    #[serde(rename = "paperSize")]
    pub paper_size: Option<String>,
    pub tray: Option<String>,
    pub copies: Option<u32>,
}

/// Kết quả emit về server qua event "ket-qua".
#[derive(Debug, Serialize, PartialEq)]
pub struct KetQua {
    #[serde(rename = "jobId")]
    pub job_id: String,
    #[serde(rename = "trangThai")]
    pub trang_thai: String, // "da_in" | "loi"
    #[serde(rename = "loiCuoi", skip_serializing_if = "Option::is_none")]
    pub loi_cuoi: Option<String>,
}

impl KetQua {
    fn da_in(job_id: String) -> Self {
        Self { job_id, trang_thai: "da_in".into(), loi_cuoi: None }
    }
    fn loi(job_id: String, ly_do: String) -> Self {
        Self { job_id, trang_thai: "loi".into(), loi_cuoi: Some(ly_do) }
    }
}

/// Kết quả in 3 nhánh — TRỌNG TÂM chống in đôi. `KhongRo` KHÔNG BAO GIỜ được
/// map sang KetQua để emit về server (xem `xu_ly_job` bên dưới): khi không
/// chắc job đã in hay chưa, agent PHẢI im lặng, để server tự suy "khong_ro"
/// và KHÔNG tự động retry. Xem thiết kế đầy đủ ở spooler.rs.
#[derive(Debug, Clone, PartialEq)]
pub enum KetQuaIn {
    /// Quan sát trực tiếp job đã in xong (bằng chứng mạnh nhất từ spooler),
    /// hoặc dry-run (test/không có máy in thật).
    DaIn,
    /// Lỗi XẢY RA TRƯỚC KHI có bằng chứng đã bắt đầu in — chắc chắn CHƯA in
    /// tờ nào, an toàn để server retry (gửi lại job).
    Loi(String),
    /// Không chắc — hoặc chưa từng quan sát được job trong hàng đợi, hoặc đã
    /// bắt đầu in rồi mất dấu/lỗi/timeout mà chưa thấy in xong. KHÔNG được
    /// suy DaIn hay Loi trong tình huống này (Loi ở đây rủi ro in đôi nếu
    /// server retry mà tờ giấy thực ra đã ra khỏi máy in).
    KhongRo(String),
}

/// Hàm in — tiêm được để test (thật = printing::in_pdf). Trả KetQuaIn (3
/// nhánh) thay vì anyhow::Result — "lỗi" và "không chắc" là hai tình huống
/// XỬ LÝ KHÁC NHAU (Loi retry được, KhongRo thì không).
pub type HamIn = dyn Fn(&[u8], &str, &str, &str, u32, &str) -> KetQuaIn;

/// Xử lý payload JSON của event "job" → Option<KetQua>.
/// - Some(kq): report được về server ("da_in" hoặc "loi") — caller (net.rs) emit.
/// - None: kết quả "không rõ" (KetQuaIn::KhongRo) — KHÔNG BAO GIỜ emit gì,
///   đây là NGUYÊN TẮC CHỐNG IN ĐÔI: khi không chắc, im lặng để server tự
///   suy "khong_ro" mà KHÔNG tự ý retry. Không bao giờ panic — job phía
///   server không được kẹt.
pub fn xu_ly_job(payload: &serde_json::Value, cfg: &Config, in_fn: &HamIn) -> Option<KetQua> {
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
        Err(e) => return Some(KetQua::loi(job_id, format!("payload sai: {}", e))),
    };
    let job = env.job;

    let pdf = match STANDARD.decode(job.pdf_base64.as_bytes()) {
        Ok(b) => b,
        // Tương tự: base64 hỏng → chưa in gì → Loi, retry an toàn.
        Err(e) => return Some(KetQua::loi(job.id, format!("base64 lỗi: {}", e))),
    };

    let paper = job.paper_size.unwrap_or_else(|| cfg.paper_size.clone());
    let tray = job.tray.unwrap_or_else(|| cfg.tray.clone());
    let copies = job.copies.unwrap_or(1);

    match in_fn(&pdf, &cfg.printer_name, &paper, &tray, copies, &job.id) {
        KetQuaIn::DaIn => Some(KetQua::da_in(job.id)),
        KetQuaIn::Loi(ly_do) => Some(KetQua::loi(job.id, ly_do)),
        // KHÔNG emit — xem doc-comment của hàm này + spooler.rs.
        KetQuaIn::KhongRo(_) => None,
    }
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
        let in_fn = move |pdf: &[u8], printer: &str, paper: &str, tray: &str, _c: u32, job_id: &str| {
            assert_eq!(pdf, b"%PDF-1.4");
            assert_eq!(printer, "HP");
            assert_eq!(paper, "A5");
            assert_eq!(tray, "tray-2");
            assert_eq!(job_id, "j1");
            KetQuaIn::DaIn
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq, Some(KetQua::da_in("j1".into())));
    }

    #[test]
    fn in_loi_tra_ket_qua_loi_khong_panic() {
        let payload = serde_json::json!({
            "job": {"id": "j2", "pdfBase64": STANDARD.encode(b"x")}
        });
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str| {
            KetQuaIn::Loi("máy in offline".into())
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn).expect("Loi phai emit");
        assert_eq!(kq.trang_thai, "loi");
        assert_eq!(kq.job_id, "j2");
        assert!(kq.loi_cuoi.unwrap().contains("offline"));
    }

    /// NGUYÊN TẮC CHỐNG IN ĐÔI: KhongRo → None, tuyệt đối không emit.
    #[test]
    fn khong_ro_tra_none_khong_emit() {
        let payload = serde_json::json!({
            "job": {"id": "j2b", "pdfBase64": STANDARD.encode(b"x")}
        });
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str| {
            KetQuaIn::KhongRo("khong quan sat duoc spooler".into())
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq, None, "KhongRo phai tra None (khong emit ket qua)");
    }

    #[test]
    fn base64_hong_tra_loi_giu_dung_job_id() {
        let payload = serde_json::json!({"job": {"id": "j3", "pdfBase64": "!!!khong-phai-base64!!!"}});
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32, _: &str| KetQuaIn::DaIn;
        let kq = xu_ly_job(&payload, &cfg(), &in_fn).expect("loi parse phai emit Loi (chua in gi)");
        assert_eq!(kq.trang_thai, "loi");
        assert_eq!(kq.job_id, "j3");
    }

    #[test]
    fn thieu_paper_tray_dung_mac_dinh_config() {
        let payload = serde_json::json!({"job": {"id": "j4", "pdfBase64": STANDARD.encode(b"p")}});
        let in_fn = |_: &[u8], _: &str, paper: &str, tray: &str, _: u32, _: &str| {
            assert_eq!(paper, "A5");   // từ config
            assert_eq!(tray, "tray-1"); // từ config
            KetQuaIn::DaIn
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn).unwrap();
        assert_eq!(kq.trang_thai, "da_in");
    }

    #[test]
    fn ket_qua_da_in_serialize_dung_field() {
        let j = serde_json::to_value(KetQua::da_in("j5".into())).unwrap();
        assert_eq!(j["jobId"], "j5");
        assert_eq!(j["trangThai"], "da_in");
        assert!(j.get("loiCuoi").is_none()); // skip khi None
    }
}
