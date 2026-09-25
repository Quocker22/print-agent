// SPDX-License-Identifier: AGPL-3.0-or-later
//! File nhật ký cục bộ `%LOCALAPPDATA%\print-agent\logs\print-agent-YYYY-MM-DD.txt`
//! — mỗi dòng một sự kiện, giữ 14 ngày (hợp đồng v2 §4.5). Đuôi `.txt` (0.2.3,
//! chủ yêu cầu): bấm đúp là mở bằng Notepad; nút "Nhật ký" trên app mở file hôm
//! nay. File `.log` của bản cũ vẫn được dọn theo 14 ngày.
//!
//! VÌ SAO CẦN: trước bản này app chỉ in ra stderr, mà mở bằng double-click thì
//! không ai thấy stderr (handoff §12.1) — shop kêu "không in được" là không có
//! gì để tra.
//!
//! BA RÀNG BUỘC, và cách giữ:
//! 1. **Ghi nhật ký không bao giờ chặn/làm hỏng việc in.** `ghi()` chỉ đẩy dòng
//!    vào kênh rồi trả về ngay; một luồng riêng ghi đĩa. Đĩa đầy, thư mục không
//!    tạo được, file bị khoá… chỉ ra một dòng stderr — luồng in không bao giờ
//!    chờ I/O đĩa, không bao giờ thấy lỗi.
//! 2. **Không lộ token** (§0.3): nơi gọi chỉ ghi id job đã cắt token
//!    (`job::rut_gon_job_id`). Nhưng `loiCuoi` có thể chứa đường dẫn file tạm
//!    — tên file chứa job id, tức chứa token — nên luồng ghi còn thay mọi token
//!    đã đăng ký (`che_bi_mat`) bằng `***` trước khi ghi: phòng hai lớp.
//! 3. **Không ghi PDF** (`pdfBase64`): không nơi gọi nào truyền nó vào đây.
//!
//! Giờ trong file là UTC (xem thoi_gian.rs) — cùng quy ước DB backend.

use crate::thoi_gian;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

const TIEN_TO: &str = "print-agent-";
const DUOI: &str = ".txt";
/// Đuôi của bản ≤ 0.2.2 — chỉ để dọn file cũ.
const DUOI_CU: &str = ".log";
/// Giữ file của 14 ngày gần nhất (kể cả hôm nay).
const SO_NGAY_GIU: u64 = 14;
/// Token ngắn hơn ngưỡng này không đem đi thay — thay chuỗi 2–3 ký tự là nát
/// cả dòng nhật ký mà không che được gì đáng kể.
const DO_DAI_BI_MAT_TOI_THIEU: usize = 6;

type Dong = (SystemTime, String, String);

/// Kênh tới luồng ghi. `None` khi không có thư mục nhật ký (máy không phải
/// Windows) hoặc không spawn được luồng — lúc đó `ghi()` là no-op.
static KENH: OnceLock<Option<Sender<Dong>>> = OnceLock::new();
/// Chuỗi phải che (token máy in). Mutex chỉ giữ trong lúc chép Vec nhỏ.
static BI_MAT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Thư mục nhật ký. Chỉ Windows, và KHÔNG trong `cargo test` (chạy test trên
/// máy build .207 không được chèn dòng giả vào nhật ký thật). Mac/CI/test trả
/// `None` — test gọi thẳng `ghi_vao`/`don_file_cu` với thư mục tạm.
fn thu_muc() -> Option<PathBuf> {
    #[cfg(all(windows, not(test)))]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("print-agent").join("logs"))
    }
    #[cfg(not(all(windows, not(test))))]
    {
        None
    }
}

/// File nhật ký của HÔM NAY (giờ UTC như tên file) — cho nút "Nhật ký" trên app.
/// `None` khi không có thư mục nhật ký (không phải Windows).
pub fn file_hom_nay() -> Option<PathBuf> {
    Some(thu_muc()?.join(ten_file(&thoi_gian::ngay_utc(SystemTime::now()))))
}

/// Thư mục nhật ký (mở bằng Explorer khi file hôm nay chưa có).
pub fn thu_muc_nhat_ky() -> Option<PathBuf> {
    thu_muc()
}

/// Đăng ký một chuỗi bí mật (token máy in) để luồng ghi thay bằng `***`.
pub fn che_bi_mat(bi_mat: &str) {
    if bi_mat.len() < DO_DAI_BI_MAT_TOI_THIEU {
        return;
    }
    let mut ds = BI_MAT.lock().unwrap_or_else(|p| p.into_inner());
    if !ds.iter().any(|s| s == bi_mat) {
        ds.push(bi_mat.to_string());
    }
}

