// SPDX-License-Identifier: AGPL-3.0-or-later
//! In PDF qua SumatraPDF (driver Windows). Dry-run: ghi PDF ra file thay vì in
//! (test trên mọi OS, không cần máy in).
//!
//! CHỐNG IN ĐÔI: `in_pdf` trả `KetQuaIn` (3 nhánh), KHÔNG phải Result<()> —
//! exit code 0 của SumatraPDF KHÔNG ĐỦ để suy "đã in thật" (Sumatra trả 0
//! ngay khi ĐÃ GỬI XONG cho spooler, chưa chắc đã in ra giấy). Sau khi gọi
//! Sumatra, gọi spooler::theo_doi_job để quan sát Windows print spooler thật
//! rồi mới quyết DaIn/Loi/KhongRo. Xem spooler.rs cho state machine chi tiết.

use crate::job::{self, KetQuaIn, LyDo};
use crate::nhat_ky;
use crate::spooler::QuanSat;
use crate::su_co::{MaSuCo, TapMa};
use anyhow::Context;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

/// Đường dẫn SumatraPDF mặc định (có thể đổi qua config sau nếu cần).
const SUMATRA_MAC_DINH: &str = "SumatraPDF.exe";

/// Chờ SumatraPDF tối đa chừng này (R8). Bản trước `.output()` chờ VÔ HẠN:
/// Sumatra treo (hộp thoại driver, máy in chia sẻ mất mạng…) là worker in
/// treo theo, mọi hoá đơn sau đó đứng hàng mãi.
pub const HAN_SUMATRA: Duration = Duration::from_secs(60);
const CHU_KY_CHO_SUMATRA: Duration = Duration::from_millis(200);

