// SPDX-License-Identifier: AGPL-3.0-or-later
//! Giữ máy tính KHÔNG TỰ NGỦ khi app đang chạy (0.2.7) — "power request" của
//! Windows (`PowerCreateRequest` + `PowerSetRequest(SystemRequired)`), đúng cơ
//! chế trình phát video / tải file dùng.
//!
//! VÌ SAO (chủ báo 25/09: "máy đôi khi bị sleep thì mất kết nối luôn"): máy
//! ngủ thì app không nhận được lệnh in nào; backend thử lại vài phút rồi báo
//! THẤT BẠI (NV phải in tay) — nhật ký PROD 25/09: máy HN rớt lúc 07:21 (ping
//! timeout) rồi không nối lại. Máy ở cửa hàng là máy làm việc, cắm điện cả ngày.
//!
//! CHỈ chặn tự ngủ khi RẢNH (idle). KHÔNG chặn: người dùng bấm Sleep / gập máy
//! / nút nguồn — lúc đó `thuc_day` lo nối lại ngay khi máy thức. KHÔNG giữ màn
//! hình sáng (màn hình vẫn tắt theo cài đặt).
//!
//! Máy Modern Standby (laptop đời mới, `AoAc`): tắt màn hình có thể vẫn vào
//! standby dù có power request — app báo trên giao diện để cửa hàng đặt "tắt
//! màn hình: Không bao giờ" khi cắm điện.
//!
//! Cài đặt lưu ở `HKCU\Software\IncokitPrintAgent\GiuMayThuc` (DWORD); THIẾU =
//! BẬT (mặc định bật: máy mới cài không cần ai nhớ bấm).
//!
//! Power request gắn với TIẾN TRÌNH — app thoát/crash là Windows tự gỡ, máy
//! ngủ lại bình thường; không để lại gì. Kiểm tra: `powercfg /requests`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

/// Tình trạng giữ máy thức — cho giao diện + nhật ký.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TinhTrang {
    /// Đang giữ được (power request đã đặt).
    pub dang_giu: bool,
    /// Máy dùng Modern Standby — tắt màn hình vẫn có thể ngủ.
    pub modern_standby: bool,
}

static TINH_TRANG: Mutex<TinhTrang> = Mutex::new(TinhTrang { dang_giu: false, modern_standby: false });
static KENH: OnceLock<Option<Sender<bool>>> = OnceLock::new();
/// Lần áp dụng cuối có lỗi (không đặt được power request) — giao diện hiện.
static CO_LOI: AtomicBool = AtomicBool::new(false);