// ── Bộ đệm gửi lên ZaloCRM (0.2.4, chủ yêu cầu 25/09) ─────────────────────
// MỌI dòng ghi file cũng vào bộ đệm này; luồng `gui-nhat-ky` (net.rs) lấy
// từng lô gửi `nhat-ky-app` kèm ack, không ack thì trả lại. Trần cố định:
// mất mạng cả ngày vẫn không phình bộ nhớ — tràn thì bỏ dòng CŨ NHẤT và đếm
// (`boQua`), backend ghi một dòng "app bỏ N dòng".

/// Trần bộ đệm gửi (dòng).
pub const TRAN_CHO_GUI: usize = 20_000;

/// Một dòng chờ gửi lên backend (đã che token khi LẤY ra).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DongGui {
    pub luc: SystemTime,
    pub su_kien: String,
    pub noi_dung: String,
}

#[derive(Default)]
struct ChoGui {
    ds: std::collections::VecDeque<DongGui>,
    bo_qua: u64,
}

impl ChoGui {
    fn them(&mut self, dong: DongGui, tran: usize) {
        if self.ds.len() >= tran {
            self.ds.pop_front();
            self.bo_qua += 1;
        }
        self.ds.push_back(dong);
    }

    fn lay(&mut self, toi_da: usize) -> (Vec<DongGui>, u64) {
        let n = toi_da.min(self.ds.len());
        (self.ds.drain(..n).collect(), std::mem::take(&mut self.bo_qua))
    }

    fn tra_lai(&mut self, lo: Vec<DongGui>, bo_qua: u64, tran: usize) {
        self.bo_qua += bo_qua;
        for d in lo.into_iter().rev() {
            self.ds.push_front(d);
        }
        while self.ds.len() > tran {
            self.ds.pop_front();
            self.bo_qua += 1;
        }
    }
}

static CHO_GUI: Mutex<Option<ChoGui>> = Mutex::new(None);

fn voi_cho_gui<R>(f: impl FnOnce(&mut ChoGui) -> R) -> R {
    let mut k = CHO_GUI.lock().unwrap_or_else(|p| p.into_inner());
    f(k.get_or_insert_with(ChoGui::default))
}

/// Che token + phẳng tab/xuống dòng một lô trước khi gửi.
fn lam_sach_lo(lo: Vec<DongGui>, bi_mat: &[String]) -> Vec<DongGui> {
    let phang = |s: &str| s.replace(['\r', '\n', '\t'], " ");
    lo.into_iter()
        .map(|d| DongGui { luc: d.luc, su_kien: phang(&che(&d.su_kien, bi_mat)), noi_dung: phang(&che(d.noi_dung.trim(), bi_mat)) })
        .collect()
}

/// Lấy tối đa `toi_da` dòng CŨ NHẤT (đã che token, phẳng tab/xuống dòng) + số
/// dòng đã bỏ vì tràn (đặt lại 0 — lô này mang nó đi).
pub fn lay_lo_gui(toi_da: usize) -> (Vec<DongGui>, u64) {
    let (lo, bo_qua) = voi_cho_gui(|c| c.lay(toi_da));
    let bi_mat = BI_MAT.lock().map(|ds| ds.clone()).unwrap_or_default();
    (lam_sach_lo(lo, &bi_mat), bo_qua)
}

/// Gửi hỏng: trả lô về ĐẦU bộ đệm (giữ thứ tự). Tràn thì bỏ dòng cũ nhất.
pub fn tra_lai_gui(lo: Vec<DongGui>, bo_qua: u64) {
    voi_cho_gui(|c| c.tra_lai(lo, bo_qua, TRAN_CHO_GUI));
}

/// Ghi một sự kiện. KHÔNG chặn, KHÔNG panic, KHÔNG trả lỗi (xem đầu file).
pub fn ghi(su_kien: &str, noi_dung: &str) {
    let dong = DongGui { luc: SystemTime::now(), su_kien: su_kien.to_string(), noi_dung: noi_dung.to_string() };
    voi_cho_gui(|c| c.them(dong, TRAN_CHO_GUI));
    let kenh = KENH.get_or_init(|| {
        let dir = thu_muc()?;
        let (gui, nhan) = mpsc::channel::<Dong>();
        std::thread::Builder::new()
            .name("nhat-ky".into())
            .spawn(move || chay_luong_ghi(dir, nhan))
            .map_err(|e| eprintln!("[print-agent] không mở được luồng nhật ký: {}", e))
            .ok()?;
        Some(gui)
    });
    if let Some(gui) = kenh {
        let _ = gui.send((SystemTime::now(), su_kien.to_string(), noi_dung.to_string()));
    }
}

