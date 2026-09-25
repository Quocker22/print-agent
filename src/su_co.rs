// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mã sự cố / trạng thái máy in dùng chung ba phần (backend, app, giao diện) —
//! hợp đồng `HOP-DONG-NHAT-KY-MAY-IN.md` §1 — và ánh xạ cờ Win32 → mã.
//!
//! VÌ SAO ánh xạ viết bằng HẰNG SỐ tự khai chứ không `use windows::...`: crate
//! `windows` chỉ có trên Windows, còn test chạy trên Mac/CI. Hàm thuần nhận
//! `u32` + hằng tự khai thì test được mọi nơi; khối `const _` cuối file so từng
//! hằng với crate `windows` LÚC BIÊN DỊCH trên Windows — gõ sai một số là
//! `cargo check --target x86_64-pc-windows-msvc` đỏ, không phải đợi ra máy shop.

use serde::Serialize;

/// Mã ở §1. Serialize ra đúng chuỗi mã (`het_giay`, `khong_tim_thay_may_in`, …)
/// để đi thẳng vào `loai`/`trangThai` của các event socket.io.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaSuCo {
    HetGiay,
    KetGiay,
    Offline,
    MoNap,
    HetMuc,
    CanXuLy,
    LoiMayIn,
    KhongTimThayMayIn,
    LoiSumatra,
    LoiPdf,
    KhongXacNhan,
    BinhThuong,
}

/// Mức ở §1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucDo {
    Loi,
    CanhBao,
    ThongTin,
}

impl MaSuCo {
    /// Chuỗi mã — đúng như serde sinh ra (test khoá hai đường này trùng nhau).
    pub fn ma(self) -> &'static str {
        match self {
            MaSuCo::HetGiay => "het_giay",
            MaSuCo::KetGiay => "ket_giay",
            MaSuCo::Offline => "offline",
            MaSuCo::MoNap => "mo_nap",
            MaSuCo::HetMuc => "het_muc",
            MaSuCo::CanXuLy => "can_xu_ly",
            MaSuCo::LoiMayIn => "loi_may_in",
            MaSuCo::KhongTimThayMayIn => "khong_tim_thay_may_in",
            MaSuCo::LoiSumatra => "loi_sumatra",
            MaSuCo::LoiPdf => "loi_pdf",
            MaSuCo::KhongXacNhan => "khong_xac_nhan",
            MaSuCo::BinhThuong => "binh_thuong",
        }
    }

    /// Nhãn tiếng Việt — nguyên văn cột "Nhãn" của §1.
    pub fn nhan(self) -> &'static str {
        match self {
            MaSuCo::HetGiay => "Hết giấy",
            MaSuCo::KetGiay => "Kẹt giấy",
            MaSuCo::Offline => "Máy in offline / mất kết nối máy in",
            MaSuCo::MoNap => "Nắp máy in đang mở",
            MaSuCo::HetMuc => "Hết mực / sắp hết mực",
            MaSuCo::CanXuLy => "Máy in cần người xử lý",
            MaSuCo::LoiMayIn => "Máy in báo lỗi",
            MaSuCo::KhongTimThayMayIn => "Không tìm thấy máy in trong Windows",
            MaSuCo::LoiSumatra => "Không gọi được / SumatraPDF lỗi",
            MaSuCo::LoiPdf => "File PDF hỏng",
            MaSuCo::KhongXacNhan => "Đã gửi máy in nhưng không xác nhận được đã in",
            MaSuCo::BinhThuong => "Máy in bình thường (đã hết sự cố)",
        }
    }

    /// "Việc cần làm" — vế đầu dòng dưới của dải cảnh báo (view_model.rs).
    /// Ngắn, động từ đầu câu: người đứng quầy đọc lướt phải biết làm gì ngay.
    ///
    /// KHÔNG CÂU NÀO ĐƯỢC BẢO "IN LẠI" (sửa sau giám sát 25/09): bản trước ghi
    /// "nạp giấy rồi in lại" / "chưa thì in lại tay" — NV in tay đúng lúc job
    /// còn nằm trong hàng đợi Windows (hoặc backend tự gửi lại) là HAI tờ. Việc
    /// in lại là của hệ thống; NV chỉ xử lý máy. Test `huong_dan_khong_bao_gio_bao_in_lai` khoá.
    pub fn huong_dan(self) -> &'static str {
        match self {
            MaSuCo::HetGiay => "Nạp giấy vào khay",
            MaSuCo::KetGiay => "Gỡ giấy kẹt rồi đóng nắp",
            MaSuCo::Offline => "Bật máy in, kiểm dây mạng/USB",
            MaSuCo::MoNap => "Đóng nắp máy in",
            MaSuCo::HetMuc => "Chuẩn bị thay mực",
            // Máy HP Laser 107 (HCM) không có màn hình — chỉ đèn; lỗi qua USB
            // không nói rõ hết giấy hay kẹt (usb_may_in.rs) nên kể đủ ba việc.
            MaSuCo::CanXuLy => "Xem đèn/màn hình máy in: nạp giấy, gỡ giấy kẹt, đóng nắp",
            MaSuCo::LoiMayIn => "Xem màn hình máy in, tắt/bật lại máy in",
            MaSuCo::KhongTimThayMayIn => "Chọn lại máy in trong app",
            MaSuCo::LoiSumatra => "Báo kỹ thuật: SumatraPDF lỗi",
            MaSuCo::LoiPdf => "Báo kỹ thuật: file PDF hỏng",
            MaSuCo::KhongXacNhan => "Xem khay giấy",
            MaSuCo::BinhThuong => "",
        }
    }

    /// Sự cố NẰM Ở MÁY IN (vật lý / trạng thái máy) — hoá đơn gặp nó thì đang
    /// chờ trong máy in và tự ra khi NV xử lý xong máy. Khác `loi_sumatra`,
    /// `loi_pdf`, `khong_xac_nhan`, `khong_tim_thay_may_in`: những mã đó không
    /// nói được gì về chỗ hoá đơn đang nằm, nên dải cảnh báo nói "chưa xác nhận".
    pub fn la_su_co_may_in(self) -> bool {
        matches!(
            self,
            MaSuCo::HetGiay
                | MaSuCo::KetGiay
                | MaSuCo::Offline
                | MaSuCo::MoNap
                | MaSuCo::HetMuc
                | MaSuCo::CanXuLy
                | MaSuCo::LoiMayIn
        )
    }

    pub fn muc(self) -> MucDo {
        match self {
            MaSuCo::HetMuc => MucDo::CanhBao,
            MaSuCo::BinhThuong => MucDo::ThongTin,
            _ => MucDo::Loi,
        }
    }

    /// Sự cố làm job KHÔNG in được (mức `loi`). `het_muc` chỉ là cảnh báo —
    /// máy vẫn in (mực yếu), nên KHÔNG được coi như lỗi chặn in: coi nó là lỗi
    /// thì mọi job lúc mực yếu thành "lỗi trước khi in" và bị xoá khỏi hàng đợi.
    pub fn chan_in(self) -> bool {
        self.muc() == MucDo::Loi
    }

    /// Hợp đồng v4 §1 "không tiêu lượt thử": `loi` mang mã này thì backend giữ
    /// hoá đơn CHỜ máy hết lỗi rồi tự gửi lại, KHÔNG tính lượt (khớp
    /// `laMaChoMayKhongTieuLuot` của backend) — mã chặn in CẤP MÁY trừ
    /// `loi_may_in`. Mọi mã khác (`loi_may_in`, `loi_pdf`, `loi_sumatra`, không
    /// mã) tiêu một lượt: quá 5 lượt backend báo `that_bai`. Giao diện chỉ được
    /// hứa "hệ thống TỰ in lại" với nhóm mã này (giám sát vòng 3, T4).
    pub fn khong_tieu_luot(self) -> bool {
        matches!(
            self,
            MaSuCo::HetGiay | MaSuCo::KetGiay | MaSuCo::Offline | MaSuCo::MoNap | MaSuCo::CanXuLy | MaSuCo::KhongTimThayMayIn
        )
    }
}

