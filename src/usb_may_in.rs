// SPDX-License-Identifier: AGPL-3.0-or-later
//! Đọc trạng thái máy in USB THẲNG từ thiết bị (usbprint.sys) — lớp thứ hai
//! bên cạnh cờ spooler, cho máy in mà Windows không biết gì về giấy.
//!
//! VÌ SAO (đo thật ở máy HCM, 25/09): máy HP Laser 103/107/108 cắm USB
//! (cổng `USB001`, driver v3 KHÔNG có language monitor) hết giấy mà Windows
//! vẫn báo "rảnh" (Win32_Printer PrinterStatus 3, DetectedErrorState 0), hàng
//! đợi trống: máy nhận trọn hoá đơn một trang vào bộ nhớ trong vài giây rồi
//! GIỮ ĐÓ chờ giấy. App chỉ đọc spooler nên báo `da_in` — INV/2026/030110 bị
//! gửi 3 lần, nạp giấy vào máy tự ra 3 tờ. Chỉ phần mềm HP biết hết giấy, vì
//! nó hỏi thẳng máy qua USB. Ta hỏi đúng hai câu CHUẨN của lớp USB máy in:
//!   - `IOCTL_USBPRINT_GET_LPT_STATUS` → một byte (GET_PORT_STATUS): bit3 =
//!     "không lỗi" (nFault), bit5 = hết giấy (PaperEmpty);
//!   - `IOCTL_USBPRINT_GET_1284_ID` → chuỗi IEEE 1284; máy HP/Samsung có trường
//!     `STATUS:` (IDLE/BUSY).
//!
//! Số đo ở máy HCM (HP Laser 103 107 108):
//!
//! | tình trạng                   | byte | bit3 "không lỗi" | STATUS |
//! |------------------------------|------|------------------|--------|
//! | có giấy, rảnh                | 0x18 | 1                | IDLE   |
//! | đang in bình thường          | 0x98 | 1                | BUSY   |
//! | hết giấy, đang giữ hoá đơn   | 0x90 | 0                | BUSY   |
//!
//! Bit5 (hết giấy chuẩn) KHÔNG bật ở máy này — hết giấy chỉ hiện ra ở bit3,
//! tức "máy đang lỗi" chung chung (hết giấy / kẹt / mở nắp) → mã `can_xu_ly`.
//! Máy nào bật bit5 thì ra `het_giay`. `STATUS:BUSY` có ở CẢ lúc in lẫn lúc
//! hết giấy — một mình nó không nói được gì; chỉ bit3 phân biệt.
//!
//! CHỈ ĐỌC: mở thiết bị với quyền truy cập 0 (đủ để hỏi trạng thái), không
//! bao giờ đọc/ghi dữ liệu in. Không chen vào lúc spooler đẩy byte xuống cổng:
//! spooler.rs chỉ gọi khi mọi job trong hàng đợi đã gửi xong (`hang_doi_cho_doc_usb`),
//! và worker tạm ngừng mọi lần đọc trong lúc Sumatra nộp job (`TamNgungDocUsb`).
//! Chỉ máy in CỤC BỘ trên đúng một cổng `USBnnn`. Tìm thiết bị (0.2.3): (1) số
//! cổng usbmon ghi trong registry — tài khoản THƯỜNG không đọc được khoá đó
//! (đo ở HCM 25/09: "0 khoa", app chạy không quyền quản trị); (2) danh sách
//! thiết bị máy in USB ĐANG CẮM (SetupAPI — tài khoản thường xem được) mà
//! chuỗi 1284 có model TRÙNG tên driver của máy in Windows, và chỉ khi đúng MỘT
//! thiết bị trùng. Không bao giờ đoán "máy USB duy nhất đang cắm" (cửa hàng có
//! thể cắm thêm máy in tem/bill: đọc nhầm là báo `da_in` cho hoá đơn đang kẹt).

// Phần Win32 chỉ chạy trên Windows; trên Mac phần thuần chỉ chạy trong test.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::su_co::MaSuCo;

/// `CTL_CODE(FILE_DEVICE_UNKNOWN, USBPRINT_IOCTL_INDEX + 12, METHOD_BUFFERED, FILE_ANY_ACCESS)` (usbprint.h).
pub const IOCTL_USBPRINT_GET_LPT_STATUS: u32 = 0x0022_0030;
/// `CTL_CODE(FILE_DEVICE_UNKNOWN, USBPRINT_IOCTL_INDEX + 13, METHOD_BUFFERED, FILE_ANY_ACCESS)` (usbprint.h).
pub const IOCTL_USBPRINT_GET_1284_ID: u32 = 0x0022_0034;

/// Bit3 của byte trạng thái: 1 = KHÔNG lỗi (nFault của cổng song song cũ).
const BIT_KHONG_LOI: u8 = 0x08;
/// Bit5: hết giấy (PaperEmpty).
const BIT_HET_GIAY: u8 = 0x20;

