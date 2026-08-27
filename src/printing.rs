// SPDX-License-Identifier: AGPL-3.0-or-later
//! In PDF qua SumatraPDF (driver Windows). Dry-run: ghi PDF ra file thay vì in
//! (test trên mọi OS, không cần máy in). Lỗi in LUÔN trả Err — cấm im lặng,
//! để job phía server không kẹt "da_gui" (báo trạng thái "loi").

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// Đường dẫn SumatraPDF mặc định (có thể đổi qua config sau nếu cần).
const SUMATRA_MAC_DINH: &str = "SumatraPDF.exe";

/// tray "tray-2" → số bin cho SumatraPDF `-print-settings bin=<n>`.
/// tray-<n> → n. Không parse được → giữ nguyên chuỗi (SumatraPDF tự hiểu tên khay).
pub fn tray_sang_bin(tray: &str) -> String {
    if let Some(so) = tray.strip_prefix("tray-") {
        format!("bin={}", so)
    } else {
        format!("bin={}", tray)
    }
}

/// Dựng argv gọi SumatraPDF để in một PDF với khổ + khay chỉ định.
/// Tách riêng để test được argv mà không cần chạy tiến trình.
pub fn lenh_in(
    sumatra: &str,
    pdf_path: &str,
    printer: &str,
    paper_size: &str,
    tray: &str,
) -> Vec<String> {
    let settings = format!("paper={},{}", paper_size, tray_sang_bin(tray));
    vec![
        sumatra.to_string(),
        "-print-to".to_string(),
        printer.to_string(),
        "-print-settings".to_string(),
        settings,
        "-silent".to_string(),
        pdf_path.to_string(),
    ]
}

/// Có đang dry-run không (biến môi trường AGENT_DRY_RUN=1).
fn dang_dry_run() -> bool {
    std::env::var("AGENT_DRY_RUN").ok().as_deref() == Some("1")
}

fn thu_muc_dry_run() -> PathBuf {
    std::env::var("AGENT_DRY_RUN_DIR")
        .unwrap_or_else(|_| "dry-run-output".to_string())
        .into()
}

/// In pdf_bytes. DRY_RUN=1 → ghi ra file, KHÔNG gọi máy in.
/// Trả Ok(()) nếu in/ghi thành công; Err nếu lỗi (caller báo "loi" về server).
pub fn in_pdf(
    pdf_bytes: &[u8],
    printer: &str,
    paper_size: &str,
    tray: &str,
    copies: u32,
) -> Result<()> {
    if dang_dry_run() {
        let dir = thu_muc_dry_run();
        std::fs::create_dir_all(&dir).context("tạo thư mục dry-run")?;
        let ten = format!("in-{}.pdf", now_id());
        let path = dir.join(ten);
        std::fs::write(&path, pdf_bytes).context("ghi file dry-run")?;
        return Ok(());
    }

    // Ghi PDF ra file tạm rồi gọi SumatraPDF từng bản copy.
    let tmp = std::env::temp_dir().join(format!("print-agent-{}.pdf", now_id()));
    std::fs::write(&tmp, pdf_bytes).context("ghi PDF tạm")?;
    let ket_qua = (|| -> Result<()> {
        let sumatra = std::env::var("SUMATRA_PATH").unwrap_or_else(|_| SUMATRA_MAC_DINH.to_string());
        for _ in 0..copies.max(1) {
            let argv = lenh_in(&sumatra, tmp.to_str().unwrap(), printer, paper_size, tray);
            let out = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .with_context(|| format!("không gọi được SumatraPDF ({})", sumatra))?;
            if !out.status.success() {
                bail!(
                    "SumatraPDF lỗi (exit {:?}): {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp); // dọn file tạm dù thành công hay lỗi
    ket_qua
}

/// Id ngắn duy nhất cho tên file (không cần crypto, chỉ tránh trùng).
fn now_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray2_sang_bin2() {
        assert_eq!(tray_sang_bin("tray-2"), "bin=2");
        assert_eq!(tray_sang_bin("tray-1"), "bin=1");
    }

    #[test]
    fn lenh_in_a5_tray2_dung_argv() {
        let a = lenh_in("SumatraPDF.exe", "c:\\a.pdf", "HP LaserJet", "A5", "tray-2");
        assert_eq!(a[0], "SumatraPDF.exe");
        assert_eq!(a[1], "-print-to");
        assert_eq!(a[2], "HP LaserJet");
        assert_eq!(a[3], "-print-settings");
        assert_eq!(a[4], "paper=A5,bin=2");
        assert_eq!(a[5], "-silent");
        assert_eq!(a[6], "c:\\a.pdf");
    }

    #[test]
    fn dry_run_ghi_file_dung_noi_dung() {
        let dir = std::env::temp_dir().join(format!("pa-test-{}", now_id()));
        std::env::set_var("AGENT_DRY_RUN", "1");
        std::env::set_var("AGENT_DRY_RUN_DIR", dir.to_str().unwrap());
        let pdf = b"%PDF-1.4 noi dung test";
        in_pdf(pdf, "HP", "A5", "tray-2", 1).unwrap();
        // đúng 1 file, đúng nội dung
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(files.len(), 1);
        let doc = std::fs::read(files[0].path()).unwrap();
        assert_eq!(doc, pdf);
        std::env::remove_var("AGENT_DRY_RUN");
        std::env::remove_var("AGENT_DRY_RUN_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