/// Hằng cờ Win32 (winspool.h). Giá trị khoá bằng `const _` cuối file trên Windows.
pub mod co {
    pub const PRINTER_STATUS_PAUSED: u32 = 0x0000_0001;
    pub const PRINTER_STATUS_ERROR: u32 = 0x0000_0002;
    pub const PRINTER_STATUS_PAPER_JAM: u32 = 0x0000_0008;
    pub const PRINTER_STATUS_PAPER_OUT: u32 = 0x0000_0010;
    pub const PRINTER_STATUS_PAPER_PROBLEM: u32 = 0x0000_0040;
    pub const PRINTER_STATUS_OFFLINE: u32 = 0x0000_0080;
    pub const PRINTER_STATUS_NOT_AVAILABLE: u32 = 0x0000_1000;
    pub const PRINTER_STATUS_TONER_LOW: u32 = 0x0002_0000;
    pub const PRINTER_STATUS_NO_TONER: u32 = 0x0004_0000;
    pub const PRINTER_STATUS_USER_INTERVENTION: u32 = 0x0010_0000;
    pub const PRINTER_STATUS_DOOR_OPEN: u32 = 0x0040_0000;
    pub const PRINTER_STATUS_SERVER_UNKNOWN: u32 = 0x0080_0000;

    /// PRINTER_INFO_2W.Attributes — "Use Printer Offline" (spooler giữ mọi job).
    pub const PRINTER_ATTRIBUTE_WORK_OFFLINE: u32 = 0x0000_0400;

    pub const JOB_STATUS_PAUSED: u32 = 0x0000_0001;
    pub const JOB_STATUS_ERROR: u32 = 0x0000_0002;
    pub const JOB_STATUS_DELETING: u32 = 0x0000_0004;
    pub const JOB_STATUS_SPOOLING: u32 = 0x0000_0008;
    pub const JOB_STATUS_PRINTING: u32 = 0x0000_0010;
    pub const JOB_STATUS_OFFLINE: u32 = 0x0000_0020;
    pub const JOB_STATUS_PAPEROUT: u32 = 0x0000_0040;
    pub const JOB_STATUS_PRINTED: u32 = 0x0000_0080;
    pub const JOB_STATUS_DELETED: u32 = 0x0000_0100;
    pub const JOB_STATUS_BLOCKED_DEVQ: u32 = 0x0000_0200;
    pub const JOB_STATUS_USER_INTERVENTION: u32 = 0x0000_0400;
    pub const JOB_STATUS_RESTART: u32 = 0x0000_0800;
    pub const JOB_STATUS_COMPLETE: u32 = 0x0000_1000;
    pub const JOB_STATUS_RETAINED: u32 = 0x0000_2000;
}

/// Thứ tự ưu tiên khi nhiều cờ cùng bật (§1): cờ cụ thể thắng cờ chung —
/// `PAPER_OUT|ERROR` phải ra "Hết giấy" (NV biết nạp giấy), không phải "Máy in
/// báo lỗi" (NV không biết làm gì).
const UU_TIEN: [MaSuCo; 7] = [
    MaSuCo::HetGiay,
    MaSuCo::KetGiay,
    MaSuCo::Offline,
    MaSuCo::MoNap,
    MaSuCo::CanXuLy,
    MaSuCo::LoiMayIn,
    MaSuCo::HetMuc,
];

