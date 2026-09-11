// SPDX-License-Identifier: AGPL-3.0-or-later
//! In PDF qua SumatraPDF (driver Windows). Dry-run: ghi PDF ra file thay vì in
//! (test trên mọi OS, không cần máy in).
//!
//! CHỐNG IN ĐÔI: `in_pdf` trả `KetQuaIn` (3 nhánh), KHÔNG phải Result<()> —
//! exit code 0 của SumatraPDF KHÔNG ĐỦ để suy "đã in thật" (Sumatra trả 0
//! ngay khi ĐÃ GỬI XONG cho spooler, chưa chắc đã in ra giấy). Sau khi gọi
//! Sumatra, gọi spooler::theo_doi_job để quan sát Windows print spooler thật
//! rồi mới quyết DaIn/Loi/KhongRo. Xem spooler.rs cho state machine chi tiết.

use crate::job::KetQuaIn;
use anyhow::Context;
use std::path::PathBuf;
use std::time::SystemTime;

/// Đường dẫn SumatraPDF mặc định (có thể đổi qua config sau nếu cần).
const SUMATRA_MAC_DINH: &str = "SumatraPDF.exe";

/// tray "tray-2" → `bin=Tray 2` cho SumatraPDF `-print-settings`.
///
/// VÌ SAO tên khay chứ không phải số: đo thật trên máy in HP LaserJet Pro 4003
/// (27/08) — `bin=2` bị máy PHỚT LỜ, in ra khay mặc định (A4); `bin=Tray 2`
/// (đúng tên khay Windows hiển thị) mới chọn đúng khay A5. tray-<n> → "Tray <n>";
/// giá trị không theo dạng tray-<n> → giữ nguyên (config ghi thẳng tên khay).
pub fn tray_sang_bin(tray: &str) -> String {
    if let Some(so) = tray.strip_prefix("tray-") {
        format!("bin=Tray {}", so)
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

/// Giữ lại a-z A-Z 0-9 - _ trong job_id để làm tên file an toàn (loại ký tự
/// lạ như "/", khoảng trắng, dấu ngoặc — job_id do server sinh, không tin
/// tưởng mù để ghép thẳng vào đường dẫn file).
fn sanitize_job_id(job_id: &str) -> String {
    let sach: String = job_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if sach.is_empty() {
        "unknown".to_string()
    } else {
        sach
    }
}

/// In pdf_bytes. DRY_RUN=1 → ghi ra file, KHÔNG gọi máy in, trả DaIn (giữ
/// hành vi test cũ). Thật (Windows): ghi PDF tạm (tên file CHỨA job_id để
/// spooler.rs match được DocumentName ↔ job_id), gọi SumatraPDF, RỒI poll
/// spooler xác nhận in thật — xem doc-comment module ở đầu file.
pub fn in_pdf(
    pdf_bytes: &[u8],
    printer: &str,
    paper_size: &str,
    tray: &str,
    copies: u32,
    job_id: &str,
) -> KetQuaIn {
    if dang_dry_run() {
        let dir = thu_muc_dry_run();
        if let Err(e) = std::fs::create_dir_all(&dir).context("tạo thư mục dry-run") {
            return KetQuaIn::Loi(format!("{:#}", e));
        }
        let ten = format!("in-{}-{}.pdf", sanitize_job_id(job_id), now_id());
        let path = dir.join(ten);
        return match std::fs::write(&path, pdf_bytes).context("ghi file dry-run") {
            Ok(()) => KetQuaIn::DaIn,
            Err(e) => KetQuaIn::Loi(format!("{:#}", e)),
        };
    }

    // Ghi PDF ra file tạm — TÊN FILE CHỨA job_id để spooler.rs (DocumentName
    // của JOB_INFO_2W chính là tên file SumatraPDF gửi cho spooler) match
    // đúng job này, không nhầm với job khác đang in đồng thời.
    let ten_file = format!("print-agent-{}-{}.pdf", sanitize_job_id(job_id), now_id());
    let tmp = std::env::temp_dir().join(ten_file);
    if let Err(e) = std::fs::write(&tmp, pdf_bytes).context("ghi PDF tạm") {
        return KetQuaIn::Loi(format!("{:#}", e));
    }

    let ket_qua = in_va_xac_nhan(&tmp, printer, paper_size, tray, copies, job_id);
    let _ = std::fs::remove_file(&tmp); // dọn file tạm dù thành công hay lỗi
    ket_qua
}

/// Gọi SumatraPDF cho từng bản copy rồi poll spooler xác nhận. Tách riêng để
/// `in_pdf` gọn — logic map exit-code/spooler nằm hết ở đây.
fn in_va_xac_nhan(
    tmp: &std::path::Path,
    printer: &str,
    paper_size: &str,
    tray: &str,
    copies: u32,
    job_id: &str,
) -> KetQuaIn {
    let sumatra = std::env::var("SUMATRA_PATH").unwrap_or_else(|_| SUMATRA_MAC_DINH.to_string());
    let submit_time = SystemTime::now();

    for lan in 0..copies.max(1) {
        let argv = lenh_in(&sumatra, tmp.to_str().unwrap_or(""), printer, paper_size, tray);
        let out = match std::process::Command::new(&argv[0]).args(&argv[1..]).output() {
            Ok(o) => o,
            Err(e) => {
                return KetQuaIn::Loi(format!("không gọi được SumatraPDF ({}): {}", sumatra, e))
            }
        };

        if !out.status.success() {
            // Exit code lỗi KHÔNG tự động = "chưa in": nếu spooler ĐÃ quan
            // sát job in (PRINTING) trước khi Sumatra trả lỗi (vd Sumatra
            // timeout đợi driver trả về nhưng máy vẫn in), suy Loi ở đây có
            // thể khiến server retry và IN ĐÔI. Luôn hỏi spooler để quyết,
            // KHÔNG override bằng exit code — đúng yêu cầu "KHÔNG để exit
            // code override evidence spooler".
            let kq_spooler = crate::spooler::theo_doi_job(printer, job_id, submit_time);
            return match kq_spooler {
                KetQuaIn::DaIn => KetQuaIn::DaIn,
                // Spooler cũng không có bằng chứng đã in → giờ mới an toàn
                // quy về lỗi Sumatra (đúng yêu cầu §3: "sumatra exit≠0 VÀ
                // spooler chưa từng observe PRINTING → Loi").
                _ => KetQuaIn::Loi(format!(
                    "SumatraPDF lỗi (exit {:?}): {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                )),
            };
        }

        // Sumatra exit 0 cho bản copy này — poll spooler xác nhận in THẬT.
        // copies>1: mỗi bản in là 1 job spooler riêng cùng tên file (Sumatra
        // gọi lại từ đầu mỗi vòng lặp) — chỉ bản CUỐI quyết định KetQuaIn trả
        // về caller; các bản giữa nếu KhongRo/Loi thì dừng ngay (không in
        // tiếp bản sau khi bản trước đã mơ hồ/lỗi, tránh in đôi/thiếu kiểm soát).
        let kq = crate::spooler::theo_doi_job(printer, job_id, submit_time);
        if !matches!(kq, KetQuaIn::DaIn) || lan == copies.max(1) - 1 {
            return kq;
        }
    }
    // copies=0 đã được max(1) chặn ở trên nên loop luôn chạy >=1 lần và trả
    // ở trong loop; nhánh này chỉ để thoả mãn kiểu trả về.
    KetQuaIn::KhongRo("khong co ban copy nao duoc in".into())
}

/// Id ngắn duy nhất cho tên file (không cần crypto, chỉ tránh trùng).
fn now_id() -> String {
    use std::time::UNIX_EPOCH;
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
        assert_eq!(tray_sang_bin("tray-2"), "bin=Tray 2");
        assert_eq!(tray_sang_bin("tray-1"), "bin=Tray 1");
    }

    #[test]
    fn lenh_in_a5_tray2_dung_argv() {
        let a = lenh_in("SumatraPDF.exe", "c:\\a.pdf", "HP LaserJet", "A5", "tray-2");
        assert_eq!(a[0], "SumatraPDF.exe");
        assert_eq!(a[1], "-print-to");
        assert_eq!(a[2], "HP LaserJet");
        assert_eq!(a[3], "-print-settings");
        assert_eq!(a[4], "paper=A5,bin=Tray 2");
        assert_eq!(a[5], "-silent");
        assert_eq!(a[6], "c:\\a.pdf");
    }

    #[test]
    fn dry_run_ghi_file_dung_noi_dung() {
        let dir = std::env::temp_dir().join(format!("pa-test-{}", now_id()));
        std::env::set_var("AGENT_DRY_RUN", "1");
        std::env::set_var("AGENT_DRY_RUN_DIR", dir.to_str().unwrap());
        let pdf = b"%PDF-1.4 noi dung test";
        let kq = in_pdf(pdf, "HP", "A5", "tray-2", 1, "job-abc");
        assert_eq!(kq, KetQuaIn::DaIn);
        // đúng 1 file, đúng nội dung, tên file chứa job_id (đã sanitize)
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(files.len(), 1);
        let ten = files[0].file_name().to_string_lossy().into_owned();
        assert!(ten.contains("job-abc"), "ten file {} phai chua job_id", ten);
        let doc = std::fs::read(files[0].path()).unwrap();
        assert_eq!(doc, pdf);
        std::env::remove_var("AGENT_DRY_RUN");
        std::env::remove_var("AGENT_DRY_RUN_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_job_id_loai_ky_tu_la() {
        assert_eq!(sanitize_job_id("abc-123_XYZ"), "abc-123_XYZ");
        assert_eq!(sanitize_job_id("a/b c*d"), "a_b_c_d");
        assert_eq!(sanitize_job_id(""), "unknown");
    }
}
