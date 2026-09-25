// SPDX-License-Identifier: AGPL-3.0-or-later
//! Một máy chỉ chạy MỘT bản app (R7c, lỗi 13.4).
//!
//! VÌ SAO: tự khởi động cùng Windows (tu_khoi_dong.rs) + NV bấm đúp mở thêm
//! là hai bản cùng token: hai kết nối, hai worker in, hai luồng đọc spooler —
//! backend giao job cho một bản, bản kia có thể xoá nhầm/báo nhầm hàng đợi
//! chung. Bản thứ hai hiện thông báo ngắn rồi thoát.
//!
//! Cách làm: named mutex `Global\print-agent-lednelia` — TOÀN MÁY, mọi phiên
//! đăng nhập (R-L, giám sát vòng 2). Bản trước dùng `Local\` (theo phiên): phiên
//! Windows thứ hai / RDP chạy bản thứ hai cùng config.ini, cùng máy in → bản
//! này RESUME đúng job bản kia vừa tạm dừng để xoá = in đôi.
//! CreateMutexW thành công mà GetLastError = ERROR_ALREADY_EXISTS ⇒ đã có bản
//! khác; ERROR_ACCESS_DENIED ⇒ mutex đã có, do phiên/người dùng KHÁC tạo (DACL
//! mặc định không cho ta mở) — cũng là đã có bản khác. Handle giữ tới khi
//! tiến trình thoát (kể cả `process::exit` — Windows tự đóng).
//!
//! T10 (giám sát vòng 3): bản đang chạy ở PHIÊN WINDOWS KHÁC (người dùng khác,
//! Remote Desktop) thì NV ở phiên này KHÔNG thấy biểu tượng khay của nó — câu
//! "xem biểu tượng ở khay" làm NV tìm mãi không thấy. Mỗi bản giữ thêm một
//! mutex THEO PHIÊN (`Local\`): Global có mà Local chưa có ⇒ bản kia ở phiên khác.

/// Tên mutex (UTF-8; chuyển UTF-16 lúc gọi Win32).
#[cfg_attr(not(windows), allow(dead_code))]
pub const TEN_KHOA: &str = "Global\\print-agent-lednelia";
/// Mutex theo PHIÊN — chỉ để biết bản kia có cùng phiên đăng nhập không.
#[cfg_attr(not(windows), allow(dead_code))]
pub const TEN_KHOA_PHIEN: &str = "Local\\print-agent-lednelia";

/// Kết quả thử tạo khoá.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KetLuanKhoa {
    DuocChay,
    DaCoBanKhac(BanKhac),
    /// Lỗi lạ — vẫn cho chạy (không để lỗi phụ làm shop mất app in).
    LoiLa,
}

/// Bản app đang chạy sẵn nằm ở đâu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BanKhac {
    /// Cùng phiên đăng nhập — biểu tượng ở khay của phiên này.
    CungPhien,
    /// Phiên đăng nhập Windows KHÁC (người dùng khác / Remote Desktop).
    PhienKhac,
}

/// Mã Win32 ERROR_ACCESS_DENIED (khoá bằng `const _` bên dưới trên Windows).
pub const MA_ACCESS_DENIED: u32 = 5;

/// Quyết từ kết quả CreateMutexW — THUẦN: `tao` = khoá TOÀN MÁY (`Ok(da_ton_tai)`
/// hoặc `Err(mã Win32)`); `phien_da_co` = khoá THEO PHIÊN đã có sẵn (bản kia
/// cùng phiên). ACCESS_DENIED = mutex do người dùng khác tạo ⇒ phiên khác.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn ket_luan_tao_khoa(tao: Result<bool, u32>, phien_da_co: bool) -> KetLuanKhoa {
    match tao {
        Ok(true) if phien_da_co => KetLuanKhoa::DaCoBanKhac(BanKhac::CungPhien),
        Ok(true) | Err(MA_ACCESS_DENIED) => KetLuanKhoa::DaCoBanKhac(BanKhac::PhienKhac),
        Ok(false) => KetLuanKhoa::DuocChay,
        Err(_) => KetLuanKhoa::LoiLa,
    }
}