/// (cờ, mã, tên cờ để ghi `chiTiet`) — PRINTER_STATUS_*, đúng bảng §1, thêm
/// PAUSED (hàng đợi bị tạm dừng: spooler giữ mọi job — NV phải bấm Resume).
const BANG_MAY_IN: [(u32, MaSuCo, &str); 12] = [
    (co::PRINTER_STATUS_PAPER_OUT, MaSuCo::HetGiay, "PAPER_OUT"),
    (co::PRINTER_STATUS_PAPER_PROBLEM, MaSuCo::HetGiay, "PAPER_PROBLEM"),
    (co::PRINTER_STATUS_PAPER_JAM, MaSuCo::KetGiay, "PAPER_JAM"),
    (co::PRINTER_STATUS_OFFLINE, MaSuCo::Offline, "OFFLINE"),
    (co::PRINTER_STATUS_NOT_AVAILABLE, MaSuCo::Offline, "NOT_AVAILABLE"),
    (co::PRINTER_STATUS_SERVER_UNKNOWN, MaSuCo::Offline, "SERVER_UNKNOWN"),
    (co::PRINTER_STATUS_DOOR_OPEN, MaSuCo::MoNap, "DOOR_OPEN"),
    (co::PRINTER_STATUS_NO_TONER, MaSuCo::HetMuc, "NO_TONER"),
    (co::PRINTER_STATUS_TONER_LOW, MaSuCo::HetMuc, "TONER_LOW"),
    (co::PRINTER_STATUS_USER_INTERVENTION, MaSuCo::CanXuLy, "USER_INTERVENTION"),
    (co::PRINTER_STATUS_PAUSED, MaSuCo::CanXuLy, "PAUSED"),
    (co::PRINTER_STATUS_ERROR, MaSuCo::LoiMayIn, "ERROR"),
];

/// JOB_STATUS_*, đúng bảng §1.
const BANG_JOB: [(u32, MaSuCo, &str); 5] = [
    (co::JOB_STATUS_PAPEROUT, MaSuCo::HetGiay, "PAPEROUT"),
    (co::JOB_STATUS_OFFLINE, MaSuCo::Offline, "OFFLINE"),
    (co::JOB_STATUS_USER_INTERVENTION, MaSuCo::CanXuLy, "USER_INTERVENTION"),
    (co::JOB_STATUS_ERROR, MaSuCo::LoiMayIn, "ERROR"),
    (co::JOB_STATUS_BLOCKED_DEVQ, MaSuCo::LoiMayIn, "BLOCKED_DEVQ"),
];

fn chon_theo_uu_tien(status: u32, bang: &[(u32, MaSuCo, &str)]) -> Option<MaSuCo> {
    UU_TIEN
        .iter()
        .copied()
        .find(|ma| bang.iter().any(|(c, m, _)| m == ma && status & c != 0))
}

/// Tập mã §1 (bitset theo thứ tự khai báo của `MaSuCo`) — `Copy`, so sánh được.
///
/// Dùng cho "cờ nền" (R-B, giám sát vòng 2): mã sự cố CẤP MÁY đã có sẵn ở lần
/// đọc đầu của job không được tính chống lại job. So TỪNG mã chứ không so mã
/// ưu tiên cao nhất: nền `PAPER_OUT|ERROR`, NV nạp giấy nhưng `ERROR` còn —
/// `loi_may_in` vẫn là nền, không phải sự cố mới.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TapMa(u16);

impl TapMa {
    pub fn them(&mut self, ma: MaSuCo) {
        self.0 |= 1 << ma as u16;
    }

    pub fn co(self, ma: MaSuCo) -> bool {
        self.0 & (1 << ma as u16) != 0
    }

    /// Mã có trong `self` mà không có trong `khac`.
    pub fn tru(self, khac: TapMa) -> TapMa {
        TapMa(self.0 & !khac.0)
    }

    #[cfg(test)]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Mã CHẶN IN ưu tiên cao nhất (§1) trong tập; `None` = không có mã chặn in.
    pub fn chan_in_uu_tien(self) -> Option<MaSuCo> {
        UU_TIEN
            .iter()
            .copied()
            .chain([MaSuCo::KhongTimThayMayIn])
            .find(|m| m.chan_in() && self.co(*m))
    }
}

impl FromIterator<MaSuCo> for TapMa {
    fn from_iter<I: IntoIterator<Item = MaSuCo>>(it: I) -> Self {
        let mut t = TapMa::default();
        for m in it {
            t.them(m);
        }
        t
    }
}

/// MỌI mã §1 mà một lần GetPrinterW (Status + Attributes) cho ra — không chỉ
/// mã ưu tiên cao nhất (xem `TapMa`).
pub fn cac_ma_may_in(status: u32, thuoc_tinh: u32) -> TapMa {
    let mut t: TapMa = BANG_MAY_IN.iter().filter(|(c, _, _)| status & c != 0).map(|(_, m, _)| *m).collect();
    if thuoc_tinh & co::PRINTER_ATTRIBUTE_WORK_OFFLINE != 0 {
        t.them(MaSuCo::Offline);
    }
    t
}

/// Mã ưu tiên cao hơn theo §1 trong hai mã (mã ngoài bảng ưu tiên — vd
/// `khong_tim_thay_may_in` — đứng sau mọi mã trong bảng; `binh_thuong` thua hết).
pub fn uu_tien_hon(a: MaSuCo, b: MaSuCo) -> MaSuCo {
    let hang = |m: MaSuCo| match m {
        MaSuCo::BinhThuong => usize::MAX,
        m => UU_TIEN.iter().position(|x| *x == m).unwrap_or(UU_TIEN.len()),
    };
    if hang(b) < hang(a) {
        b
    } else {
        a
    }
}