pub fn tinh_trang() -> TinhTrang {
    *TINH_TRANG.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn co_loi() -> bool {
    CO_LOI.load(Ordering::SeqCst)
}

/// Câu dưới ô tick trên giao diện (rỗng = không có gì phải nói).
pub fn chu_giai_thich(bat: bool, tt: TinhTrang, loi: bool) -> String {
    if !bat {
        return "Máy tính tự ngủ thì app KHÔNG nhận được lệnh in cho tới khi có người đánh thức máy.".into();
    }
    if loi {
        return "Windows không cho giữ máy thức — vào Cài đặt nguồn, đặt \"Ngủ: Không bao giờ\" khi cắm điện.".into();
    }
    if tt.modern_standby {
        return "Máy này có thể ngủ khi TẮT MÀN HÌNH (Modern Standby) — đặt \"Tắt màn hình: Không bao giờ\" khi cắm điện."
            .into();
    }
    String::new()
}

/// Luồng giữ power request suốt đời app. Gọi một lần lúc khởi động; áp dụng
/// cài đặt đang lưu.
pub fn khoi_dong() {
    let bat = dang_bat();
    let kenh = KENH.get_or_init(|| {
        let (gui, nhan) = mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("giu-thuc".into())
            .spawn(move || chay(nhan))
            .map_err(|e| crate::nhat_ky::ghi("giu_thuc_loi", &format!("khong tao duoc luong: {}", e)))
            .ok()?;
        Some(gui)
    });
    if let Some(k) = kenh {
        let _ = k.send(bat);
    }
}

/// Bật/tắt: ghi cài đặt rồi áp dụng NGAY (như "Khởi động cùng Windows").
pub fn dat(bat: bool) -> anyhow::Result<()> {
    luu(bat)?;
    if let Some(Some(k)) = KENH.get() {
        let _ = k.send(bat);
    }
    Ok(())
}

fn chay(nhan: mpsc::Receiver<bool>) {
    let mut giu = he::Giu::default();
    let modern_standby = he::modern_standby();
    while let Ok(bat) = nhan.recv() {
        let kq = if bat { giu.bat() } else { giu.tat() };
        let dang_giu = bat && kq.is_ok();
        CO_LOI.store(kq.is_err(), Ordering::SeqCst);
        *TINH_TRANG.lock().unwrap_or_else(|p| p.into_inner()) = TinhTrang { dang_giu, modern_standby };
        crate::nhat_ky::ghi(
            "giu_may_thuc",
            &format!(
                "{} cach={} modern_standby={}{}",
                if dang_giu { "bat" } else { "tat" },
                giu.cach(),
                if modern_standby { "co" } else { "khong" },
                kq.err().map(|e| format!(" loi={}", e)).unwrap_or_default()
            ),
        );
    }
}

#[cfg(windows)]
const KHOA: &str = r"Software\IncokitPrintAgent";
#[cfg(windows)]
const TEN: &str = "GiuMayThuc";

/// Cài đặt đang lưu. Thiếu / không đọc được = BẬT.
#[cfg(windows)]
pub fn dang_bat() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KHOA)
        .and_then(|k| k.get_value::<u32, _>(TEN))
        .map_or(true, |v| v != 0)
}

#[cfg(windows)]
fn luu(bat: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let (khoa, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KHOA).context("mở khoá registry")?;
    khoa.set_value(TEN, &u32::from(bat)).context("ghi registry")?;
    Ok(())
}

#[cfg(not(windows))]
static GIA_LAP: AtomicBool = AtomicBool::new(true);

#[cfg(not(windows))]
pub fn dang_bat() -> bool {
    GIA_LAP.load(Ordering::SeqCst)
}

#[cfg(not(windows))]
fn luu(bat: bool) -> anyhow::Result<()> {
    GIA_LAP.store(bat, Ordering::SeqCst);
    Ok(())
}