#[cfg(windows)]
const _: () = assert!(MA_ACCESS_DENIED == windows::Win32::Foundation::ERROR_ACCESS_DENIED.0);

/// Giữ khoá "một bản" — thả khi tiến trình kết thúc.
pub struct KhoaMotBan {
    #[cfg(windows)]
    _handle: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(windows)]
    _handle_phien: Option<windows::Win32::Foundation::HANDLE>,
}

/// Tạo (hoặc mở) một named mutex: `(kết quả CreateMutexW, đã tồn tại từ trước)`.
#[cfg(windows)]
fn tao_mutex(ten: &str) -> (windows::core::Result<windows::Win32::Foundation::HANDLE>, bool) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    let ten: Vec<u16> = ten.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `ten` kết thúc \0 và sống tới hết lời gọi; không truyền security attributes.
    let tao = unsafe { CreateMutexW(None, false, PCWSTR(ten.as_ptr())) };
    // GetLastError ngay sau CreateMutexW thành công: ALREADY_EXISTS = mutex đã
    // có từ trước (bản khác đang giữ).
    let da_ton_tai = tao.is_ok() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    (tao, da_ton_tai)
}

/// `Err(chỗ bản kia)` = đã có bản khác đang chạy. Không tạo được mutex (lỗi
/// lạ) thì vẫn cho chạy — không để một lỗi phụ làm shop mất app in.
#[cfg(windows)]
pub fn giu_mot_ban() -> Result<KhoaMotBan, BanKhac> {
    use windows::Win32::Foundation::{CloseHandle, WIN32_ERROR};

    let (tao, da_ton_tai) = tao_mutex(TEN_KHOA);
    // Khoá theo phiên: bản đầu giữ nó suốt đời; bản sau chỉ dùng để biết bản
    // kia có cùng phiên không (Local\ riêng từng phiên đăng nhập).
    let (tao_phien, phien_da_co) = tao_mutex(TEN_KHOA_PHIEN);
    let ket_luan = ket_luan_tao_khoa(
        match &tao {
            Ok(_) => Ok(da_ton_tai),
            Err(e) => Err(WIN32_ERROR::from_error(e).map_or(u32::MAX, |w| w.0)),
        },
        phien_da_co,
    );
    let dong = |h: windows::core::Result<windows::Win32::Foundation::HANDLE>| {
        if let Ok(h) = h {
            // SAFETY: handle vừa tạo, chưa ai dùng.
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    };
    match (ket_luan, tao) {
        (KetLuanKhoa::DuocChay, Ok(h)) => Ok(KhoaMotBan { _handle: Some(h), _handle_phien: tao_phien.ok() }),
        (KetLuanKhoa::DaCoBanKhac(o_dau), tao) => {
            dong(tao);
            dong(tao_phien);
            Err(o_dau)
        }
        (_, tao) => {
            eprintln!("[print-agent] không tạo được khoá một bản ({:?}) — vẫn chạy", tao.err());
            Ok(KhoaMotBan { _handle: None, _handle_phien: tao_phien.ok() })
        }
    }
}

/// Mac/dev/test: không có khái niệm này — luôn cho chạy.
#[cfg(not(windows))]
pub fn giu_mot_ban() -> Result<KhoaMotBan, BanKhac> {
    Ok(KhoaMotBan {})
}

/// Câu báo cho bản thứ hai — CÙNG phiên đăng nhập.
pub const CHU_DA_CHAY: &str =
    "Incokit Print Agent đang chạy rồi (xem biểu tượng máy in ở khay hệ thống, góc phải thanh taskbar).";

/// Câu báo cho bản thứ hai khi bản đang chạy ở PHIÊN KHÁC (T10) — biểu tượng
/// khay của nó KHÔNG hiện ở phiên này.
pub const CHU_DA_CHAY_PHIEN_KHAC: &str = "Incokit Print Agent đang chạy ở phiên đăng nhập Windows khác (người dùng khác hoặc Remote Desktop) — mỗi máy chỉ chạy một bản. Muốn chạy ở phiên này thì thoát app ở phiên kia trước.";

/// Câu hộp thoại theo chỗ bản đang chạy.
pub fn chu_da_chay(o_dau: BanKhac) -> &'static str {
    match o_dau {
        BanKhac::CungPhien => CHU_DA_CHAY,
        BanKhac::PhienKhac => CHU_DA_CHAY_PHIEN_KHAC,
    }
}