/// PRINTER_INFO_2W.Status → mã §1. Không cờ sự cố nào → `BinhThuong`
/// (cờ thông tin như BUSY/PRINTING/POWER_SAVE không phải sự cố). Mã chạy
/// thật dùng `tinh_trang_may_in` (có cả Attributes); hàm này để test bảng cờ.
#[cfg(test)]
pub fn ma_tu_co_may_in(status: u32) -> MaSuCo {
    chon_theo_uu_tien(status, &BANG_MAY_IN).unwrap_or(MaSuCo::BinhThuong)
}

/// Câu chiTiet khi hàng đợi máy in bị Pause (R10) — NV đọc là biết phải Resume.
pub const CHU_HANG_DOI_TAM_DUNG: &str = "Hàng đợi máy in đang tạm dừng (Pause)";
/// Câu chiTiet khi máy in để "Use Printer Offline" (Attributes WORK_OFFLINE).
pub const CHU_DUNG_OFFLINE: &str = "Máy in đang để chế độ 'Use Printer Offline'";

/// Trạng thái máy in đọc từ MỘT lần GetPrinterW (Status + Attributes) → (mã §1, chiTiet).
///
/// `Attributes & WORK_OFFLINE` ("Use Printer Offline" bật trong Windows) cũng
/// là `offline`: spooler giữ mọi job lại, Status có thể không bật cờ nào. Nhiều
/// nguồn cùng lúc thì vẫn lấy theo thứ tự ưu tiên §1.
pub fn tinh_trang_may_in(status: u32, thuoc_tinh: u32) -> (MaSuCo, Option<String>) {
    let tu_co = chon_theo_uu_tien(status, &BANG_MAY_IN);
    let dung_offline = thuoc_tinh & co::PRINTER_ATTRIBUTE_WORK_OFFLINE != 0;
    let ma = UU_TIEN
        .iter()
        .copied()
        .find(|m| Some(*m) == tu_co || (dung_offline && *m == MaSuCo::Offline))
        .unwrap_or(MaSuCo::BinhThuong);
    if ma == MaSuCo::BinhThuong {
        return (ma, None);
    }
    let co_may = mo_ta_co_may_in(status);
    let chi_tiet = if ma == MaSuCo::Offline && tu_co != Some(MaSuCo::Offline) {
        // Chỉ vì Attributes — Status không có cờ offline nào.
        format!("{}; {}, Attributes 0x{:08X}", CHU_DUNG_OFFLINE, co_may, thuoc_tinh)
    } else if ma == MaSuCo::CanXuLy && status & co::PRINTER_STATUS_USER_INTERVENTION == 0 {
        // CanXuLy mà không có USER_INTERVENTION ⇒ chỉ có thể do PAUSED.
        format!("{}; {}", CHU_HANG_DOI_TAM_DUNG, co_may)
    } else {
        co_may
    };
    (ma, Some(chi_tiet))
}

/// JOB_INFO_2W.Status → mã §1, `None` khi job không mang cờ sự cố nào.
pub fn ma_tu_co_job(status: u32) -> Option<MaSuCo> {
    chon_theo_uu_tien(status, &BANG_JOB)
}

/// Bỏ dấu tiếng Việt + chữ thường, để so câu trạng thái driver không phân biệt
/// hoa thường, có/không dấu ("Hết giấy", "HET GIAY", "hết giấy" đều thành
/// "het giay"). Dấu tổ hợp (NFD, U+0300–U+036F) cũng bị bỏ.
fn bo_dau_thuong(chu: &str) -> String {
    let mut ra = String::with_capacity(chu.len());
    for c in chu.chars().flat_map(char::to_lowercase) {
        if ('\u{300}'..='\u{36f}').contains(&c) {
            continue;
        }
        let goc = match c {
            'à' | 'á' | 'ạ' | 'ả' | 'ã' | 'â' | 'ầ' | 'ấ' | 'ậ' | 'ẩ' | 'ẫ' | 'ă' | 'ằ' | 'ắ' | 'ặ' | 'ẳ' | 'ẵ' => 'a',
            'è' | 'é' | 'ẹ' | 'ẻ' | 'ẽ' | 'ê' | 'ề' | 'ế' | 'ệ' | 'ể' | 'ễ' => 'e',
            'ì' | 'í' | 'ị' | 'ỉ' | 'ĩ' => 'i',
            'ò' | 'ó' | 'ọ' | 'ỏ' | 'õ' | 'ô' | 'ồ' | 'ố' | 'ộ' | 'ổ' | 'ỗ' | 'ơ' | 'ờ' | 'ớ' | 'ợ' | 'ở' | 'ỡ' => 'o',
            'ù' | 'ú' | 'ụ' | 'ủ' | 'ũ' | 'ư' | 'ừ' | 'ứ' | 'ự' | 'ử' | 'ữ' => 'u',
            'ỳ' | 'ý' | 'ỵ' | 'ỷ' | 'ỹ' => 'y',
            'đ' => 'd',
            khac => khac,
        };
        ra.push(goc);
    }
    ra
}

/// (mẫu đã bỏ dấu, mã) — xét theo thứ tự ưu tiên §1.
const MAU_CHU_DRIVER: [(&[&str], MaSuCo); 5] = [
    (&["paper out", "out of paper", "paper empty", "load paper", "het giay"], MaSuCo::HetGiay),
    (&["jam", "ket giay"], MaSuCo::KetGiay),
    (&["offline", "not connected", "mat ket noi"], MaSuCo::Offline),
    (&["door open", "cover open", "lid open", "mo nap"], MaSuCo::MoNap),
    (&["toner low", "ink low", "low toner", "sap het muc"], MaSuCo::HetMuc),
];