/// Một lần đọc thiết bị USB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocUsb {
    /// Byte của `IOCTL_USBPRINT_GET_LPT_STATUS`.
    pub byte: u8,
    /// Trường `STATUS:` trong chuỗi IEEE 1284, viết hoa, đã cắt khoảng trắng —
    /// `None` khi máy không có trường này (hoặc hỏi chuỗi 1284 lỗi).
    pub status: Option<String>,
}

/// Tình trạng máy suy ra từ một lần đọc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TinhTrangUsb {
    /// Máy báo lỗi (bit3 tắt hoặc bit5 bật): hết giấy / kẹt / mở nắp — hoá
    /// đơn đã gửi xuống đang nằm trong bộ nhớ máy chờ người xử lý.
    Loi(MaSuCo),
    /// Không lỗi, `STATUS` KHÁC `IDLE` (`BUSY`, hoặc giá trị chưa đo — máy đang
    /// "chuẩn bị in" chẳng hạn) — máy đang làm việc, CHƯA xong.
    DangIn,
    /// Không lỗi, `STATUS:IDLE` — rảnh, không còn gì trong máy.
    Ranh,
    /// Không lỗi nhưng không có trường `STATUS` (máy không báo, hoặc hỏi chuỗi
    /// 1284 trục trặc) — không biết đang in hay rảnh: thiếu tin.
    KhongLoi,
}

impl DocUsb {
    pub fn tinh_trang(&self) -> TinhTrangUsb {
        if self.byte & BIT_HET_GIAY != 0 {
            return TinhTrangUsb::Loi(MaSuCo::HetGiay);
        }
        if self.byte & BIT_KHONG_LOI == 0 {
            return TinhTrangUsb::Loi(MaSuCo::CanXuLy);
        }
        // CHỈ `IDLE` (đã đo) mới là rảnh. Giá trị lạ = máy đang làm việc (25/09:
        // 0.2.1 báo `da_in` lúc máy HP còn "Preparing print job" — không bao giờ
        // đoán "xong" từ chữ chưa đo; tệ nhất là xác nhận muộn, không bao giờ sai).
        match self.status.as_deref() {
            Some("IDLE") => TinhTrangUsb::Ranh,
            Some(_) => TinhTrangUsb::DangIn,
            None => TinhTrangUsb::KhongLoi,
        }
    }

    /// Mã sự cố chặn in nếu máy đang báo lỗi.
    pub fn ma_su_co(&self) -> Option<MaSuCo> {
        match self.tinh_trang() {
            TinhTrangUsb::Loi(ma) => Some(ma),
            _ => None,
        }
    }

    /// Dạng ngắn cho vết từng hoá đơn: `0x98/BUSY`, `0x18/-` (không có STATUS).
    pub fn mo_ta_ngan(&self) -> String {
        format!("0x{:02X}/{}", self.byte, self.status.as_deref().unwrap_or("-"))
    }

    /// `chiTiet` cho nhật ký/ZaloCRM, vd
    /// "USB 0x90 STATUS:BUSY — máy in báo lỗi qua USB (thường là hết giấy, kẹt giấy hoặc mở nắp)".
    pub fn mo_ta(&self) -> String {
        let mut s = format!("USB 0x{:02X}", self.byte);
        if let Some(st) = &self.status {
            s.push_str(" STATUS:");
            s.push_str(st);
        }
        match self.tinh_trang() {
            TinhTrangUsb::Loi(MaSuCo::HetGiay) => s.push_str(" — máy in báo hết giấy qua USB"),
            TinhTrangUsb::Loi(_) => s.push_str(" — máy in báo lỗi qua USB (thường là hết giấy, kẹt giấy hoặc mở nắp)"),
            _ => {}
        }
        s
    }
}

/// Giá trị trường `khoa` (một trong các tên) của chuỗi IEEE 1284 (`KHOA:giá
/// trị;` nối nhau), đã cắt khoảng trắng. Khoá so không phân biệt hoa thường;
/// giá trị rỗng coi như không có.
pub fn tach_truong<'a>(chuoi_1284: &'a str, khoa_can: &[&str]) -> Option<&'a str> {
    chuoi_1284.split(';').find_map(|cap| {
        let (khoa, gia_tri) = cap.split_once(':')?;
        let gia_tri = gia_tri.trim();
        (khoa_can.iter().any(|k| khoa.trim().eq_ignore_ascii_case(k)) && !gia_tri.is_empty()).then_some(gia_tri)
    })
}

/// Trường `STATUS:` của chuỗi IEEE 1284, viết hoa.
pub fn tach_status(chuoi_1284: &str) -> Option<String> {
    tach_truong(chuoi_1284, &["STATUS"]).map(str::to_ascii_uppercase)
}

