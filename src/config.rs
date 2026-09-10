// SPDX-License-Identifier: AGPL-3.0-or-later
//! Đọc config.ini (định dạng INI đơn giản) thành struct Config.
//! Tự bỏ BOM đầu file — notepad Windows hay thêm BOM (bug thật đã gặp bản Python).

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub server_url: String,
    /// Token của MÁY NÀY — gen từ trang ZaloCRM (mỗi máy in 1 token riêng),
    /// đọc bắt buộc từ config.ini. KHÔNG còn hằng nhúng lúc build: server
    /// giờ định tuyến theo token (nhiều chi nhánh, nhiều máy), nên mỗi máy
    /// phải có token khác nhau — không thể nhúng chung 1 giá trị lúc build nữa.
    pub token: String,
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
        token: bat_buoc("token")?,
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
         token = {}\n\
         printer_name = {}\n\
         tray = {}\n\
         paper_size = {}\n",
        cfg.server_url, cfg.token, cfg.printer_name, cfg.tray, cfg.paper_size
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DU: &str = "[agent]\nserver_url = https://crm.example.com\ntoken = tok123\nprinter_name = HP LaserJet\n";

    #[test]
    fn parse_day_du_field() {
        let c = parse_config(DU).unwrap();
        assert_eq!(c.server_url, "https://crm.example.com");
        assert_eq!(c.token, "tok123");
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
    fn thieu_token_bao_loi() {
        // Token giờ BẮT BUỘC — mỗi máy 1 token riêng (server định tuyến theo
        // token), không còn hằng nhúng lúc build để fallback nữa.
        let e = parse_config("[agent]\nserver_url = https://crm.example.com\nprinter_name = HP LaserJet\n")
            .unwrap_err();
        assert!(e.to_string().contains("token"));
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
        let t = "; comment\n\n[agent]\n# ghi chu\nserver_url = u\ntoken = t\nprinter_name = p\n";
        let c = parse_config(t).unwrap();
        assert_eq!(c.server_url, "u");
    }

    #[test]
    fn file_cu_con_dong_org_id_van_doc_duoc_khong_loi() {
        // Tương thích ngược: file config.ini cũ (từ bản mã-shop/org_id) còn
        // dòng "org_id = ..." — dòng này chỉ bị BỎ QUA (không có field org_id
        // trong struct nữa), KHÔNG được làm parse lỗi.
        let t = "[agent]\nserver_url = https://crm.example.com\ntoken = tok123\norg_id = org1\nprinter_name = HP LaserJet\n";
        let c = parse_config(t).unwrap();
        assert_eq!(c.server_url, "https://crm.example.com");
        assert_eq!(c.token, "tok123");
        assert_eq!(c.printer_name, "HP LaserJet");
    }

    #[test]
    fn ghi_config_roi_doc_lai_ra_dung_gia_tri() {
        // round-trip: parse -> ghi -> parse lại phải ra cùng Config, KỂ CẢ
        // token (giờ token là dữ liệu thật của máy, ghi_config phải lưu lại
        // để không mất token khi app khởi động lại).
        let c = parse_config(DU).unwrap();
        let text = ghi_config(&c);
        let c2 = parse_config(&text).unwrap();
        assert_eq!(c, c2);
    }
}