/// Cờ chung chung (`loi_may_in`/`can_xu_ly` — vd job chỉ bật ERROR) → đọc thêm
/// câu trạng thái driver (`pStatus` của job) để ra mã cụ thể. Không khớp mẫu
/// nào, hoặc mã đã cụ thể, thì giữ nguyên `ma`.
///
/// VÌ SAO (giám sát 25/09): HP 4003 qua cổng WSD có thể chỉ bật ERROR khi hết
/// giấy, còn câu "Paper out" nằm ở pStatus — không đọc thì NV thấy "Máy in báo
/// lỗi" mà không biết phải nạp giấy.
///
/// R-F (giám sát vòng 2): `het_giay` cũng là cờ chung chung — PAPEROUT/
/// PAPER_PROBLEM được nhiều driver bật cả khi KẸT giấy. Chữ driver nói kẹt
/// ("jam", "kẹt giấy") mà KHÔNG nói hết giấy → `ket_giay`: NV cần gỡ giấy
/// kẹt, nạp thêm giấy không giải quyết gì.
pub fn tinh_chinh_theo_chu(ma: MaSuCo, chu: &str) -> MaSuCo {
    if chu.trim().is_empty() {
        return ma;
    }
    let chu = bo_dau_thuong(chu);
    let khop = |m: MaSuCo| MAU_CHU_DRIVER.iter().any(|(mau, x)| *x == m && mau.iter().any(|s| chu.contains(s)));
    match ma {
        MaSuCo::HetGiay if khop(MaSuCo::KetGiay) && !khop(MaSuCo::HetGiay) => MaSuCo::KetGiay,
        MaSuCo::LoiMayIn | MaSuCo::CanXuLy => MAU_CHU_DRIVER
            .iter()
            .find(|(mau, _)| mau.iter().any(|m| chu.contains(m)))
            .map_or(ma, |(_, m)| *m),
        _ => ma,
    }
}

fn mo_ta(tien_to: &str, status: u32, bang: &[(u32, MaSuCo, &str)]) -> String {
    let ten: Vec<&str> = bang.iter().filter(|(c, _, _)| status & c != 0).map(|(_, _, t)| *t).collect();
    let ten = if ten.is_empty() { "-".to_string() } else { ten.join("|") };
    // Kèm số hex đủ 32 bit: cờ không có trong bảng (POWER_SAVE, BUSY…) vẫn
    // tra lại được từ nhật ký mà không phải đoán.
    format!("{} {} (0x{:08X})", tien_to, ten, status)
}

/// `chiTiet` cho sự cố đọc từ máy in, vd "PRINTER_STATUS PAPER_OUT|ERROR (0x00000012)".
pub fn mo_ta_co_may_in(status: u32) -> String {
    mo_ta("PRINTER_STATUS", status, &BANG_MAY_IN)
}

/// `chiTiet` cho sự cố đọc từ job, vd "JOB_STATUS PAPEROUT (0x00000040)".
pub fn mo_ta_co_job(status: u32) -> String {
    mo_ta("JOB_STATUS", status, &BANG_JOB)
}

// Khoá giá trị hằng tự khai với crate `windows` lúc biên dịch (xem đầu file).
#[cfg(windows)]
const _: () = {
    use windows::Win32::Graphics::Printing as w;
    assert!(co::PRINTER_STATUS_PAUSED == w::PRINTER_STATUS_PAUSED);
    assert!(co::PRINTER_STATUS_ERROR == w::PRINTER_STATUS_ERROR);
    assert!(co::PRINTER_STATUS_PAPER_JAM == w::PRINTER_STATUS_PAPER_JAM);
    assert!(co::PRINTER_STATUS_PAPER_OUT == w::PRINTER_STATUS_PAPER_OUT);
    assert!(co::PRINTER_STATUS_PAPER_PROBLEM == w::PRINTER_STATUS_PAPER_PROBLEM);
    assert!(co::PRINTER_STATUS_OFFLINE == w::PRINTER_STATUS_OFFLINE);
    assert!(co::PRINTER_STATUS_NOT_AVAILABLE == w::PRINTER_STATUS_NOT_AVAILABLE);
    assert!(co::PRINTER_STATUS_TONER_LOW == w::PRINTER_STATUS_TONER_LOW);
    assert!(co::PRINTER_STATUS_NO_TONER == w::PRINTER_STATUS_NO_TONER);
    assert!(co::PRINTER_STATUS_USER_INTERVENTION == w::PRINTER_STATUS_USER_INTERVENTION);
    assert!(co::PRINTER_STATUS_DOOR_OPEN == w::PRINTER_STATUS_DOOR_OPEN);
    assert!(co::PRINTER_STATUS_SERVER_UNKNOWN == w::PRINTER_STATUS_SERVER_UNKNOWN);
    assert!(co::PRINTER_ATTRIBUTE_WORK_OFFLINE == w::PRINTER_ATTRIBUTE_WORK_OFFLINE);
    assert!(co::JOB_STATUS_PAUSED == w::JOB_STATUS_PAUSED);
    assert!(co::JOB_STATUS_ERROR == w::JOB_STATUS_ERROR);
    assert!(co::JOB_STATUS_DELETING == w::JOB_STATUS_DELETING);
    assert!(co::JOB_STATUS_SPOOLING == w::JOB_STATUS_SPOOLING);
    assert!(co::JOB_STATUS_DELETED == w::JOB_STATUS_DELETED);
    assert!(co::JOB_STATUS_RESTART == w::JOB_STATUS_RESTART);
    assert!(co::JOB_STATUS_PRINTING == w::JOB_STATUS_PRINTING);
    assert!(co::JOB_STATUS_OFFLINE == w::JOB_STATUS_OFFLINE);
    assert!(co::JOB_STATUS_PAPEROUT == w::JOB_STATUS_PAPEROUT);
    assert!(co::JOB_STATUS_PRINTED == w::JOB_STATUS_PRINTED);
    assert!(co::JOB_STATUS_BLOCKED_DEVQ == w::JOB_STATUS_BLOCKED_DEVQ);
    assert!(co::JOB_STATUS_USER_INTERVENTION == w::JOB_STATUS_USER_INTERVENTION);
    assert!(co::JOB_STATUS_COMPLETE == w::JOB_STATUS_COMPLETE);
    assert!(co::JOB_STATUS_RETAINED == w::JOB_STATUS_RETAINED);
};