fn chay_luong_ghi(dir: PathBuf, nhan: mpsc::Receiver<Dong>) {
    // Ngày (UTC) đã dọn file cũ — dọn lúc ghi dòng đầu tiên và mỗi khi sang ngày.
    let mut ngay_da_don: Option<String> = None;
    while let Ok((luc, su_kien, noi_dung)) = nhan.recv() {
        let hom_nay = thoi_gian::ngay_utc(luc);
        if ngay_da_don.as_deref() != Some(hom_nay.as_str()) {
            don_file_cu(&dir, luc);
            ngay_da_don = Some(hom_nay);
        }
        let bi_mat = BI_MAT.lock().map(|ds| ds.clone()).unwrap_or_default();
        let noi_dung = che(&noi_dung, &bi_mat);
        if let Err(e) = ghi_vao(&dir, luc, &su_kien, &noi_dung) {
            eprintln!("[print-agent] ghi nhật ký lỗi ({}): {}", dir.display(), e);
        }
    }
}

fn che(chu: &str, bi_mat: &[String]) -> String {
    bi_mat.iter().fold(chu.to_string(), |acc, s| acc.replace(s.as_str(), "***"))
}

/// Một dòng: `<ISO UTC>\t<su_kien>\t<noi_dung>`. Tab/xuống dòng trong nội dung
/// (stderr của Sumatra có nhiều dòng) thành khoảng trắng — một sự kiện một dòng
/// thì `findstr`/`Select-String` trên máy shop mới tìm được.
fn dong_nhat_ky(luc: SystemTime, su_kien: &str, noi_dung: &str) -> String {
    let phang = |s: &str| s.replace(['\r', '\n', '\t'], " ");
    format!("{}\t{}\t{}\n", thoi_gian::iso_utc(luc), phang(su_kien), phang(noi_dung.trim()))
}

fn ten_file(ngay: &str) -> String {
    format!("{}{}{}", TIEN_TO, ngay, DUOI)
}

/// Mở-ghi-đóng mỗi dòng: không giữ handle qua đêm (sang ngày tự sang file mới,
/// người dùng xoá/mở file lúc app chạy cũng không sao).
fn ghi_vao(dir: &Path, luc: SystemTime, su_kien: &str, noi_dung: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(ten_file(&thoi_gian::ngay_utc(luc))))?;
    f.write_all(dong_nhat_ky(luc, su_kien, noi_dung).as_bytes())
}

/// Ngày trong tên file nhật ký của app, `None` nếu không đúng mẫu — chỉ đụng
/// file do chính app đặt tên, không bao giờ xoá file lạ trong thư mục.
fn ngay_cua_file(ten: &str) -> Option<&str> {
    let than = ten.strip_prefix(TIEN_TO)?;
    let ngay = than.strip_suffix(DUOI).or_else(|| than.strip_suffix(DUOI_CU))?;
    let b = ngay.as_bytes();
    let dung_mau = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    dung_mau.then_some(ngay)
}