/// Chuỗi 1284 của thiết bị có phải ĐÚNG model của máy in Windows (tên driver)
/// không — `MDL`/`MODEL`, có hoặc không kèm hãng (`MFG`) phía trước. So sau khi
/// gộp khoảng trắng, không phân biệt hoa thường; KHÔNG so "chứa" (máy in tem
/// "HP Laser 107w" ≠ driver "HP Laser 103 107 108").
pub fn khop_model(chuoi_1284: &str, ten_driver: &str) -> bool {
    let chuan = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let driver = chuan(ten_driver);
    let Some(mdl) = tach_truong(chuoi_1284, &["MDL", "MODEL"]).map(chuan) else { return false };
    if driver.is_empty() {
        return false;
    }
    let mfg = tach_truong(chuoi_1284, &["MFG", "MANUFACTURER"]).map(chuan);
    driver == mdl || mfg.is_some_and(|m| driver == format!("{} {}", m, mdl))
}

/// Số cổng của cổng USB ảo: `USB001` → 1. Chỉ đúng MỘT cổng dạng `USB` + chữ
/// số (không phân biệt hoa thường); cổng mạng/WSD/`LPT1`/nhiều cổng gộp
/// (`USB001,USB002` — printer pooling) → `None`: không biết thiết bị nào.
pub fn so_cong_usb(cong: &str) -> Option<u32> {
    let cong = cong.trim();
    let so = cong.get(..3).filter(|d| d.eq_ignore_ascii_case("USB")).and(cong.get(3..))?;
    if so.is_empty() || !so.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    so.parse().ok()
}

/// Chuỗi IEEE 1284 từ bộ đệm của `IOCTL_USBPRINT_GET_1284_ID`: hai byte đầu là
/// độ dài (big-endian, GỒM cả hai byte đó), sau là chữ ASCII. Độ dài khai sai
/// (lớn hơn số byte nhận được) → cắt theo số byte nhận được.
pub fn chuoi_1284(bo_dem: &[u8]) -> Option<String> {
    if bo_dem.len() < 2 {
        return None;
    }
    let khai = u16::from_be_bytes([bo_dem[0], bo_dem[1]]) as usize;
    let het = khai.clamp(2, bo_dem.len());
    Some(String::from_utf8_lossy(&bo_dem[2..het]).into_owned())
}

/// Gộp các dòng nhật ký GIỐNG NHAU liên tiếp mà vẫn giữ mạch thời gian: dòng đổi
/// thì ghi ngay; dòng giữ nguyên thì cứ `nhip` ghi lại một lần kèm số lần đã
/// gộp. Dùng cho nhật ký đọc USB liên tục (mỗi 500 ms) và vết từng hoá đơn.
#[derive(Debug, Default)]
pub struct GopDong {
    truoc: Option<String>,
    lap: usize,
    luc_ghi: Option<std::time::Instant>,
}

impl GopDong {
    /// Dòng CẦN GHI bây giờ (`None` = gộp vào dòng trước).
    pub fn them(&mut self, dong: &str, bay_gio: std::time::Instant, nhip: std::time::Duration) -> Option<String> {
        let giong = self.truoc.as_deref() == Some(dong);
        if giong && self.luc_ghi.is_some_and(|t| bay_gio.saturating_duration_since(t) < nhip) {
            self.lap += 1;
            return None;
        }
        let ra = if self.lap > 0 { format!("{} [+{} lan doc giong dong truoc]", dong, self.lap) } else { dong.to_string() };
        self.truoc = Some(dong.to_string());
        self.lap = 0;
        self.luc_ghi = Some(bay_gio);
        Some(ra)
    }
}

/// Nhịp tối thiểu ghi lại dòng giữ nguyên (nhật ký USB liên tục, 25/09).
pub const NHIP_GHI_LAI: std::time::Duration = std::time::Duration::from_secs(10);

/// Kết quả một lần hỏi máy in theo cổng.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocCong {
    /// Không phải máy in cục bộ trên đúng một cổng USB — lớp USB không áp dụng.
    KhongPhaiUsb,
    /// Worker đang cho Sumatra nộp job (`TamNgungDocUsb`) — không hỏi, không biết gì mới.
    TamNgung,
    /// Máy USB mà không tìm/mở/hỏi được thiết bị: máy in tắt, rút dây, hoặc
    /// không đọc được registry. Theo dõi tiếp coi "mất thiết bị" lúc máy đang
    /// giữ hoá đơn là hoá đơn có thể đã mất (tắt máy xoá bộ nhớ).
    KhongDocDuoc,
    Doc(DocUsb),
}

/// Máy in `cong` có phải máy CỤC BỘ trên đúng một cổng USB không → số cổng.
/// `may_chu` = PRINTER_INFO_2W.pServerName (rỗng = máy cục bộ); kết nối máy in
/// chia sẻ `\\PC\may` mang tên cổng CỦA MÁY CHỦ (`USB001`) — đọc thiết bị cục
/// bộ cùng số cổng là đọc nhầm máy.
pub fn la_cong_usb_cuc_bo(may_chu: &str, thuoc_tinh: u32, cong: &str) -> Option<u32> {
    if !may_chu.trim().is_empty() || thuoc_tinh & crate::su_co::co::PRINTER_ATTRIBUTE_NETWORK != 0 {
        return None;
    }
    so_cong_usb(cong)
}