#[cfg(test)]
mod tests {
    use super::*;
    use co::*;

    const TAT_CA: [MaSuCo; 12] = [
        MaSuCo::HetGiay, MaSuCo::KetGiay, MaSuCo::Offline, MaSuCo::MoNap, MaSuCo::HetMuc,
        MaSuCo::CanXuLy, MaSuCo::LoiMayIn, MaSuCo::KhongTimThayMayIn, MaSuCo::LoiSumatra,
        MaSuCo::LoiPdf, MaSuCo::KhongXacNhan, MaSuCo::BinhThuong,
    ];

    #[test]
    fn serde_ra_dung_chuoi_ma_cua_hop_dong() {
        for ma in TAT_CA {
            assert_eq!(serde_json::to_value(ma).unwrap(), ma.ma(), "{:?}", ma);
        }
        assert_eq!(MaSuCo::KhongTimThayMayIn.ma(), "khong_tim_thay_may_in");
    }

    #[test]
    fn muc_dung_bang_1() {
        assert_eq!(MaSuCo::HetMuc.muc(), MucDo::CanhBao);
        assert_eq!(MaSuCo::BinhThuong.muc(), MucDo::ThongTin);
        for ma in TAT_CA.into_iter().filter(|m| !matches!(m, MaSuCo::HetMuc | MaSuCo::BinhThuong)) {
            assert_eq!(ma.muc(), MucDo::Loi, "{:?}", ma);
            assert!(ma.chan_in());
        }
        assert!(!MaSuCo::HetMuc.chan_in(), "mực yếu vẫn in được — không được chặn/xoá job");
    }

    #[test]
    fn co_may_in_don_le_ra_dung_ma() {
        let bang = [
            (PRINTER_STATUS_PAPER_OUT, MaSuCo::HetGiay),
            (PRINTER_STATUS_PAPER_PROBLEM, MaSuCo::HetGiay),
            (PRINTER_STATUS_PAPER_JAM, MaSuCo::KetGiay),
            (PRINTER_STATUS_OFFLINE, MaSuCo::Offline),
            (PRINTER_STATUS_NOT_AVAILABLE, MaSuCo::Offline),
            (PRINTER_STATUS_SERVER_UNKNOWN, MaSuCo::Offline),
            (PRINTER_STATUS_DOOR_OPEN, MaSuCo::MoNap),
            (PRINTER_STATUS_NO_TONER, MaSuCo::HetMuc),
            (PRINTER_STATUS_TONER_LOW, MaSuCo::HetMuc),
            (PRINTER_STATUS_USER_INTERVENTION, MaSuCo::CanXuLy),
            (PRINTER_STATUS_ERROR, MaSuCo::LoiMayIn),
        ];
        for (c, ma) in bang {
            assert_eq!(ma_tu_co_may_in(c), ma, "co 0x{:X}", c);
        }
    }

    #[test]
    fn may_in_khong_co_su_co_la_binh_thuong() {
        assert_eq!(ma_tu_co_may_in(0), MaSuCo::BinhThuong);
        // BUSY(0x200) | PRINTING(0x400) | POWER_SAVE(0x1000000): cờ thông tin, không phải sự cố.
        assert_eq!(ma_tu_co_may_in(0x200 | 0x400 | 0x0100_0000), MaSuCo::BinhThuong);
    }

    #[test]
    fn nhieu_co_may_in_lay_theo_uu_tien() {
        assert_eq!(ma_tu_co_may_in(PRINTER_STATUS_ERROR | PRINTER_STATUS_PAPER_OUT), MaSuCo::HetGiay);
        assert_eq!(ma_tu_co_may_in(PRINTER_STATUS_PAPER_JAM | PRINTER_STATUS_OFFLINE), MaSuCo::KetGiay);
        assert_eq!(ma_tu_co_may_in(PRINTER_STATUS_OFFLINE | PRINTER_STATUS_DOOR_OPEN), MaSuCo::Offline);
        assert_eq!(ma_tu_co_may_in(PRINTER_STATUS_DOOR_OPEN | PRINTER_STATUS_USER_INTERVENTION), MaSuCo::MoNap);
        assert_eq!(ma_tu_co_may_in(PRINTER_STATUS_USER_INTERVENTION | PRINTER_STATUS_ERROR), MaSuCo::CanXuLy);
        assert_eq!(ma_tu_co_may_in(PRINTER_STATUS_ERROR | PRINTER_STATUS_TONER_LOW), MaSuCo::LoiMayIn);
    }

    #[test]
    fn co_job_ra_dung_ma_va_uu_tien() {
        assert_eq!(ma_tu_co_job(JOB_STATUS_PAPEROUT), Some(MaSuCo::HetGiay));
        assert_eq!(ma_tu_co_job(JOB_STATUS_OFFLINE), Some(MaSuCo::Offline));
        assert_eq!(ma_tu_co_job(JOB_STATUS_USER_INTERVENTION), Some(MaSuCo::CanXuLy));
        assert_eq!(ma_tu_co_job(JOB_STATUS_ERROR), Some(MaSuCo::LoiMayIn));
        assert_eq!(ma_tu_co_job(JOB_STATUS_BLOCKED_DEVQ), Some(MaSuCo::LoiMayIn));
        assert_eq!(ma_tu_co_job(JOB_STATUS_ERROR | JOB_STATUS_PAPEROUT), Some(MaSuCo::HetGiay));
        assert_eq!(ma_tu_co_job(JOB_STATUS_ERROR | JOB_STATUS_OFFLINE), Some(MaSuCo::Offline));
        // PRINTING/PRINTED/SPOOLING(0x8) không phải sự cố.
        assert_eq!(ma_tu_co_job(JOB_STATUS_PRINTING | JOB_STATUS_PRINTED | 0x8), None);
    }