#[cfg(windows)]
mod he {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Power::{
        GetPwrCapabilities, PowerClearRequest, PowerCreateRequest, PowerRequestExecutionRequired,
        PowerRequestSystemRequired, PowerSetRequest, SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
        SYSTEM_POWER_CAPABILITIES,
    };
    use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};

    /// Lý do hiện trong `powercfg /requests` — người sửa máy thấy ngay ai giữ.
    const LY_DO: &str = "Incokit print-agent: giữ máy thức để nhận lệnh in hoá đơn";

    /// Power request (ưu tiên) hoặc `SetThreadExecutionState` trên CHÍNH luồng
    /// này (dự phòng — cờ đó gắn với luồng, luồng `giu-thuc` sống suốt đời app).
    #[derive(Default)]
    pub struct Giu {
        yeu_cau: Option<HANDLE>,
        dung_luong: bool,
        // Chuỗi lý do phải sống cùng handle (Windows giữ con trỏ).
        _ly_do: Vec<u16>,
    }

    impl Giu {
        pub fn bat(&mut self) -> Result<(), String> {
            if self.yeu_cau.is_some() || self.dung_luong {
                return Ok(());
            }
            let mut ly_do: Vec<u16> = LY_DO.encode_utf16().chain(std::iter::once(0)).collect();
            let ctx = REASON_CONTEXT {
                Version: 0, // POWER_REQUEST_CONTEXT_VERSION
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(ly_do.as_mut_ptr()) },
            };
            // SAFETY: ctx + chuỗi hợp lệ trong lời gọi; handle được giữ và đóng ở `tat`.
            match unsafe { PowerCreateRequest(&ctx) } {
                Ok(h) => match unsafe { PowerSetRequest(h, PowerRequestSystemRequired) } {
                    Ok(()) => {
                        // Modern Standby: xin thêm "đang chạy việc" — lỗi (máy cũ) thì bỏ qua.
                        let _ = unsafe { PowerSetRequest(h, PowerRequestExecutionRequired) };
                        self.yeu_cau = Some(h);
                        self._ly_do = ly_do;
                        Ok(())
                    }
                    Err(e) => {
                        let _ = unsafe { CloseHandle(h) };
                        self.du_phong(format!("PowerSetRequest: {}", e))
                    }
                },
                Err(e) => self.du_phong(format!("PowerCreateRequest: {}", e)),
            }
        }

        fn du_phong(&mut self, loi_truoc: String) -> Result<(), String> {
            // SAFETY: chỉ đổi cờ thực thi của luồng hiện tại.
            let cu = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
            if cu.0 == 0 {
                return Err(format!("{}; SetThreadExecutionState that bai", loi_truoc));
            }
            self.dung_luong = true;
            Ok(())
        }

        pub fn tat(&mut self) -> Result<(), String> {
            if let Some(h) = self.yeu_cau.take() {
                // SAFETY: handle do PowerCreateRequest trả, đóng đúng một lần.
                unsafe {
                    let _ = PowerClearRequest(h, PowerRequestExecutionRequired);
                    let _ = PowerClearRequest(h, PowerRequestSystemRequired);
                    let _ = CloseHandle(h);
                }
                self._ly_do.clear();
            }
            if self.dung_luong {
                // SAFETY: như trên.
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                self.dung_luong = false;
            }
            Ok(())
        }

        pub fn cach(&self) -> &'static str {
            if self.yeu_cau.is_some() {
                "power_request"
            } else if self.dung_luong {
                "execution_state"
            } else {
                "-"
            }
        }
    }

    pub fn modern_standby() -> bool {
        let mut c = SYSTEM_POWER_CAPABILITIES::default();
        // SAFETY: con trỏ tới struct cục bộ đúng kiểu.
        unsafe { GetPwrCapabilities(&mut c) }.as_bool() && c.AoAc.as_bool()
    }
}

#[cfg(not(windows))]
mod he {
    /// Mac/CI: không có gì để giữ — luôn "được".
    #[derive(Default)]
    pub struct Giu {
        dang: bool,
    }

    impl Giu {
        pub fn bat(&mut self) -> Result<(), String> {
            self.dang = true;
            Ok(())
        }
        pub fn tat(&mut self) -> Result<(), String> {
            self.dang = false;
            Ok(())
        }
        pub fn cach(&self) -> &'static str {
            if self.dang {
                "gia_lap"
            } else {
                "-"
            }
        }
    }

    pub fn modern_standby() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chu_giai_thich_theo_tinh_trang() {
        let binh_thuong = TinhTrang { dang_giu: true, modern_standby: false };
        assert_eq!(chu_giai_thich(true, binh_thuong, false), "", "giữ được, máy thường: không cần nói gì");
        assert!(chu_giai_thich(false, binh_thuong, false).contains("KHÔNG nhận được lệnh in"));
        assert!(chu_giai_thich(true, binh_thuong, true).contains("Không bao giờ"));
        let ms = TinhTrang { dang_giu: true, modern_standby: true };
        assert!(chu_giai_thich(true, ms, false).contains("TẮT MÀN HÌNH"));
    }

    #[test]
    fn giu_gia_lap_bat_tat() {
        let mut g = he::Giu::default();
        assert!(g.bat().is_ok());
        assert!(g.bat().is_ok(), "bật hai lần không lỗi");
        assert!(g.tat().is_ok());
        assert!(g.tat().is_ok(), "tắt hai lần không lỗi");
    }
}
