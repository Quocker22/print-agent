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

/// Hàm in — tiêm được để test (thật = printing::in_pdf).
pub type HamIn = dyn Fn(&[u8], &str, &str, &str, u32) -> anyhow::Result<()>;

/// Xử lý payload JSON của event "job" → KetQua. LUÔN trả KetQua (kể cả lỗi),
/// không bao giờ panic — job phía server không được kẹt.
pub fn xu_ly_job(payload: &serde_json::Value, cfg: &Config, in_fn: &HamIn) -> KetQua {
    // Lấy job_id sớm để mọi nhánh lỗi đều báo đúng job.
    let job_id = payload
        .get("job")
        .and_then(|j| j.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let env: JobEnvelope = match serde_json::from_value(payload.clone()) {
        Ok(e) => e,
        Err(e) => return KetQua::loi(job_id, format!("payload sai: {}", e)),
    };
    let job = env.job;

    let pdf = match STANDARD.decode(job.pdf_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return KetQua::loi(job.id, format!("base64 lỗi: {}", e)),
    };

    let paper = job.paper_size.unwrap_or_else(|| cfg.paper_size.clone());
    let tray = job.tray.unwrap_or_else(|| cfg.tray.clone());
    let copies = job.copies.unwrap_or(1);

    match in_fn(&pdf, &cfg.printer_name, &paper, &tray, copies) {
        Ok(()) => KetQua::da_in(job.id),
        Err(e) => KetQua::loi(job.id, format!("in lỗi: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn cfg() -> Config {
        Config {
            server_url: "u".into(), token: "t".into(), org_id: "o".into(),
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
        // hàm in giả: nhận đúng bytes + tham số → Ok
        let in_fn = move |pdf: &[u8], printer: &str, paper: &str, tray: &str, _c: u32| {
            assert_eq!(pdf, b"%PDF-1.4");
            assert_eq!(printer, "HP");
            assert_eq!(paper, "A5");
            assert_eq!(tray, "tray-2");
            Ok(())
        };
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq, KetQua::da_in("j1".into()));
    }

    #[test]
    fn in_loi_tra_ket_qua_loi_khong_panic() {
        let payload = serde_json::json!({
            "job": {"id": "j2", "pdfBase64": STANDARD.encode(b"x")}
        });
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32| anyhow::bail!("máy in offline");
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq.trang_thai, "loi");
        assert_eq!(kq.job_id, "j2");
        assert!(kq.loi_cuoi.unwrap().contains("offline"));
    }

    #[test]
    fn base64_hong_tra_loi_giu_dung_job_id() {
        let payload = serde_json::json!({"job": {"id": "j3", "pdfBase64": "!!!khong-phai-base64!!!"}});
        let in_fn = |_: &[u8], _: &str, _: &str, _: &str, _: u32| Ok(());
        let kq = xu_ly_job(&payload, &cfg(), &in_fn);
        assert_eq!(kq.trang_thai, "loi");
        assert_eq!(kq.job_id, "j3");
    }

    #[test]
    fn thieu_paper_tray_dung_mac_dinh_config() {
        let payload = serde_json::json!({"job": {"id": "j4", "pdfBase64": STANDARD.encode(b"p")}});
        let in_fn = |_: &[u8], _: &str, paper: &str, tray: &str, _: u32| {
            assert_eq!(paper, "A5");   // từ config
            assert_eq!(tray, "tray-1"); // từ config
            Ok(())
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
    }
}