/// Tiến trình con chờ được có hạn — trait để test `cho_co_han` không cần tiến trình thật.
pub trait TienTrinh {
    /// `Ok(None)` = còn chạy.
    fn thu_cho(&mut self) -> std::io::Result<Option<KetThuc>>;
    /// Giết + thu dọn (best-effort).
    fn giet(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KetThuc {
    pub thanh_cong: bool,
    pub ma: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KetQuaCho {
    Xong(KetThuc),
    /// Quá hạn — đã giết tiến trình.
    QuaHan,
    /// Không hỏi được trạng thái tiến trình — đã giết cho chắc.
    LoiCho(String),
}

impl TienTrinh for std::process::Child {
    fn thu_cho(&mut self) -> std::io::Result<Option<KetThuc>> {
        Ok(self.try_wait()?.map(|s| KetThuc { thanh_cong: s.success(), ma: s.code() }))
    }
    fn giet(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}

/// Chờ tiến trình xong, tối đa `han` (đếm theo tổng thời gian đã ngủ — tất
/// định, test được), hỏi mỗi `chu_ky`. Quá hạn hoặc không hỏi được → giết.
pub fn cho_co_han(p: &mut dyn TienTrinh, han: Duration, chu_ky: Duration, ngu: &mut dyn FnMut(Duration)) -> KetQuaCho {
    let mut da_cho = Duration::ZERO;
    loop {
        match p.thu_cho() {
            Ok(Some(k)) => return KetQuaCho::Xong(k),
            Ok(None) => {}
            Err(e) => {
                p.giet();
                return KetQuaCho::LoiCho(e.to_string());
            }
        }
        if da_cho >= han {
            p.giet();
            return KetQuaCho::QuaHan;
        }
        ngu(chu_ky);
        da_cho += chu_ky;
    }
}

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
pub fn dang_dry_run() -> bool {
    std::env::var("AGENT_DRY_RUN").ok().as_deref() == Some("1")
}

fn thu_muc_dry_run() -> PathBuf {
    std::env::var("AGENT_DRY_RUN_DIR")
        .unwrap_or_else(|_| "dry-run-output".to_string())
        .into()
}

/// Trần độ dài tên file lấy từ server — đường dẫn tạm Windows + tên phải còn
/// dưới MAX_PATH (260).
const TEN_FILE_TOI_DA: usize = 200;

/// Tên file PDF tạm cho một job.
///
/// Server gửi `name` (backend dựng "AI-INV_2026_030045-<Ten_Khach>-<jobId>.pdf")
/// thì DÙNG NÓ, sau khi lọc: chỉ giữ A–Z a–z 0–9 `-` `_` `.` (loại mọi ký tự
/// Windows cấm và dấu phân cách thư mục — không tin tưởng mù chuỗi từ mạng),
/// bảo đảm đuôi `.pdf`. RƠI VỀ tên cũ `print-agent-<job_id>-<id>.pdf` khi:
///   - server không gửi `name` (backend bản cũ), hoặc rỗng sau khi lọc;
///   - tên KHÔNG CHỨA job_id — spooler.rs nhận ra job trong hàng đợi Windows
///     bằng DocumentName CHỨA job_id; thiếu nó là không bao giờ xác nhận được
///     "đã in", job treo khong_ro;
///   - tên dài quá TEN_FILE_TOI_DA.
pub fn ten_file_in(job_id: &str, ten_goi_y: Option<&str>) -> String {
    if let Some(ten) = ten_goi_y {
        let sach: String = ten
            .trim()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
            .collect();
        let sach = sach.trim_start_matches('.').to_string();
        let co_duoi = if sach.to_ascii_lowercase().ends_with(".pdf") { sach } else { format!("{}.pdf", sach) };
        if !job_id.is_empty() && co_duoi.len() > ".pdf".len() && co_duoi.contains(job_id) && co_duoi.len() <= TEN_FILE_TOI_DA {
            return co_duoi;
        }
    }
    format!("print-agent-{}-{}.pdf", sanitize_job_id(job_id), now_id())
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
///
/// `bao`: nhận NGAY mọi điều spooler quan sát được trong lúc theo dõi (sự cố,
/// trạng thái máy in) — net.rs dùng để gửi `su-co` không chờ hết 15 giây.
///
/// `nen`: cờ CẤP MÁY chụp ở bước kiểm TRƯỚC khi in (R-B, T3). `None` (nút "In
/// thử" — không qua bước kiểm của worker) → chụp ngay tại đây, vẫn TRƯỚC Sumatra.
#[allow(clippy::too_many_arguments)]
pub fn in_pdf(
    pdf_bytes: &[u8],
    printer: &str,
    paper_size: &str,
    tray: &str,
    copies: u32,
    job_id: &str,
    ten_goi_y: Option<&str>,
    nen: Option<TapMa>,
    bao: &dyn Fn(QuanSat),
) -> KetQuaIn {
    if dang_dry_run() {
        let dir = thu_muc_dry_run();
        if let Err(e) = std::fs::create_dir_all(&dir).context("tạo thư mục dry-run") {
            return KetQuaIn::Loi(format!("{:#}", e).into());
        }
        let path = dir.join(ten_file_in(job_id, ten_goi_y));
        return match std::fs::write(&path, pdf_bytes).context("ghi file dry-run") {
            Ok(()) => KetQuaIn::DaIn,
            Err(e) => KetQuaIn::Loi(format!("{:#}", e).into()),
        };
    }

    // Ghi PDF ra file tạm — TÊN FILE CHỨA job_id để spooler.rs (DocumentName
    // của JOB_INFO_2W chính là tên file SumatraPDF gửi cho spooler) match
    // đúng job này, không nhầm với job khác đang in đồng thời. Tên lấy theo
    // `name` server gửi nếu hợp lệ (xem `ten_file_in`).
    let tmp = std::env::temp_dir().join(ten_file_in(job_id, ten_goi_y));
    if let Err(e) = std::fs::write(&tmp, pdf_bytes).context("ghi PDF tạm") {
        return KetQuaIn::Loi(format!("{:#}", e).into());
    }

    // Cờ nền phải chụp TRƯỚC khi Sumatra chạy (T3): sự cố bắt đầu trong lúc
    // Sumatra chạy (1–60 s) là sự cố MỚI, không phải "có từ trước job".
    let nen = nen.unwrap_or_else(|| crate::spooler::chup_nen_may_in(printer));
    let ket_qua = in_va_xac_nhan(&tmp, printer, paper_size, tray, copies, job_id, HAN_SUMATRA, |p, j, t| {
        crate::spooler::theo_doi_job(p, j, t, nen, bao)
    });
    let _ = std::fs::remove_file(&tmp); // dọn file tạm dù thành công hay lỗi
    ket_qua
}

/// Gọi SumatraPDF cho từng bản copy rồi poll spooler xác nhận. Tách riêng để
/// `in_pdf` gọn — logic map exit-code/spooler nằm hết ở đây.
///
/// `hoi_spooler` TIÊM ĐƯỢC (thật = `spooler::theo_doi_job`, test = closure giả
/// trả `KetQuaIn` bất kỳ) — ĐÂY LÀ CHỖ BẮT BUỘC PHẢI TIÊM ĐƯỢC: bug tìm thấy ở
/// review round 1 (KhongRo bị nhánh `_ =>` nuốt thành Loi khi Sumatra exit≠0)
/// chỉ lộ ra được khi test giả lập spooler trả KhongRo — không gọi Win32 thật
/// thì không bao giờ tái tạo được ca này trên Mac/CI.
#[allow(clippy::too_many_arguments)]
fn in_va_xac_nhan(
    tmp: &std::path::Path,
    printer: &str,
    paper_size: &str,
    tray: &str,
    copies: u32,
    job_id: &str,
    han_sumatra: Duration,
    hoi_spooler: impl Fn(&str, &str, SystemTime) -> KetQuaIn,
) -> KetQuaIn {
    let sumatra = std::env::var("SUMATRA_PATH").unwrap_or_else(|_| SUMATRA_MAC_DINH.to_string());
    let submit_time = SystemTime::now();

    for lan in 0..copies.max(1) {
        let argv = lenh_in(&sumatra, tmp.to_str().unwrap_or(""), printer, paper_size, tray);
        // spawn + chờ có hạn (R8) thay cho `.output()` chờ vô hạn. stdout bỏ
        // (Sumatra -silent không in gì), stderr giữ để ghi lý do khi lỗi.
        let mut tien_trinh = match Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            // Không chạy được Sumatra thì chưa có gì tới spooler — Loi an toàn.
            // Riêng từ bản sao thứ hai trở đi: bản trước ĐÃ in, xem `sau_ban_da_in`.
            Err(e) => {
                let ly_do = LyDo::co_loai(format!("không gọi được SumatraPDF ({}): {}", sumatra, e), MaSuCo::LoiSumatra);
                return sau_ban_da_in(lan, copies, KetQuaIn::Loi(ly_do));
            }
        };

        let mo_ta_loi = match cho_co_han(&mut tien_trinh, han_sumatra, CHU_KY_CHO_SUMATRA, &mut |d| std::thread::sleep(d)) {
            KetQuaCho::Xong(k) if k.thanh_cong => None,
            KetQuaCho::Xong(k) => {
                let mut stderr = String::new();
                if let Some(mut e) = tien_trinh.stderr.take() {
                    let _ = e.read_to_string(&mut stderr);
                }
                Some(format!("SumatraPDF lỗi (exit {:?}): {}", k.ma, stderr.trim()))
            }
            // Quá hạn: Sumatra có thể ĐÃ đẩy job vào spooler rồi mới treo —
            // KHÔNG tự kết luận Loi, vẫn hỏi spooler như nhánh exit≠0.
            KetQuaCho::QuaHan => {
                nhat_ky::ghi(
                    "sumatra_qua_han",
                    &format!("job={} han={}s — da dung tien trinh, van theo doi spooler", job::rut_gon_job_id(job_id), han_sumatra.as_secs()),
                );
                Some(format!("SumatraPDF quá {} s chưa xong — đã dừng tiến trình", han_sumatra.as_secs()))
            }
            KetQuaCho::LoiCho(e) => {
                nhat_ky::ghi("sumatra_loi_cho", &format!("job={} {}", job::rut_gon_job_id(job_id), e));
                Some(format!("không chờ được SumatraPDF: {}", e))
            }
        };

        if let Some(mo_ta_loi) = mo_ta_loi {
            // Sumatra lỗi/quá hạn KHÔNG tự động = "chưa in": nếu spooler ĐÃ quan
            // sát job in (PRINTING) trước khi Sumatra trả lỗi (vd Sumatra
            // timeout đợi driver trả về nhưng máy vẫn in), suy Loi ở đây có
            // thể khiến server retry và IN ĐÔI. Luôn hỏi spooler để quyết,
            // KHÔNG override bằng exit code — đúng yêu cầu "KHÔNG để exit
            // code override evidence spooler".
            //
            // BA nhánh riêng biệt — KHÔNG gộp `_ =>` (bug đã sửa ở review
            // round 1): KhongRo phải CHẢY NGUYÊN VẸN ra ngoài, tuyệt đối
            // không bị quy thành Loi chỉ vì "không phải DaIn". Quy Loi ở
            // nhánh KhongRo sẽ khiến server coi là an toàn để retry —
            // trong khi máy có thể ĐÃ nhả giấy (PRINTING quan sát được)
            // rồi mất dấu — retry lúc đó CHÍNH LÀ IN ĐÔI.
            let kq = match hoi_spooler(printer, job_id, submit_time) {
                KetQuaIn::DaIn => KetQuaIn::DaIn,
                // Spooler không thấy sự cố vật lý nào (chỉ "không xác nhận
                // được") → nguyên nhân nhiều khả năng là chính Sumatra: gắn
                // `loi_sumatra` để NV thấy đúng chỗ cần sửa. Vẫn KhongRo.
                KetQuaIn::KhongRo(mut ly_do) => {
                    if ly_do.loai == Some(MaSuCo::KhongXacNhan) {
                        ly_do.loai = Some(MaSuCo::LoiSumatra);
                    }
                    KetQuaIn::KhongRo(ly_do)
                }
                // Spooler trả Loi = chính app đã xoá job SẠCH khỏi hàng đợi và
                // kiểm lại là hết (đường duy nhất ra Loi, spooler.rs) → an toàn
                // quy về lỗi. Giữ mã sự cố spooler thấy (cụ thể hơn), không có
                // mới là loi_sumatra.
                KetQuaIn::Loi(ly_do) => KetQuaIn::Loi(LyDo {
                    chu: format!("{}; spooler: {}", mo_ta_loi, ly_do.chu),
                    loai: ly_do.loai.or(Some(MaSuCo::LoiSumatra)),
                    ban_da_in: None,
                }),
            };
            return sau_ban_da_in(lan, copies, kq);
        }

        // Sumatra exit 0 cho bản copy này — poll spooler xác nhận in THẬT.
        // copies>1: mỗi bản in là 1 job spooler riêng cùng tên file (Sumatra
        // gọi lại từ đầu mỗi vòng lặp) — chỉ bản CUỐI quyết định KetQuaIn trả
        // về caller; các bản giữa nếu KhongRo/Loi thì dừng ngay (không in
        // tiếp bản sau khi bản trước đã mơ hồ/lỗi, tránh in đôi/thiếu kiểm soát).
        let kq = hoi_spooler(printer, job_id, submit_time);
        if !matches!(kq, KetQuaIn::DaIn) || lan == copies.max(1) - 1 {
            return sau_ban_da_in(lan, copies, kq);
        }
    }
    // copies=0 đã được max(1) chặn ở trên nên loop luôn chạy >=1 lần và trả
    // ở trong loop; nhánh này chỉ để thoả mãn kiểu trả về.
    KetQuaIn::KhongRo("khong co ban copy nao duoc in".into())
}

/// Bản sao thứ `lan` (đếm từ 0) ra Loi mà các bản TRƯỚC đã in xong → KhongRo.
///
/// Server nhận Loi là gửi lại CẢ job (đủ `copies` bản) — các bản đã ra giấy sẽ
/// ra thêm lần nữa, đúng thứ §0.1 cấm. Bản đầu tiên (`lan == 0`) Loi thì chưa
/// có tờ nào ra: giữ Loi.
///
/// T9 (giám sát vòng 3): `Loi` của bản sau nghĩa là CHÍNH app đã gỡ bản đó khỏi
/// hàng đợi (hoặc chưa gửi được xuống) — nó CHƯA in và không còn ở đâu cả.
/// Câu cũ ("da in xong 1 ban truoc do") + `conTrongHangDoi:false` làm dải nói
/// "có thể đang nằm trong máy in" — sai. Nay câu nói đúng số bản đã/chưa in,
/// và mang `ban_da_in` để giao diện hiện đúng câu đó.
fn sau_ban_da_in(lan: u32, copies: u32, kq: KetQuaIn) -> KetQuaIn {
    match kq {
        KetQuaIn::Loi(ly_do) if lan > 0 => {
            let tong = copies.max(1);
            let o_dau = if ly_do.loai == Some(MaSuCo::LoiSumatra) { "chưa gửi xuống máy in" } else { "đã gỡ khỏi hàng đợi" };
            KetQuaIn::KhongRo(LyDo {
                chu: format!("{} ({}): {}", chu_thieu_ban(lan, tong), o_dau, ly_do.chu),
                loai: ly_do.loai,
                ban_da_in: Some((lan, tong)),
            })
        }
        kq => kq,
    }
}

/// "Đã in <k>/<n> bản — bản còn lại CHƯA in" (T9) — dùng chung cho `loiCuoi`,
/// dải cảnh báo và "In gần đây".
pub fn chu_thieu_ban(da_in: u32, tong: u32) -> String {
    format!("Đã in {}/{} bản — bản còn lại CHƯA in", da_in, tong)
}

/// PDF 1 trang A5 HỢP LỆ tối thiểu cho nút "In thử" (R11d).
///
/// Bản trước là `b"%PDF-1.4\n% in thu…"` — không có đối tượng, không có xref:
/// Sumatra từ chối mở, nút "In thử" không bao giờ in được gì (lỗi 13.5). Dựng
/// bằng code để bảng xref tính ĐÚNG offset từng đối tượng (test khoá).
pub fn pdf_in_thu() -> Vec<u8> {
    let noi_dung = "BT /F1 20 Tf 40 520 Td (In thu - Incokit Print Agent) Tj 0 -30 Td /F1 12 Tf (Neu thay trang nay, may in da nhan lenh.) Tj ET";
    let doi_tuong = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        // A5 dọc = 148 × 210 mm ≈ 420 × 595 pt.
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 595] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        format!("<< /Length {} >>\nstream\n{}\nendstream", noi_dung.len(), noi_dung),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offset = Vec::with_capacity(doi_tuong.len());
    for (i, dt) in doi_tuong.iter().enumerate() {
        offset.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, dt).as_bytes());
    }
    let xref = pdf.len();
    // Mỗi dòng xref đúng 20 byte: 10 số + ' ' + 5 số + ' ' + n/f + " \n".
    let mut bang = format!("xref\n0 {}\n0000000000 65535 f \n", doi_tuong.len() + 1);
    for o in &offset {
        bang.push_str(&format!("{:010} 00000 n \n", o));
    }
    bang.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        doi_tuong.len() + 1,
        xref
    ));
    pdf.extend_from_slice(bang.as_bytes());
    pdf
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
    use std::sync::Mutex;

    /// SUMATRA_PATH là biến môi trường TOÀN TIẾN TRÌNH — cargo test chạy
    /// nhiều test song song trong cùng 1 process, nên 3 test set/unset biến
    /// này PHẢI khoá tuần tự với nhau (không cần khoá với test khác vì không
    /// test nào khác đọc/ghi SUMATRA_PATH).
    static SUMATRA_PATH_LOCK: Mutex<()> = Mutex::new(());

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
        let kq = in_pdf(pdf, "HP", "A5", "tray-2", 1, "job-abc", None, None, &|_| {});
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
    fn ten_file_in_dung_name_server_gui_neu_chua_job_id() {
        let id = "tokHN-1727170000000-3";
        let name = format!("AI-INV_2026_030045-Anh_Loc_Beco_Thanh_Hoa-{}.pdf", id);
        assert_eq!(ten_file_in(id, Some(&name)), name);
    }

    #[test]
    fn ten_file_in_loc_ky_tu_cam_va_them_duoi_pdf() {
        let id = "j1";
        // "/" "\\" ":" "*" và chữ có dấu → "_"; thiếu .pdf → thêm
        assert_eq!(ten_file_in(id, Some("AI-INV/2026:0*1-Lộc-j1")), "AI-INV_2026_0_1-L_c-j1.pdf");
        assert_eq!(ten_file_in(id, Some("..\\..\\x-j1.pdf")), "_.._x-j1.pdf");
    }

    #[test]
    fn ten_file_in_roi_ve_ten_cu_khi_khong_co_hoac_khong_chua_job_id() {
        let id = "job-xyz";
        for ten in [None, Some(""), Some("AI-INV_1-Khach.pdf")] {
            let t = ten_file_in(id, ten);
            assert!(t.starts_with("print-agent-job-xyz-") && t.ends_with(".pdf"), "{:?} -> {}", ten, t);
        }
        let dai = format!("{}-{}.pdf", "A".repeat(TEN_FILE_TOI_DA), id);
        assert!(ten_file_in(id, Some(&dai)).starts_with("print-agent-"));
    }

    #[test]
    fn dry_run_dung_name_server_gui() {
        let dir = std::env::temp_dir().join(format!("pa-test-name-{}", now_id()));
        std::env::set_var("AGENT_DRY_RUN", "1");
        std::env::set_var("AGENT_DRY_RUN_DIR", dir.to_str().unwrap());
        let kq = in_pdf(b"%PDF-1.4", "HP", "A5", "tray-2", 1, "j9", Some("AI-INV_2026_030045-Chi_Muoi-j9.pdf"), None, &|_| {});
        assert_eq!(kq, KetQuaIn::DaIn);
        let ten: Vec<String> = std::fs::read_dir(&dir).unwrap()
            .filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(ten, vec!["AI-INV_2026_030045-Chi_Muoi-j9.pdf".to_string()]);
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

    /// Sinh 1 script/batch nhỏ LUÔN exit 1 bất kể argv nhận được — dùng làm
    /// "SumatraPDF giả" để test `in_va_xac_nhan` không cần binary in ấn thật.
    /// KHÔNG dùng "cmd" trực tiếp: cmd.exe không argv sẽ mở shell tương tác
    /// và TREO MÃI chờ stdin — nguy hiểm hơn cả không đúng, phải tự sinh
    /// script có `exit 1` tường minh rồi trỏ SUMATRA_PATH vào đó.
    fn tao_lenh_luon_that_bai() -> PathBuf {
        tao_lenh_thoat(1)
    }

    /// Như `tao_lenh_luon_that_bai` nhưng chọn được exit code (0 = Sumatra in ổn).
    fn tao_lenh_thoat(ma: u8) -> PathBuf {
        if cfg!(windows) {
            let p = std::env::temp_dir().join(format!("pa-test-exit{}-{}.cmd", ma, now_id()));
            std::fs::write(&p, format!("@echo off\r\nexit /b {}\r\n", ma)).unwrap();
            p
        } else {
            let p = std::env::temp_dir().join(format!("pa-test-exit{}-{}.sh", ma, now_id()));
            std::fs::write(&p, format!("#!/bin/sh\nexit {}\n", ma)).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&p).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&p, perms).unwrap();
            }
            p
        }
    }

    /// TÁI TẠO BUG review round 1: Sumatra exit≠0 NHƯNG spooler đã quan sát
    /// "đã bắt đầu in rồi mất dấu" (KhongRo) — trước fix, nhánh `_ =>` gộp
    /// KhongRo thành Loi ở đây, khiến server coi là an toàn để retry trong
    /// khi máy có thể ĐÃ nhả giấy → IN ĐÔI. Sau fix: PHẢI giữ nguyên KhongRo.
    #[test]
    fn sumatra_that_bai_nhung_spooler_khong_ro_thi_giu_khong_ro_khong_duoc_thanh_loi() {
        let _guard = SUMATRA_PATH_LOCK.lock().unwrap();
        let script = tao_lenh_luon_that_bai();
        let tmp = std::env::temp_dir().join(format!("pa-test-tmp-{}.pdf", now_id()));
        std::fs::write(&tmp, b"%PDF-1.4").unwrap();
        std::env::set_var("SUMATRA_PATH", &script);

        let kq = in_va_xac_nhan(&tmp, "HP", "A5", "tray-1", 1, "job-x", HAN_SUMATRA, |_p, _j, _t| {
            KetQuaIn::KhongRo("da bat dau in nhung khong xac nhan duoc luc in xong".into())
        });

        std::env::remove_var("SUMATRA_PATH");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&script);

        assert!(
            matches!(kq, KetQuaIn::KhongRo(_)),
            "BUG TAI XUAT HIEN: Sumatra exit!=0 + spooler KhongRo phai giu KhongRo (khong emit), \
             KHONG duoc quy thanh Loi (retry = in doi). Got: {:?}",
            kq
        );
    }

    /// Đối chứng: Sumatra exit≠0 VÀ spooler CŨNG trả Loi rõ ràng (chưa từng
    /// thấy PRINTING) → mới an toàn quy về Loi (retry được, chưa in gì).
    #[test]
    fn sumatra_that_bai_va_spooler_loi_ro_rang_thi_tra_loi() {
        let _guard = SUMATRA_PATH_LOCK.lock().unwrap();
        let script = tao_lenh_luon_that_bai();
        let tmp = std::env::temp_dir().join(format!("pa-test-tmp-{}.pdf", now_id()));
        std::fs::write(&tmp, b"%PDF-1.4").unwrap();
        std::env::set_var("SUMATRA_PATH", &script);

        let kq = in_va_xac_nhan(&tmp, "HP", "A5", "tray-1", 1, "job-y", HAN_SUMATRA, |_p, _j, _t| {
            KetQuaIn::Loi("may in offline tu truoc".into())
        });

        std::env::remove_var("SUMATRA_PATH");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&script);

        assert!(matches!(kq, KetQuaIn::Loi(_)), "expect Loi, got {:?}", kq);
    }

    /// Đối chứng: Sumatra exit≠0 nhưng spooler lại có bằng chứng DaIn (race
    /// hiếm — Sumatra trả lỗi trễ sau khi máy đã in xong) → vẫn phải DaIn,
    /// không được hạ xuống Loi/KhongRo.
    #[test]
    fn sumatra_that_bai_nhung_spooler_thay_da_in_thi_tra_da_in() {
        let _guard = SUMATRA_PATH_LOCK.lock().unwrap();
        let script = tao_lenh_luon_that_bai();
        let tmp = std::env::temp_dir().join(format!("pa-test-tmp-{}.pdf", now_id()));
        std::fs::write(&tmp, b"%PDF-1.4").unwrap();
        std::env::set_var("SUMATRA_PATH", &script);

        let kq = in_va_xac_nhan(&tmp, "HP", "A5", "tray-1", 1, "job-z", HAN_SUMATRA, |_p, _j, _t| KetQuaIn::DaIn);

        std::env::remove_var("SUMATRA_PATH");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&script);

        assert_eq!(kq, KetQuaIn::DaIn);
    }

    /// Chạy `in_va_xac_nhan` với Sumatra giả thoát `ma_thoat` và spooler giả
    /// trả lần lượt `ket_qua_spooler` (mỗi bản sao hỏi spooler một lần).
    fn in_voi(ma_thoat: u8, copies: u32, ket_qua_spooler: Vec<KetQuaIn>) -> (KetQuaIn, usize) {
        let _guard = SUMATRA_PATH_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let script = tao_lenh_thoat(ma_thoat);
        let tmp = std::env::temp_dir().join(format!("pa-test-tmp-{}.pdf", now_id()));
        std::fs::write(&tmp, b"%PDF-1.4").unwrap();
        std::env::set_var("SUMATRA_PATH", &script);
        let con = Mutex::new(ket_qua_spooler.into_iter());
        let so_lan = Mutex::new(0usize);
        let kq = in_va_xac_nhan(&tmp, "HP", "A5", "tray-1", copies, "job-c", HAN_SUMATRA, |_p, _j, _t| {
            *so_lan.lock().unwrap() += 1;
            con.lock().unwrap().next().expect("hoi spooler qua so lan du kien")
        });
        std::env::remove_var("SUMATRA_PATH");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&script);
        let n = *so_lan.lock().unwrap();
        (kq, n)
    }

    /// §0.1 cho copies > 1: bản 1 ĐÃ in, bản 2 lỗi trước khi in. Trả Loi là
    /// server gửi lại CẢ job → bản 1 ra lần nữa. Phải là KhongRo.
    #[test]
    fn ban_sau_loi_khi_ban_truoc_da_in_thi_khong_ro_khong_phai_loi() {
        let (kq, n) = in_voi(0, 2, vec![KetQuaIn::DaIn, KetQuaIn::Loi(LyDo::co_loai("loi truoc khi in", MaSuCo::HetGiay))]);
        assert_eq!(n, 2);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::HetGiay)),
            "bản trước đã ra giấy — Loi ở đây là in đôi: {:?}", kq);
    }

    /// T9: bản 2/3 bị app gỡ sạch khỏi hàng đợi (DaGoXong → Loi) sau khi bản 1
    /// đã in → KhongRo nói ĐÚNG "Đã in 1/3 bản — bản còn lại CHƯA in (đã gỡ
    /// khỏi hàng đợi)", mang `ban_da_in` (không phải "có thể trong máy in").
    #[test]
    fn t9_ban_sau_bi_go_thi_noi_dung_so_ban_da_in() {
        let go = LyDo::co_loai("loi truoc khi in: Hết giấy (da xoa job khoi hang doi Windows)", MaSuCo::HetGiay);
        let (kq, n) = in_voi(0, 3, vec![KetQuaIn::DaIn, KetQuaIn::Loi(go)]);
        assert_eq!(n, 2, "bản 2 lỗi thì không gửi bản 3");
        let KetQuaIn::KhongRo(l) = kq else { panic!("{:?}", kq) };
        assert!(l.chu.starts_with("Đã in 1/3 bản — bản còn lại CHƯA in (đã gỡ khỏi hàng đợi)"), "{}", l.chu);
        assert_eq!((l.loai, l.ban_da_in), (Some(MaSuCo::HetGiay), Some((1, 3))));
        // Sumatra không chạy được ở bản 2: bản đó chưa hề gửi xuống
        let KetQuaIn::KhongRo(l) = sau_ban_da_in(1, 2, KetQuaIn::Loi(LyDo::co_loai("x", MaSuCo::LoiSumatra))) else { panic!() };
        assert!(l.chu.starts_with("Đã in 1/2 bản — bản còn lại CHƯA in (chưa gửi xuống máy in)"), "{}", l.chu);
        // bản đầu lỗi / bản sau KhongRo thường: không đụng
        assert_eq!(sau_ban_da_in(0, 2, KetQuaIn::Loi("x".into())), KetQuaIn::Loi("x".into()));
        let kr = KetQuaIn::KhongRo(LyDo::co_loai("y", MaSuCo::KetGiay));
        assert_eq!(sau_ban_da_in(1, 2, kr.clone()), kr);
    }

    #[test]
    fn ban_dau_loi_thi_van_loi() {
        let (kq, n) = in_voi(0, 2, vec![KetQuaIn::Loi(LyDo::co_loai("x", MaSuCo::HetGiay))]);
        assert_eq!(n, 1, "bản đầu lỗi thì dừng, không in bản sau");
        assert!(matches!(kq, KetQuaIn::Loi(_)), "{:?}", kq);
    }

    #[test]
    fn nhieu_ban_deu_in_duoc_thi_da_in() {
        let (kq, n) = in_voi(0, 3, vec![KetQuaIn::DaIn, KetQuaIn::DaIn, KetQuaIn::DaIn]);
        assert_eq!((kq, n), (KetQuaIn::DaIn, 3));
    }

    #[test]
    fn sumatra_loi_gan_ma_loi_sumatra_khi_spooler_khong_biet_gi_hon() {
        // spooler Loi không mã → loi_sumatra
        let (kq, _) = in_voi(1, 1, vec![KetQuaIn::Loi("khong co trong hang doi".into())]);
        assert!(matches!(kq, KetQuaIn::Loi(ref l) if l.loai == Some(MaSuCo::LoiSumatra)), "{:?}", kq);
        // spooler Loi có mã cụ thể → giữ mã đó
        let (kq, _) = in_voi(1, 1, vec![KetQuaIn::Loi(LyDo::co_loai("x", MaSuCo::Offline))]);
        assert!(matches!(kq, KetQuaIn::Loi(ref l) if l.loai == Some(MaSuCo::Offline)), "{:?}", kq);
        // spooler KhongRo "không xác nhận được" → vẫn KhongRo, mã loi_sumatra
        let (kq, _) = in_voi(1, 1, vec![KetQuaIn::KhongRo(LyDo::co_loai("het gio", MaSuCo::KhongXacNhan))]);
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::LoiSumatra)), "{:?}", kq);
    }

    // --- R8: SumatraPDF có hạn ---

    /// Tiến trình giả: xong ở lần hỏi thứ `xong_o` (None = không bao giờ).
    struct TienTrinhGia {
        xong_o: Option<usize>,
        loi_hoi: bool,
        so_lan_hoi: usize,
        da_giet: bool,
    }
    impl TienTrinh for TienTrinhGia {
        fn thu_cho(&mut self) -> std::io::Result<Option<KetThuc>> {
            self.so_lan_hoi += 1;
            if self.loi_hoi {
                return Err(std::io::Error::other("hong"));
            }
            Ok((Some(self.so_lan_hoi) == self.xong_o).then_some(KetThuc { thanh_cong: true, ma: Some(0) }))
        }
        fn giet(&mut self) {
            self.da_giet = true;
        }
    }

    #[test]
    fn cho_co_han_xong_truoc_han_thi_khong_giet() {
        let mut p = TienTrinhGia { xong_o: Some(3), loi_hoi: false, so_lan_hoi: 0, da_giet: false };
        let mut da_ngu = Duration::ZERO;
        let kq = cho_co_han(&mut p, Duration::from_secs(60), Duration::from_millis(200), &mut |d| da_ngu += d);
        assert_eq!(kq, KetQuaCho::Xong(KetThuc { thanh_cong: true, ma: Some(0) }));
        assert!(!p.da_giet);
        assert_eq!(da_ngu, Duration::from_millis(400));
    }

    #[test]
    fn cho_co_han_qua_han_thi_giet() {
        let mut p = TienTrinhGia { xong_o: None, loi_hoi: false, so_lan_hoi: 0, da_giet: false };
        let mut da_ngu = Duration::ZERO;
        let kq = cho_co_han(&mut p, Duration::from_secs(60), Duration::from_millis(200), &mut |d| da_ngu += d);
        assert_eq!(kq, KetQuaCho::QuaHan);
        assert!(p.da_giet, "quá hạn phải giết Sumatra");
        assert_eq!(da_ngu, Duration::from_secs(60), "chờ ĐÚNG hạn rồi mới bỏ");
    }

    #[test]
    fn cho_co_han_khong_hoi_duoc_thi_giet() {
        let mut p = TienTrinhGia { xong_o: None, loi_hoi: true, so_lan_hoi: 0, da_giet: false };
        assert!(matches!(cho_co_han(&mut p, HAN_SUMATRA, CHU_KY_CHO_SUMATRA, &mut |_| {}), KetQuaCho::LoiCho(_)));
        assert!(p.da_giet);
    }

    /// Sumatra treo quá hạn: bị giết, NHƯNG vẫn hỏi spooler (job có thể đã
    /// spool) và KHÔNG tự kết luận Loi.
    #[test]
    fn sumatra_qua_han_van_hoi_spooler_khong_tu_ket_luan_loi() {
        let _guard = SUMATRA_PATH_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let script = tao_lenh_treo();
        let tmp = std::env::temp_dir().join(format!("pa-test-tmp-{}.pdf", now_id()));
        std::fs::write(&tmp, b"%PDF-1.4").unwrap();
        std::env::set_var("SUMATRA_PATH", &script);
        let da_hoi = std::sync::atomic::AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        let kq = in_va_xac_nhan(&tmp, "HP", "A5", "tray-1", 1, "job-treo", Duration::from_millis(300), |_p, _j, _t| {
            da_hoi.store(true, std::sync::atomic::Ordering::SeqCst);
            KetQuaIn::KhongRo(LyDo::co_loai("het gio", MaSuCo::KhongXacNhan))
        });
        let da_mat = t0.elapsed();
        std::env::remove_var("SUMATRA_PATH");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&script);
        assert!(da_mat < Duration::from_secs(5), "không được chờ Sumatra vô hạn: {:?}", da_mat);
        assert!(da_hoi.load(std::sync::atomic::Ordering::SeqCst), "quá hạn vẫn phải theo dõi spooler");
        assert!(matches!(kq, KetQuaIn::KhongRo(ref l) if l.loai == Some(MaSuCo::LoiSumatra)), "{:?}", kq);
    }

    /// "Sumatra" giả treo 30 s (sh `exec sleep` để kill trúng tiến trình).
    fn tao_lenh_treo() -> PathBuf {
        if cfg!(windows) {
            let p = std::env::temp_dir().join(format!("pa-test-treo-{}.cmd", now_id()));
            std::fs::write(&p, "@echo off\r\nping -n 30 127.0.0.1 >nul\r\n").unwrap();
            p
        } else {
            let p = std::env::temp_dir().join(format!("pa-test-treo-{}.sh", now_id()));
            std::fs::write(&p, "#!/bin/sh\nexec sleep 30\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&p).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&p, perms).unwrap();
            }
            p
        }
    }

    // --- R11d: PDF "In thử" hợp lệ ---

    fn tim(hay: &[u8], kim: &[u8]) -> Option<usize> {
        hay.windows(kim.len()).position(|w| w == kim)
    }

    /// Lỗi 13.5: PDF_GIA không phải PDF hợp lệ. Nay: mỗi mục xref trỏ ĐÚNG
    /// "<n> 0 obj", startxref trỏ đúng "xref", /Length đúng số byte stream.
    #[test]
    fn pdf_in_thu_hop_le_xref_dung_offset() {
        let pdf = pdf_in_thu();
        assert!(pdf.starts_with(b"%PDF-1.4\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        let chu = String::from_utf8(pdf.clone()).unwrap();
        let sx = chu.rfind("startxref\n").unwrap() + "startxref\n".len();
        let xref: usize = chu[sx..].lines().next().unwrap().parse().unwrap();
        assert!(chu[xref..].starts_with("xref\n0 6\n"), "startxref phải trỏ đúng bảng xref");
        let dong: Vec<&str> = chu[xref..].lines().skip(2).take(6).collect();
        assert_eq!(dong[0], "0000000000 65535 f ");
        for (i, d) in dong.iter().enumerate().skip(1) {
            assert_eq!(d.len() + 1, 20, "mỗi dòng xref đúng 20 byte (kể cả \\n): {:?}", d);
            let off: usize = d[..10].parse().unwrap();
            assert!(chu[off..].starts_with(&format!("{} 0 obj\n", i)), "xref #{} lệch: {}", i, off);
        }
        // /Length khớp số byte giữa "stream\n" và "\nendstream"
        let bd = tim(&pdf, b"stream\n").unwrap() + b"stream\n".len();
        let kt = tim(&pdf, b"\nendstream").unwrap();
        let len_khai: usize = {
            let i = chu.find("/Length ").unwrap() + "/Length ".len();
            chu[i..].split_whitespace().next().unwrap().parse().unwrap()
        };
        assert_eq!(kt - bd, len_khai);
        assert!(chu.contains("/MediaBox [0 0 420 595]"), "khổ A5");
    }
}