    #[test]
    fn mo_ta_co_ghi_ten_va_hex() {
        assert_eq!(
            mo_ta_co_may_in(PRINTER_STATUS_PAPER_OUT | PRINTER_STATUS_ERROR),
            "PRINTER_STATUS PAPER_OUT|ERROR (0x00000012)"
        );
        assert_eq!(mo_ta_co_job(JOB_STATUS_PAPEROUT), "JOB_STATUS PAPEROUT (0x00000040)");
        assert_eq!(mo_ta_co_may_in(0x0100_0000), "PRINTER_STATUS - (0x01000000)");
    }

    #[test]
    fn moi_ma_loi_deu_co_huong_dan() {
        for ma in TAT_CA.into_iter().filter(|m| *m != MaSuCo::BinhThuong) {
            assert!(!ma.huong_dan().is_empty(), "{:?} thiếu hướng dẫn", ma);
            assert!(!ma.nhan().is_empty());
        }
    }

    /// R1 (giám sát 25/09): "nạp giấy rồi in lại" làm NV in tay trong lúc job
    /// còn chờ trong máy in → HAI tờ. Không câu hướng dẫn nào được bảo in lại.
    #[test]
    fn huong_dan_khong_bao_gio_bao_in_lai() {
        for ma in TAT_CA {
            let hd = ma.huong_dan().to_lowercase();
            assert!(!hd.contains("in lại") && !hd.contains("in lai"), "{:?}: {}", ma, hd);
        }
        assert_eq!(MaSuCo::HetGiay.huong_dan(), "Nạp giấy vào khay");
        assert_eq!(MaSuCo::KhongXacNhan.huong_dan(), "Xem khay giấy");
        assert_eq!(MaSuCo::LoiMayIn.huong_dan(), "Xem màn hình máy in, tắt/bật lại máy in");
    }

    #[test]
    fn su_co_may_in_la_su_co_vat_ly() {
        for ma in [MaSuCo::HetGiay, MaSuCo::KetGiay, MaSuCo::Offline, MaSuCo::MoNap, MaSuCo::HetMuc,
                   MaSuCo::CanXuLy, MaSuCo::LoiMayIn] {
            assert!(ma.la_su_co_may_in(), "{:?}", ma);
        }
        for ma in [MaSuCo::KhongTimThayMayIn, MaSuCo::LoiSumatra, MaSuCo::LoiPdf, MaSuCo::KhongXacNhan, MaSuCo::BinhThuong] {
            assert!(!ma.la_su_co_may_in(), "{:?}", ma);
        }
    }

    /// R10: hàng đợi Pause và "Use Printer Offline" — spooler giữ mọi job.
    #[test]
    fn may_in_tam_dung_va_dung_offline() {
        let (ma, ct) = tinh_trang_may_in(PRINTER_STATUS_PAUSED, 0);
        assert_eq!(ma, MaSuCo::CanXuLy);
        assert!(ct.as_deref().unwrap().starts_with(CHU_HANG_DOI_TAM_DUNG), "{:?}", ct);
        let (ma, ct) = tinh_trang_may_in(0, PRINTER_ATTRIBUTE_WORK_OFFLINE | 0x40 /* SHARED */);
        assert_eq!(ma, MaSuCo::Offline);
        assert!(ct.as_deref().unwrap().starts_with(CHU_DUNG_OFFLINE), "{:?}", ct);
        // ưu tiên §1: hết giấy thắng offline do Attributes; offline thắng pause
        assert_eq!(tinh_trang_may_in(PRINTER_STATUS_PAPER_OUT, PRINTER_ATTRIBUTE_WORK_OFFLINE).0, MaSuCo::HetGiay);
        assert_eq!(tinh_trang_may_in(PRINTER_STATUS_PAUSED, PRINTER_ATTRIBUTE_WORK_OFFLINE).0, MaSuCo::Offline);
        // USER_INTERVENTION thật thì không gắn câu Pause
        let (ma, ct) = tinh_trang_may_in(PRINTER_STATUS_USER_INTERVENTION | PRINTER_STATUS_PAUSED, 0);
        assert_eq!(ma, MaSuCo::CanXuLy);
        assert!(!ct.unwrap().contains(CHU_HANG_DOI_TAM_DUNG));
        assert_eq!(tinh_trang_may_in(0, 0), (MaSuCo::BinhThuong, None));
        assert_eq!(tinh_trang_may_in(PRINTER_STATUS_OFFLINE, 0).1.as_deref(), Some("PRINTER_STATUS OFFLINE (0x00000080)"));
    }