/// Hiện thông báo ngắn cho bản thứ hai (Windows: hộp thoại; nơi khác: stderr).
#[cfg(windows)]
pub fn bao_da_chay(o_dau: BanKhac) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let noi_dung: Vec<u16> = chu_da_chay(o_dau).encode_utf16().chain(std::iter::once(0)).collect();
    let tieu_de: Vec<u16> = "Incokit Print Agent".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: hai chuỗi kết thúc \0, sống tới hết lời gọi; không có cửa sổ cha.
    unsafe {
        MessageBoxW(None, PCWSTR(noi_dung.as_ptr()), PCWSTR(tieu_de.as_ptr()), MB_OK | MB_ICONINFORMATION);
    }
}

#[cfg(not(windows))]
pub fn bao_da_chay(o_dau: BanKhac) {
    eprintln!("[print-agent] {}", chu_da_chay(o_dau));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_khoa_dung() {
        assert_eq!(TEN_KHOA, r"Global\print-agent-lednelia", "R-L: toàn máy, không theo phiên");
        assert_eq!(TEN_KHOA_PHIEN, r"Local\print-agent-lednelia");
    }

    #[cfg(not(windows))]
    #[test]
    fn ngoai_windows_luon_cho_chay() {
        assert!(giu_mot_ban().is_ok());
        assert!(giu_mot_ban().is_ok(), "mac/test: no-op, gọi lại vẫn được");
    }

    /// Windows THẬT (đo trên máy build .207, 25/09): lần giữ thứ hai trong CÙNG
    /// phiên bị chặn và nhận ra là cùng phiên. Bản trước của test này không
    /// gắn `cfg(not(windows))` nên đỏ trên Windows — chính vì khoá chạy đúng.
    #[cfg(windows)]
    #[test]
    fn windows_ban_thu_hai_cung_phien_bi_chan() {
        let dau = giu_mot_ban();
        if dau.is_err() {
            // Máy đang chạy app print-agent thật (giữ khoá) — không kiểm được ở đây.
            eprintln!("có print-agent đang chạy trên máy này — bỏ qua");
            return;
        }
        assert_eq!(giu_mot_ban().err(), Some(BanKhac::CungPhien));
    }

    /// R-L: ACCESS_DENIED = mutex đã có, do phiên/người dùng khác tạo → bản kia
    /// đang chạy. T10: Global có mà Local chưa có → bản kia ở PHIÊN KHÁC.
    #[test]
    fn r_l_access_denied_la_da_co_ban_khac() {
        use BanKhac::*;
        assert_eq!(ket_luan_tao_khoa(Ok(false), false), KetLuanKhoa::DuocChay);
        assert_eq!(ket_luan_tao_khoa(Ok(true), true), KetLuanKhoa::DaCoBanKhac(CungPhien));
        assert_eq!(ket_luan_tao_khoa(Ok(true), false), KetLuanKhoa::DaCoBanKhac(PhienKhac));
        assert_eq!(ket_luan_tao_khoa(Err(MA_ACCESS_DENIED), false), KetLuanKhoa::DaCoBanKhac(PhienKhac));
        assert_eq!(ket_luan_tao_khoa(Err(MA_ACCESS_DENIED), true), KetLuanKhoa::DaCoBanKhac(PhienKhac), "người dùng khác");
        assert_eq!(ket_luan_tao_khoa(Err(87), false), KetLuanKhoa::LoiLa, "lỗi lạ vẫn cho chạy");
    }

    #[test]
    fn t10_cau_hop_thoai_noi_ro_phien_khac() {
        assert!(chu_da_chay(BanKhac::CungPhien).contains("khay hệ thống"));
        let chu = chu_da_chay(BanKhac::PhienKhac);
        assert!(chu.contains("đang chạy ở phiên đăng nhập Windows khác"), "{}", chu);
        assert!(!chu.contains("khay"), "phiên khác: biểu tượng khay không hiện ở đây — đừng bảo NV tìm nó");
    }
}