/// Các đường dẫn thiết bị usbmon ghi cho ĐÚNG cổng `USB<cong>` (có thể nhiều:
/// thiết bị đã rút còn khoá cũ — người gọi thử mở từng cái). KHÔNG có đường lùi
/// "máy duy nhất": không khớp số cổng thì không đọc.
pub fn thiet_bi_cua_cong(ds: &[(Option<u32>, String)], cong: u32) -> Vec<&str> {
    ds.iter().filter(|(so, _)| *so == Some(cong)).map(|(_, d)| d.as_str()).collect()
}

static SO_TAM_NGUNG: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Tạm ngừng MỌI lần hỏi thiết bị USB (mọi luồng) khi còn giữ giá trị này —
/// worker giữ trong lúc Sumatra nộp job, đúng khe hàng đợi có thể còn rỗng mà
/// usbmon sắp mở cổng (usbprint có thể không cho mở chung).
#[must_use]
pub struct TamNgungDocUsb(());

impl TamNgungDocUsb {
    pub fn bat() -> Self {
        SO_TAM_NGUNG.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        TamNgungDocUsb(())
    }
}

impl Drop for TamNgungDocUsb {
    fn drop(&mut self) {
        SO_TAM_NGUNG.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn dang_tam_ngung() -> bool {
    SO_TAM_NGUNG.load(std::sync::atomic::Ordering::SeqCst) > 0
}

// ===================== Phần Win32 thật (chỉ Windows) =====================

#[cfg(windows)]
mod win {
    use super::*;
    use crate::nhat_ky;
    use std::sync::Mutex;
    use std::time::Instant;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    /// Khoá registry liệt kê mọi thiết bị USB máy in (GUID_DEVINTERFACE_USBPRINT).
    /// Mỗi khoá con: `#\Device Parameters` có "Base Name" (`USB`) + "Port Number"
    /// (1 → `USB001`) do usbmon ghi; `#` có "SymbolicLink" = đường dẫn mở thiết bị.
    const KHOA_THIET_BI: &str =
        r"SYSTEM\CurrentControlSet\Control\DeviceClasses\{28d78fad-5a12-11d1-ae5b-0000f803a8c2}";

    /// Đường dẫn thiết bị đã dò được cho từng số cổng (dò registry mỗi 500 ms là thừa).
    static DA_DO: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());

    /// Nhật ký USB LIÊN TỤC (chủ yêu cầu 25/09 — để cải thiện sau): MỌI lần hỏi
    /// đều qua đây; dòng giống nhau liên tiếp được gộp nhưng cứ `NHIP_GHI_LAI`
    /// ghi lại một lần. Chuỗi IEEE 1284 đầy đủ ghi mỗi khi nó đổi (có thể có
    /// trường khác ngoài STATUS lúc máy "chuẩn bị in").
    struct NhatKyCong {
        cong: u32,
        gop: GopDong,
        chuoi_truoc: Option<String>,
    }
    static NHAT_KY: Mutex<Vec<NhatKyCong>> = Mutex::new(Vec::new());

    fn ghi_lan_doc(cong: u32, dong: &str, chuoi_1284: Option<&str>) {
        let mut ds = NHAT_KY.lock().unwrap_or_else(|p| p.into_inner());
        let vi_tri = match ds.iter().position(|n| n.cong == cong) {
            Some(i) => i,
            None => {
                ds.push(NhatKyCong { cong, gop: GopDong::default(), chuoi_truoc: None });
                ds.len() - 1
            }
        };
        let n = &mut ds[vi_tri];
        let dong_ghi = n.gop.them(dong, Instant::now(), NHIP_GHI_LAI);
        let chuoi_moi = chuoi_1284.filter(|c| n.chuoi_truoc.as_deref() != Some(*c)).map(str::to_string);
        if let Some(c) = &chuoi_moi {
            n.chuoi_truoc = Some(c.clone());
        }
        drop(ds);
        if let Some(d) = dong_ghi {
            nhat_ky::ghi("usb_doc", &format!("cong=USB{:03} {}", cong, d));
        }
        if let Some(c) = chuoi_moi {
            nhat_ky::ghi("usb_1284", &format!("cong=USB{:03} {}", cong, c));
        }
    }

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Một lần đọc thiết bị ĐẦY ĐỦ: (DocUsb, chuỗi 1284 thô) hoặc lý do hỏng
    /// (để nhật ký nói đúng chỗ: mở thiết bị / hỏi byte / hỏi chuỗi).
    fn doc_thiet_bi(duong_dan: &str) -> Result<(DocUsb, Option<String>), String> {
        let wide: Vec<u16> = duong_dan.encode_utf16().chain(std::iter::once(0)).collect();
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                HANDLE::default(),
            )
        }
        .map_err(|e| format!("khong mo duoc thiet bi ({})", e.code().0 & 0xFFFF))?;
        let h = Handle(h);
        let mut byte = [0u8; 1];
        let mut nhan: u32 = 0;
        unsafe {
            DeviceIoControl(
                h.0,
                IOCTL_USBPRINT_GET_LPT_STATUS,
                None,
                0,
                Some(byte.as_mut_ptr().cast()),
                1,
                Some(&mut nhan),
                None,
            )
        }
        .map_err(|e| format!("hoi byte trang thai loi ({})", e.code().0 & 0xFFFF))?;
        if nhan < 1 {
            return Err("hoi byte trang thai: 0 byte".into());
        }
        let mut bo_dem = [0u8; 1024];
        let mut nhan_id: u32 = 0;
        let chuoi = unsafe {
            DeviceIoControl(
                h.0,
                IOCTL_USBPRINT_GET_1284_ID,
                None,
                0,
                Some(bo_dem.as_mut_ptr().cast()),
                bo_dem.len() as u32,
                Some(&mut nhan_id),
                None,
            )
        }
        .ok()
        .and_then(|_| chuoi_1284(&bo_dem[..(nhan_id as usize).min(bo_dem.len())]));
        let status = chuoi.as_deref().and_then(tach_status);
        Ok((DocUsb { byte: byte[0], status }, chuoi))
    }

    /// Mọi (số cổng, đường dẫn) usbmon đã ghi trong registry (kể cả thiết bị
    /// đã rút — người gọi thử mở để biết cái nào còn cắm).
    fn cac_thiet_bi() -> Result<Vec<(Option<u32>, String)>, String> {
        use winreg::enums::HKEY_LOCAL_MACHINE;
        use winreg::RegKey;
        let goc = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(KHOA_THIET_BI)
            .map_err(|e| format!("khong doc duoc registry DeviceClasses ({})", e))?;
        Ok(goc
            .enum_keys()
            .filter_map(Result::ok)
            .filter_map(|ten| {
                let khoa = goc.open_subkey(&ten).ok()?;
                let duong_dan: String = khoa.open_subkey("#").ok()?.get_value("SymbolicLink").ok()?;
                let tham_so = khoa.open_subkey(r"#\Device Parameters").ok();
                let la_usb = tham_so
                    .as_ref()
                    .and_then(|t| t.get_value::<String, _>("Base Name").ok())
                    .is_none_or(|b| b.eq_ignore_ascii_case("USB"));
                let so = tham_so.as_ref().and_then(|t| t.get_value::<u32, _>("Port Number").ok());
                la_usb.then_some((so, duong_dan))
            })
            .collect())
    }

    /// Đường dẫn mọi thiết bị máy in USB ĐANG CẮM (SetupAPI, GUID_DEVINTERFACE_USBPRINT
    /// — tài khoản thường xem được như Device Manager).
    fn thiet_bi_dang_cam() -> Result<Vec<String>, String> {
        use windows::Win32::Devices::DeviceAndDriverInstallation::{
            SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
            SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO,
            SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
        };
        use windows::Win32::Foundation::HWND;
        struct Tap(HDEVINFO);
        impl Drop for Tap {
            fn drop(&mut self) {
                unsafe {
                    let _ = SetupDiDestroyDeviceInfoList(self.0);
                }
            }
        }
        let guid = windows::core::GUID::from_u128(0x28d78fad_5a12_11d1_ae5b_0000f803a8c2);
        let tap = unsafe { SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), HWND::default(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) }
            .map_err(|e| format!("SetupDiGetClassDevs loi ({})", e.code().0 & 0xFFFF))?;
        let tap = Tap(tap);
        let mut ra = Vec::new();
        for i in 0..64u32 {
            let mut d = SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
            if unsafe { SetupDiEnumDeviceInterfaces(tap.0, None, &guid, i, &mut d) }.is_err() {
                break;
            }
            let mut can: u32 = 0;
            let _ = unsafe { SetupDiGetDeviceInterfaceDetailW(tap.0, &d, None, 0, Some(&mut can), None) };
            if !(8..=8192).contains(&can) {
                continue;
            }
            // Bộ đệm căn 4 byte (SP_DEVICE_INTERFACE_DETAIL_DATA_W có u32 đầu).
            let mut bo_dem = vec![0u32; (can as usize).div_ceil(4)];
            let p = bo_dem.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            unsafe {
                (*p).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            }
            if unsafe { SetupDiGetDeviceInterfaceDetailW(tap.0, &d, Some(p), can, None, None) }.is_err() {
                continue;
            }
            // DevicePath bắt đầu ngay sau cbSize (u32), kết thúc bằng \0, nằm trong `can` byte.
            let so_u16 = (can as usize - 4) / 2;
            let chu = unsafe { std::slice::from_raw_parts((bo_dem.as_ptr() as *const u8).add(4) as *const u16, so_u16) };
            let het = chu.iter().position(|&c| c == 0).unwrap_or(so_u16);
            ra.push(String::from_utf16_lossy(&chu[..het]));
        }
        Ok(ra)
    }

    /// Dò thiết bị của cổng `USB<cong>` (máy in Windows dùng driver `ten_driver`):
    /// (1) khoá registry có "Port Number" KHỚP mà mở + hỏi được; (2) không có →
    /// thiết bị ĐANG CẮM có model trùng tên driver, chỉ khi đúng MỘT cái. Không
    /// có → lý do đủ hai đường (không đoán thiết bị khác).
    fn do_thiet_bi(cong: u32, ten_driver: &str) -> Result<(String, DocUsb, Option<String>), String> {
        let mut loi = Vec::new();
        match cac_thiet_bi() {
            Ok(ds) => {
                let khop = thiet_bi_cua_cong(&ds, cong);
                for d in &khop {
                    match doc_thiet_bi(d) {
                        Ok((doc, chuoi)) => return Ok((d.to_string(), doc, chuoi)),
                        Err(e) => loi.push(e),
                    }
                }
                loi.insert(0, format!("registry: {} khoa, {} khop so cong {}", ds.len(), khop.len(), cong));
            }
            Err(e) => loi.insert(0, format!("registry: {}", e)),
        }
        let dang_cam = thiet_bi_dang_cam()?;
        let mut trung = Vec::new();
        let mut model_khac = Vec::new();
        for d in &dang_cam {
            match doc_thiet_bi(d) {
                Ok((doc, Some(chuoi))) if khop_model(&chuoi, ten_driver) => trung.push((d.clone(), doc, Some(chuoi))),
                Ok((_, chuoi)) => model_khac.push(chuoi.as_deref().and_then(|c| tach_truong(c, &["MDL", "MODEL"])).unwrap_or("?").to_string()),
                Err(e) => loi.push(e),
            }
        }
        if trung.len() == 1 {
            return Ok(trung.remove(0));
        }
        Err(format!(
            "{}; dang cam {} thiet bi, {} trung model \"{}\" (model khac: {})",
            loi.join("; "),
            dang_cam.len(),
            trung.len(),
            ten_driver,
            if model_khac.is_empty() { "-".to_string() } else { model_khac.join(", ") }
        ))
    }

    /// Hỏi máy in (PRINTER_INFO_2W: `may_chu` = pServerName, `thuoc_tinh`,
    /// `cong` = pPortName, `ten_driver` = pDriverName). Không phải máy USB cục bộ
    /// → `KhongPhaiUsb`.
    pub fn doc_theo_cong(may_chu: &str, thuoc_tinh: u32, cong: &str, ten_driver: &str) -> DocCong {
        let Some(so) = la_cong_usb_cuc_bo(may_chu, thuoc_tinh, cong) else {
            return DocCong::KhongPhaiUsb;
        };
        if dang_tam_ngung() {
            return DocCong::TamNgung;
        }
        let da_do = DA_DO.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(s, _)| *s == so).map(|(_, d)| d.clone());
        let ket_qua = match da_do.as_deref().map(doc_thiet_bi) {
            Some(Ok(doc)) => Ok(doc),
            // Chưa dò, hoặc rút ra cắm lại có thể đổi đường dẫn — dò lại theo số cổng.
            _ => {
                let moi = do_thiet_bi(so, ten_driver);
                let mut nho = DA_DO.lock().unwrap_or_else(|p| p.into_inner());
                nho.retain(|(s, _)| *s != so);
                if let Ok((duong_dan, _, _)) = &moi {
                    if da_do.as_deref() != Some(duong_dan.as_str()) {
                        nhat_ky::ghi("usb_thiet_bi", &format!("cong=USB{:03} {}", so, duong_dan));
                    }
                    nho.push((so, duong_dan.clone()));
                }
                moi.map(|(_, doc, chuoi)| (doc, chuoi))
            }
        };
        match ket_qua {
            Ok((doc, chuoi)) => {
                ghi_lan_doc(so, &doc.mo_ta(), chuoi.as_deref());
                DocCong::Doc(doc)
            }
            Err(ly_do) => {
                ghi_lan_doc(so, &format!("KHONG DOC DUOC: {}", ly_do), None);
                DocCong::KhongDocDuoc
            }
        }
    }
}

