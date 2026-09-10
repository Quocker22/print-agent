// SPDX-License-Identifier: AGPL-3.0-or-later
//! Đọc config.ini (định dạng INI đơn giản) thành struct Config.
//! Tự bỏ BOM đầu file — notepad Windows hay thêm BOM (bug thật đã gặp bản Python).

use anyhow::{bail, Result};

/// Token agent nhúng lúc build: `AGENT_TOKEN=<token> cargo build`.
/// Khi thiếu env lúc build → rỗng (net.rs auth fail rõ ràng, không âm thầm sai).
pub const AGENT_TOKEN: &str = match option_env!("AGENT_TOKEN") {
    Some(t) => t,
    None => "",
};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub server_url: String,
    pub token: String,
    pub org_id: String,
    pub printer_name: String,
    /// Khay mặc định nếu job không chỉ định. Dạng "tray-<n>".
    pub tray: String,
    /// Khổ giấy mặc định nếu job không chỉ định.
    pub paper_size: String,
}

const DEFAULT_TRAY: &str = "tray-1";
const DEFAULT_PAPER: &str = "A5";

/// Parse nội dung config.ini (chuỗi) → Config. Chỉ hiểu section [agent],
/// dòng `key = value`, bỏ qua dòng trống và comment (bắt đầu bằng ; hoặc #).
pub fn parse_config(text: &str) -> Result<Config> {
    // Bỏ BOM (\u{feff}) đầu chuỗi nếu có — notepad Windows hay thêm.
    let text = text.trim_start_matches('\u{feff}');

    let mut trong_agent = false;
    let mut kv: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for dong in text.lines() {
        let d = dong.trim();
        if d.is_empty() || d.starts_with(';') || d.starts_with('#') {
            continue;
        }
        if d.starts_with('[') && d.ends_with(']') {
            trong_agent = &d[1..d.len() - 1] == "agent";
            continue;
        }
        if !trong_agent {
            continue;
        }
        if let Some((k, v)) = d.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let get = |k: &str| kv.get(k).map(|s| s.as_str()).unwrap_or("").to_string();
    let bat_buoc = |k: &str| -> Result<String> {
        let v = get(k);
        if v.is_empty() {
            bail!("config.ini thiếu field bắt buộc: {}", k);
        }
        Ok(v)
    };

    if !text.contains("[agent]") {
        bail!("config.ini thiếu section [agent]");
    }

    Ok(Config {
        server_url: bat_buoc("server_url")?,
        token: {
            let v = get("token");
            if v.is_empty() { AGENT_TOKEN.to_string() } else { v }
        },
        org_id: bat_buoc("org_id")?,
        printer_name: bat_buoc("printer_name")?,
        tray: {
            let v = get("tray");
            if v.is_empty() { DEFAULT_TRAY.to_string() } else { v }
        },
        paper_size: {
            let v = get("paper_size");
            if v.is_empty() { DEFAULT_PAPER.to_string() } else { v }
        },
    })
}

/// Serialize Config → chuỗi INI (ngược của parse_config) để UI ghi lại
/// config.ini sau khi người dùng sửa trong tab Cấu hình.
///
/// VÌ SAO không dùng crate ini riêng: định dạng quá đơn giản (1 section,
/// key=value phẳng), thêm crate chỉ để làm việc này là thừa; tự viết vài
/// dòng đọc lại được ngay bằng parse_config ở trên (test round-trip bên dưới).
pub fn ghi_config(cfg: &Config) -> String {
    format!(
        "[agent]\n\
         server_url = {}\n\
         org_id = {}\n\
         printer_name = {}\n\
         tray = {}\n\
         paper_size = {}\n",
        cfg.server_url, cfg.org_id, cfg.printer_name, cfg.tray, cfg.paper_size
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DU: &str = "[agent]\nserver_url = https://crm.example.com\ntoken = tok123\norg_id = org1\nprinter_name = HP LaserJet\n";

    #[test]
    fn parse_day_du_field() {
        let c = parse_config(DU).unwrap();
        assert_eq!(c.server_url, "https://crm.example.com");
        assert_eq!(c.token, "tok123");
        assert_eq!(c.org_id, "org1");
        assert_eq!(c.printer_name, "HP LaserJet");
        assert_eq!(c.tray, "tray-1"); // mặc định
        assert_eq!(c.paper_size, "A5"); // mặc định
    }

    #[test]
    fn co_BOM_van_doc_duoc() {
        // notepad Windows hay thêm BOM đầu file → phải bỏ.
        let voi_bom = format!("\u{feff}{}", DU);
        let c = parse_config(&voi_bom).unwrap();
        assert_eq!(c.token, "tok123");
    }

    #[test]
    fn thieu_section_agent_bao_loi() {
        let e = parse_config("server_url = x\n").unwrap_err();
        assert!(e.to_string().contains("[agent]"));
    }

    #[test]
    fn thieu_field_bat_buoc_bao_loi() {
        let e = parse_config("[agent]\ntoken = t\n").unwrap_err();
        assert!(e.to_string().contains("server_url"));
    }

    #[test]
    fn tray_paper_ghi_de_duoc() {
        let t = format!("{}tray = tray-2\npaper_size = A4\n", DU);
        let c = parse_config(&t).unwrap();
        assert_eq!(c.tray, "tray-2");
        assert_eq!(c.paper_size, "A4");
    }

    #[test]
    fn bo_qua_comment_va_dong_trong() {
        let t = "; comment\n\n[agent]\n# ghi chu\nserver_url = u\ntoken = t\norg_id = o\nprinter_name = p\n";
        let c = parse_config(t).unwrap();
        assert_eq!(c.server_url, "u");
    }

    #[test]
    fn khong_co_token_thi_dung_gia_tri_default() {
        // Config không có token → dùng AGENT_TOKEN (rỗng lúc test vì không set env)
        let cfg_text = "[agent]\nserver_url = https://crm.example.com\norg_id = org1\nprinter_name = HP LaserJet\n";
        let c = parse_config(cfg_text).unwrap();
        assert_eq!(c.token, AGENT_TOKEN);
        assert_eq!(c.token, ""); // AGENT_TOKEN rỗng khi không set env lúc build
    }

    #[test]
    fn ghi_config_roi_doc_lai_ra_dung_gia_tri() {
        // round-trip: parse -> ghi -> parse lại phải ra cùng Config (tất cả field TRỪ token vì token không ghi).
        let c = parse_config(DU).unwrap();
        let text = ghi_config(&c);
        let c2 = parse_config(&text).unwrap();
        // So sánh tất cả field TRỪ token (vì token từ file là "tok123" nhưng ghi_config không ghi → parse lại sẽ = AGENT_TOKEN = "")
        assert_eq!(c.server_url, c2.server_url);
        assert_eq!(c.org_id, c2.org_id);
        assert_eq!(c.printer_name, c2.printer_name);
        assert_eq!(c.tray, c2.tray);
        assert_eq!(c.paper_size, c2.paper_size);
        // token không so sánh vì ghi_config bỏ nó
    }
}