/// Xoá file nhật ký có ngày ≤ (hôm nay − 14): giữ đúng 14 ngày gần nhất.
/// So chuỗi "YYYY-MM-DD" theo thứ tự từ điển = so ngày.
fn don_file_cu(dir: &Path, bay_gio: SystemTime) {
    let moc = thoi_gian::ngay_utc_truoc(bay_gio, SO_NGAY_GIU);
    let Ok(ds) = std::fs::read_dir(dir) else { return };
    for f in ds.flatten() {
        let ten = f.file_name().to_string_lossy().into_owned();
        if ngay_cua_file(&ten).is_some_and(|ngay| ngay <= moc.as_str()) {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    /// 2026-09-24T12:00:00Z
    fn hom_nay() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_790_251_200)
    }

    fn thu_muc_tam(ten: &str) -> PathBuf {
        let ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("pa-nhat-ky-{}-{:x}", ten, ns))
    }

    #[test]
    fn moc_test_dung_ngay() {
        assert_eq!(thoi_gian::iso_utc(hom_nay()), "2026-09-24T12:00:00.000Z");
    }

    #[test]
    fn mot_su_kien_mot_dong_khong_tab_xuong_dong_trong_noi_dung() {
        let d = dong_nhat_ky(hom_nay(), "ket_qua", "loi\r\ndong 2\tcot");
        assert_eq!(d, "2026-09-24T12:00:00.000Z\tket_qua\tloi  dong 2 cot\n");
    }

    #[test]
    fn che_token_trong_noi_dung() {
        let bi_mat = vec!["tokHN123456".to_string()];
        let s = che(r"SumatraPDF lỗi: C:\Temp\AI-INV_1-Khach-tokHN123456-1727-3.pdf", &bi_mat);
        assert!(!s.contains("tokHN123456"));
        assert!(s.contains("AI-INV_1-Khach-***-1727-3.pdf"));
    }

    #[test]
    fn ghi_vao_file_theo_ngay_va_noi_tiep() {
        let dir = thu_muc_tam("ghi");
        ghi_vao(&dir, hom_nay(), "ket_noi", "server=x").unwrap();
        ghi_vao(&dir, hom_nay() + Duration::from_secs(1), "ket_qua", "da_in").unwrap();
        let noi_dung = std::fs::read_to_string(dir.join("print-agent-2026-09-24.txt")).unwrap();
        let dong: Vec<&str> = noi_dung.lines().collect();
        assert_eq!(dong.len(), 2);
        assert!(dong[0].ends_with("\tket_noi\tserver=x"));
        assert!(dong[1].ends_with("\tket_qua\tda_in"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nhan_dung_mau_ten_file() {
        assert_eq!(ngay_cua_file("print-agent-2026-09-10.txt"), Some("2026-09-10"));
        assert_eq!(ngay_cua_file("print-agent-2026-09-10.log"), Some("2026-09-10"), "file của bản cũ vẫn dọn được");
        for ten in ["print-agent-2026-9-10.log", "print-agent-2026-09-10.csv", "khac-2026-09-10.log",
                    "print-agent-abcd-ef-gh.log", "print-agent-2026-09-100.log"] {
            assert_eq!(ngay_cua_file(ten), None, "{}", ten);
        }
    }

    #[test]
    fn don_file_cu_giu_14_ngay_va_khong_dung_file_la() {
        let dir = thu_muc_tam("don");
        std::fs::create_dir_all(&dir).unwrap();
        for ten in ["print-agent-2026-09-09.log", "print-agent-2026-09-10.log", "print-agent-2026-09-11.log",
                    "print-agent-2026-09-24.log", "ghi-chu.txt", "print-agent-cu.log"] {
            std::fs::write(dir.join(ten), "x").unwrap();
        }
        don_file_cu(&dir, hom_nay());
        let mut con: Vec<String> = std::fs::read_dir(&dir).unwrap()
            .flatten().map(|f| f.file_name().to_string_lossy().into_owned()).collect();
        con.sort();
        // 24/09 − 14 = 10/09 ⇒ xoá 09/09 và 10/09; giữ 11/09..24/09 (14 ngày) + file lạ.
        assert_eq!(con, vec!["ghi-chu.txt", "print-agent-2026-09-11.log", "print-agent-2026-09-24.log", "print-agent-cu.log"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ghi_loi_khong_panic() {
        // Thư mục là một FILE → create_dir_all lỗi → trả Err, không panic.
        let f = thu_muc_tam("la-file");
        std::fs::write(&f, "x").unwrap();
        assert!(ghi_vao(&f, hom_nay(), "x", "y").is_err());
        let _ = std::fs::remove_file(&f);
        // Trên Mac không có thư mục nhật ký: ghi() là no-op, không panic.
        ghi("thu", "noi dung");
    }

    fn d(i: u64) -> DongGui {
        DongGui { luc: UNIX_EPOCH + Duration::from_secs(i), su_kien: "e".into(), noi_dung: format!("n{}", i) }
    }

    /// Bộ đệm gửi (0.2.4): lấy cũ nhất trước; gửi hỏng trả lại ĐẦU, giữ thứ tự;
    /// tràn bỏ dòng CŨ NHẤT và đếm.
    #[test]
    fn cho_gui_thu_tu_tra_lai_va_tran() {
        let mut c = ChoGui::default();
        for i in 0..5 {
            c.them(d(i), 4);
        }
        assert_eq!(c.bo_qua, 1, "tràn một dòng");
        let (lo, bo) = c.lay(2);
        assert_eq!((lo.iter().map(|x| x.noi_dung.as_str()).collect::<Vec<_>>(), bo), (vec!["n1", "n2"], 1));
        assert_eq!(c.bo_qua, 0, "bo_qua đi theo lô");
        c.them(d(9), 4);
        c.tra_lai(lo, bo, 4);
        let (tat_ca, bo) = c.lay(10);
        assert_eq!(tat_ca.iter().map(|x| x.noi_dung.as_str()).collect::<Vec<_>>(), vec!["n2", "n3", "n4", "n9"], "tràn khi trả lại bỏ dòng cũ nhất");
        assert_eq!(bo, 2);
    }

    #[test]
    fn lam_sach_lo_che_token_va_phang() {
        let lo = vec![DongGui { luc: UNIX_EPOCH, su_kien: "ket\tqua".into(), noi_dung: " a TOKEN123456 b\nc ".into() }];
        let ra = lam_sach_lo(lo, &["TOKEN123456".to_string()]);
        assert_eq!(ra[0].su_kien, "ket qua");
        assert_eq!(ra[0].noi_dung, "a *** b c");
    }
}