#[cfg(windows)]
pub use win::doc_theo_cong;

/// Mac/dev: không có thiết bị USB Windows để đọc.
#[cfg(not(windows))]
pub fn doc_theo_cong(_may_chu: &str, _thuoc_tinh: u32, _cong: &str, _ten_driver: &str) -> DocCong {
    DocCong::KhongPhaiUsb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(byte: u8, status: Option<&str>) -> DocUsb {
        DocUsb { byte, status: status.map(str::to_string) }
    }

    #[test]
    fn ba_tinh_trang_do_that_o_hcm() {
        assert_eq!(doc(0x18, Some("IDLE")).tinh_trang(), TinhTrangUsb::Ranh, "có giấy, rảnh");
        assert_eq!(doc(0x98, Some("BUSY")).tinh_trang(), TinhTrangUsb::DangIn, "đang in bình thường");
        assert_eq!(
            doc(0x90, Some("BUSY")).tinh_trang(),
            TinhTrangUsb::Loi(MaSuCo::CanXuLy),
            "hết giấy đang giữ hoá đơn: chỉ bit3 tắt — không biết là hết giấy hay kẹt"
        );
    }

    #[test]
    fn bit_het_giay_chuan_thi_ra_het_giay_ke_ca_khi_bit3_con_bat() {
        assert_eq!(doc(0x30, None).tinh_trang(), TinhTrangUsb::Loi(MaSuCo::HetGiay));
        assert_eq!(doc(0x38, Some("IDLE")).tinh_trang(), TinhTrangUsb::Loi(MaSuCo::HetGiay));
    }

    #[test]
    fn loi_thang_moi_status() {
        // Bit3 tắt là lỗi, dù STATUS nói IDLE — máy nói "lỗi" thì không báo "rảnh".
        assert_eq!(doc(0x10, Some("IDLE")).ma_su_co(), Some(MaSuCo::CanXuLy));
        assert_eq!(doc(0x18, Some("IDLE")).ma_su_co(), None);
    }

    /// Chỉ IDLE là rảnh; chữ lạ = đang làm việc (không bao giờ "xong" sớm);
    /// thiếu STATUS = thiếu tin.
    #[test]
    fn status_la_la_dang_lam_viec_thieu_la_thieu_tin() {
        assert_eq!(doc(0x18, None).tinh_trang(), TinhTrangUsb::KhongLoi);
        assert_eq!(doc(0x18, Some("SLEEP")).tinh_trang(), TinhTrangUsb::DangIn);
        assert_eq!(doc(0x98, Some("PRINTING")).tinh_trang(), TinhTrangUsb::DangIn);
        assert_eq!(doc(0x18, Some("WARMUP")).tinh_trang(), TinhTrangUsb::DangIn);
    }

    #[test]
    fn mo_ta_noi_ro_byte_status_va_viec_can_lam() {
        assert_eq!(
            doc(0x90, Some("BUSY")).mo_ta(),
            "USB 0x90 STATUS:BUSY — máy in báo lỗi qua USB (thường là hết giấy, kẹt giấy hoặc mở nắp)"
        );
        assert_eq!(doc(0x18, Some("IDLE")).mo_ta(), "USB 0x18 STATUS:IDLE");
        assert_eq!(doc(0x30, None).mo_ta(), "USB 0x30 — máy in báo hết giấy qua USB");
    }

    #[test]
    fn tach_status_tu_chuoi_1284_that() {
        let het_giay = "MFG:HP;CMD:SPL,URF,FWV,PIC,EXT,PWGRaster;PRN:4ZB79A;MDL:HP Laser 103 107 108;CLS:PRINTER;CID:HPLJPCLMSMV1;MODE:SPL3,R000105;STATUS:BUSY;";
        assert_eq!(tach_status(het_giay).as_deref(), Some("BUSY"));
        assert_eq!(tach_status("MFG:HP;status: idle ;").as_deref(), Some("IDLE"));
        assert_eq!(tach_status("MFG:HP;MDL:X;"), None);
        assert_eq!(tach_status("MFG:HP;STATUS:;"), None);
        // "STATUS" phải là KHOÁ, không phải chữ nằm trong giá trị khác.
        assert_eq!(tach_status("DES:NO STATUS:X;MFG:HP;"), None);
    }

    #[test]
    fn so_cong_chi_nhan_dung_mot_cong_usb() {
        assert_eq!(so_cong_usb("USB001"), Some(1));
        assert_eq!(so_cong_usb(" usb012 "), Some(12));
        for cong in ["", "USB", "LPT1:", "WSD-1234", "USB001,USB002", "USBX01", "192.168.1.5", "USB-001"] {
            assert_eq!(so_cong_usb(cong), None, "{:?}", cong);
        }
    }

    #[test]
    fn chuoi_1284_doc_do_dai_big_endian_va_chong_khai_sai() {
        let mut bo_dem = vec![0u8, 13];
        bo_dem.extend_from_slice(b"STATUS:IDLE;rac");
        assert_eq!(chuoi_1284(&bo_dem).as_deref(), Some("STATUS:IDLE"));
        // Khai dài hơn số byte nhận được → cắt theo số byte nhận được.
        assert_eq!(chuoi_1284(&[0x10, 0x00, b'A', b'B']).as_deref(), Some("AB"));
        assert_eq!(chuoi_1284(&[0x00]), None);
        assert_eq!(chuoi_1284(&[0x00, 0x00]).as_deref(), Some(""));
    }

    #[test]
    fn ma_ioctl_dung_ctl_code() {
        // CTL_CODE(0x22, n, METHOD_BUFFERED=0, FILE_ANY_ACCESS=0) = 0x22<<16 | n<<2.
        assert_eq!(IOCTL_USBPRINT_GET_LPT_STATUS, (0x22 << 16) | (12 << 2));
        assert_eq!(IOCTL_USBPRINT_GET_1284_ID, (0x22 << 16) | (13 << 2));
    }

    #[test]
    fn chi_may_cuc_bo_tren_cong_usb() {
        use crate::su_co::co::PRINTER_ATTRIBUTE_NETWORK;
        assert_eq!(la_cong_usb_cuc_bo("", 0x40, "USB001"), Some(1));
        // Kết nối máy in chia sẻ \\PC\may: cổng là của MÁY CHỦ — không đọc thiết bị cục bộ.
        assert_eq!(la_cong_usb_cuc_bo(r"\\PC-KHO", 0, "USB001"), None);
        assert_eq!(la_cong_usb_cuc_bo("", PRINTER_ATTRIBUTE_NETWORK, "USB001"), None);
        assert_eq!(la_cong_usb_cuc_bo("", 0, "WSD-5a6b"), None);
    }

    /// Giám sát 25/09: KHÔNG có đường lùi "máy USB duy nhất" — cửa hàng cắm thêm
    /// máy in tem/bill thì đọc nhầm, báo `da_in` cho hoá đơn đang kẹt trong máy HP.
    #[test]
    fn thiet_bi_chi_theo_dung_so_cong() {
        let ds = vec![
            (Some(2), r"\\?\usb#vid_tem".to_string()),
            (None, r"\\?\usb#khong_so".to_string()),
            (Some(1), r"\\?\usb#hp_cu".to_string()),
            (Some(1), r"\\?\usb#hp".to_string()),
        ];
        assert_eq!(thiet_bi_cua_cong(&ds, 1), vec![r"\\?\usb#hp_cu", r"\\?\usb#hp"]);
        assert!(thiet_bi_cua_cong(&ds, 3).is_empty(), "không khớp số cổng thì KHÔNG đoán");
    }

    /// Bộ đếm dùng chung cả tiến trình (test in khác chạy song song cũng giữ) —
    /// chỉ kiểm điều luôn đúng: còn giữ ít nhất một khoá thì đang tạm ngừng.
    #[test]
    fn tam_ngung_doc_usb_long_nhau() {
        let a = TamNgungDocUsb::bat();
        let b = TamNgungDocUsb::bat();
        assert!(dang_tam_ngung());
        drop(a);
        assert!(dang_tam_ngung(), "còn b");
        drop(b);
    }

    /// Nhật ký USB liên tục: dòng đổi ghi ngay, dòng giữ nguyên gộp nhưng vẫn
    /// ghi lại mỗi nhịp kèm số lần đã gộp.
    #[test]
    fn gop_dong_giu_mach_thoi_gian() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let nhip = Duration::from_secs(10);
        let mut g = GopDong::default();
        assert_eq!(g.them("0x18 IDLE", t0, nhip).as_deref(), Some("0x18 IDLE"));
        for i in 1..=5 {
            assert_eq!(g.them("0x18 IDLE", t0 + Duration::from_millis(500 * i), nhip), None);
        }
        assert_eq!(g.them("0x98 BUSY", t0 + Duration::from_secs(3), nhip).as_deref(), Some("0x98 BUSY [+5 lan doc giong dong truoc]"));
        assert_eq!(g.them("0x98 BUSY", t0 + Duration::from_secs(4), nhip), None);
        // Giữ nguyên quá nhịp → ghi lại.
        assert_eq!(g.them("0x98 BUSY", t0 + Duration::from_secs(14), nhip).as_deref(), Some("0x98 BUSY [+1 lan doc giong dong truoc]"));
    }

    /// Khớp model thiết bị với driver máy in Windows — chuỗi 1284 THẬT của máy HCM.
    #[test]
    fn khop_model_theo_chuoi_that() {
        let hp = "MFG:HP;CMD:SPL,URF,FWV,PIC,EXT,PWGRaster;PRN:4ZB79A;MDL:HP Laser 103 107 108;CLS:PRINTER;CID:HPLJPCLMSMV1;MODE:SPL3,R000105;STATUS:IDLE;";
        assert!(khop_model(hp, "HP Laser 103 107 108"));
        assert!(khop_model(hp, "  hp laser  103 107 108 "));
        assert!(!khop_model(hp, "HP LaserJet Pro 4003"));
        assert!(!khop_model(hp, "HP Laser 107"), "không so 'chứa'");
        assert!(!khop_model(hp, ""));
        // Hãng tách riêng MFG, driver ghi cả hãng.
        assert!(khop_model("MFG:Brother;MDL:HL-L2320D series;", "Brother HL-L2320D series"));
        assert!(!khop_model("MFG:Xprinter;MDL:XP-365B;", "HP Laser 103 107 108"));
        assert!(!khop_model("MFG:HP;CLS:PRINTER;", "HP Laser 103 107 108"), "không có MDL");
        assert_eq!(tach_truong(hp, &["MDL", "MODEL"]), Some("HP Laser 103 107 108"));
    }
}