    /// R9: cờ chung chung → đọc câu driver, không phân biệt hoa thường/dấu.
    #[test]
    fn suy_ma_tu_chu_driver() {
        let bang = [
            ("Paper Out", MaSuCo::HetGiay),
            ("Error - Out of paper", MaSuCo::HetGiay),
            ("PAPER EMPTY", MaSuCo::HetGiay),
            ("Load paper in Tray 2", MaSuCo::HetGiay),
            ("Máy in hết giấy", MaSuCo::HetGiay),
            ("HET GIAY", MaSuCo::HetGiay),
            ("Paper jam", MaSuCo::KetGiay),
            ("Kẹt giấy khay 2", MaSuCo::KetGiay),
            ("ket giay", MaSuCo::KetGiay),
            ("Door open", MaSuCo::MoNap),
            ("Front cover open", MaSuCo::MoNap),
            ("lid open", MaSuCo::MoNap),
            ("Mở nắp", MaSuCo::MoNap),
            ("Toner low", MaSuCo::HetMuc),
            ("Ink Low", MaSuCo::HetMuc),
            ("Low toner", MaSuCo::HetMuc),
            ("Sắp hết mực", MaSuCo::HetMuc),
            ("Printer offline", MaSuCo::Offline),
            ("Not connected", MaSuCo::Offline),
            ("Mất kết nối", MaSuCo::Offline),
            // dấu tổ hợp (NFD): "hết giấy" viết e + U+0302 + U+0301
            ("he\u{302}\u{301}t gia\u{302}\u{301}y", MaSuCo::HetGiay),
            ("Error", MaSuCo::LoiMayIn),
            ("", MaSuCo::LoiMayIn),
        ];
        for (chu, mong) in bang {
            assert_eq!(tinh_chinh_theo_chu(MaSuCo::LoiMayIn, chu), mong, "{:?}", chu);
        }
        assert_eq!(tinh_chinh_theo_chu(MaSuCo::CanXuLy, "Paper jam"), MaSuCo::KetGiay);
        // mã đã cụ thể thì không đụng
        assert_eq!(tinh_chinh_theo_chu(MaSuCo::Offline, "Paper out"), MaSuCo::Offline);
        assert_eq!(tinh_chinh_theo_chu(MaSuCo::KetGiay, "Paper out"), MaSuCo::KetGiay);
        // nhiều mẫu cùng khớp → ưu tiên §1 (hết giấy > kẹt giấy)
        assert_eq!(tinh_chinh_theo_chu(MaSuCo::LoiMayIn, "Paper jam / paper out"), MaSuCo::HetGiay);
    }

    /// R-F: PAPEROUT/PAPER_PROBLEM là cờ chung chung — chữ driver nói kẹt thì
    /// là kẹt giấy (NV gỡ giấy kẹt, không phải nạp giấy).
    #[test]
    fn het_giay_ma_driver_noi_ket_thi_ket_giay() {
        for chu in ["Paper jam", "Paper Jam in Tray 2", "Kẹt giấy", "KET GIAY khay 2", "jam"] {
            assert_eq!(tinh_chinh_theo_chu(MaSuCo::HetGiay, chu), MaSuCo::KetGiay, "{:?}", chu);
        }
        // chữ nói hết giấy (hoặc cả hai) → giữ hết giấy; chữ khác/rỗng → giữ
        for chu in ["Paper out", "Paper jam / paper out", "Door open", "", "Error"] {
            assert_eq!(tinh_chinh_theo_chu(MaSuCo::HetGiay, chu), MaSuCo::HetGiay, "{:?}", chu);
        }
        // "mất kết nối" không bị nhận nhầm là kẹt ("ket" ≠ "ket giay")
        assert_eq!(tinh_chinh_theo_chu(MaSuCo::HetGiay, "Mất kết nối"), MaSuCo::HetGiay);
    }

    /// T4: khớp backend `laMaChoMayKhongTieuLuot` = chặn in cấp máy trừ `loi_may_in`.
    #[test]
    fn khong_tieu_luot_khop_backend() {
        for ma in TAT_CA {
            let chan_in_cap_may = ma.chan_in() && !matches!(ma, MaSuCo::LoiSumatra | MaSuCo::LoiPdf | MaSuCo::KhongXacNhan);
            assert_eq!(ma.khong_tieu_luot(), chan_in_cap_may && ma != MaSuCo::LoiMayIn, "{:?}", ma);
        }
        assert!(!MaSuCo::LoiMayIn.khong_tieu_luot(), "loi_may_in tiêu lượt — quá 5 lần là that_bai");
        assert!(MaSuCo::CanXuLy.khong_tieu_luot() && MaSuCo::KhongTimThayMayIn.khong_tieu_luot());
    }

    /// R-B: tập mã cấp máy — so từng mã, không chỉ mã ưu tiên cao nhất.
    #[test]
    fn tap_ma_may_in_va_uu_tien() {
        let nen = cac_ma_may_in(PRINTER_STATUS_PAPER_OUT | PRINTER_STATUS_ERROR, 0);
        assert!(nen.co(MaSuCo::HetGiay) && nen.co(MaSuCo::LoiMayIn) && !nen.co(MaSuCo::Offline));
        assert_eq!(nen.chan_in_uu_tien(), Some(MaSuCo::HetGiay));
        // nạp giấy, ERROR còn: không có mã MỚI
        assert_eq!(cac_ma_may_in(PRINTER_STATUS_ERROR, 0).tru(nen).chan_in_uu_tien(), None);
        // kẹt giấy mới xuất hiện
        let moi = cac_ma_may_in(PRINTER_STATUS_ERROR | PRINTER_STATUS_PAPER_JAM, 0).tru(nen);
        assert_eq!(moi.chan_in_uu_tien(), Some(MaSuCo::KetGiay));
        // mực yếu không chặn in; WORK_OFFLINE → offline
        assert_eq!(cac_ma_may_in(PRINTER_STATUS_TONER_LOW, 0).chan_in_uu_tien(), None);
        assert_eq!(cac_ma_may_in(0, PRINTER_ATTRIBUTE_WORK_OFFLINE).chan_in_uu_tien(), Some(MaSuCo::Offline));
        assert!(cac_ma_may_in(0, 0).is_empty());
        assert_eq!([MaSuCo::KhongTimThayMayIn].into_iter().collect::<TapMa>().chan_in_uu_tien(), Some(MaSuCo::KhongTimThayMayIn));
        assert_eq!(uu_tien_hon(MaSuCo::LoiMayIn, MaSuCo::HetGiay), MaSuCo::HetGiay);
        assert_eq!(uu_tien_hon(MaSuCo::BinhThuong, MaSuCo::HetMuc), MaSuCo::HetMuc);
        assert_eq!(uu_tien_hon(MaSuCo::Offline, MaSuCo::BinhThuong), MaSuCo::Offline);
    }
}
