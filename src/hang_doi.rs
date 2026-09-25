// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hàng đợi in do server giữ + huỷ / bỏ theo dõi từ app (hợp đồng
//! `HOP-DONG-HANG-DOI-HUY-v5.md` §8, 25/09).
//!
//! VÌ SAO CÓ: HCM 25/09 — mười hoá đơn gửi lúc máy in hết giấy nằm im trên
//! server, app không hiện gì, nạp giấy là in ra cả loạt, không ai huỷ được.
//! Chủ yêu cầu: thấy hàng đợi ở CẢ app lẫn ZaloCRM, huỷ được, và huỷ phải báo
//! kết quả THẬT.
//!
//! LUẬT CỦA FILE NÀY (v5.1):
//! - App KHÔNG tự huỷ gì. Huỷ = hỏi server (`yeu-cau-huy`, có ack); server chỉ
//!   huỷ được hoá đơn còn `cho_in` (chưa rời server) — chắc chắn không in.
//! - "Đã huỷ" (xanh) CHỈ khi ack `ok:true` CÓ `id` ĐÚNG hoá đơn đã hỏi (ack trùng
//!   id của rust_socketio — xem `net.rs` `CongSocket`). Mọi đường khác nói đúng
//!   điều biết được: KHÔNG huỷ được (kèm lý do server) / CHƯA huỷ (yêu cầu chưa
//!   rời máy) / CHƯA RÕ (đã gửi mà không có trả lời).
//! - "Bỏ khỏi hàng đợi" (`bo_qua`) không bao giờ hiện thành "Đã huỷ".
//! - Hết giờ chờ ack thì HỎI LẠI (huỷ lặp là an toàn: server trả `da_huy_truoc`)
//!   — phần lớn "chưa rõ" thành câu trả lời chắc chắn.
//! - Nút xác nhận KHÔNG ăn cú bấm trong `CHONG_BAM_DUP` sau khi hộp mở: hộp mở
//!   làm danh sách tự cuộn, "Huỷ lệnh in" rơi đúng chỗ nút "Huỷ" vừa bấm — cú
//!   bấm thứ hai của một lần bấm đúp sẽ huỷ luôn mà người dùng chưa đọc câu hỏi
//!   (giám sát 0.2.6, đo trên ảnh chụp).
//!
//! Logic THUẦN (không socket, không Slint, thời gian truyền vào) — test được
//! mọi nhánh; `net.rs` bơm ảnh chụp vào, `ui.rs` gọi các hàm `bam_*`.

use crate::bao_cao::{self, HangDoiServer, MucHangDoi};
use crate::hop_thu_di::{CanHoTro, DuongGui, LoiGuiAck};
use crate::su_co::MaSuCo;
use std::time::{Duration, Instant};

/// "Đã huỷ" hiện bao lâu rồi dòng rời danh sách (hợp đồng §6.2).
pub const GIU_DA_HUY: Duration = Duration::from_secs(5);
/// "Đã bỏ khỏi hàng đợi" dài hơn — câu dài, có việc cần làm.
pub const GIU_DA_BO: Duration = Duration::from_secs(8);
/// Tổng kết "Huỷ cả N" (toàn bộ thành công) hiện bao lâu.
pub const GIU_TONG_KET: Duration = Duration::from_secs(6);
/// App hỏi lại ảnh chụp theo nhịp này (server chỉ đẩy khi đổi).
pub const NHIP_LAM_MOI: Duration = Duration::from_secs(30);
/// Đã nối mà quá lâu không có ảnh chụp mới → ghi "có thể chưa cập nhật".
pub const CU_SAU: Duration = Duration::from_secs(90);
/// Chờ ack một yêu cầu (hợp đồng §8.7: 20 s).
pub const CHO_ACK: Duration = Duration::from_secs(20);
/// Hỏi lại tối đa trong chừng này kể từ lần bấm.
pub const HAN_HOI_LAI: Duration = Duration::from_secs(60);
/// Tối đa số lần yêu cầu THỰC SỰ rời máy.
pub const SO_LAN_GUI_TOI_DA: u32 = 3;
/// Nghỉ giữa hai lần hỏi.
pub const NGHI_HOI_LAI: Duration = Duration::from_secs(3);
/// Nút xác nhận bỏ qua cú bấm tới sớm hơn chừng này sau khi hộp mở (bấm đúp).
pub const CHONG_BAM_DUP: Duration = Duration::from_millis(500);

/// Câu giải thích "Vì sao không huỷ được?" — cùng lời với server (§8.2).
pub const VI_SAO_DANG_IN: &str =
    "Hoá đơn đang được gửi/in ở máy in — không huỷ được nữa. Nếu không cần tờ này: bỏ tờ in ra.";
pub const VI_SAO_CHUA_XAC_NHAN: &str = "Hoá đơn đã gửi xuống máy in nhưng chưa xác nhận đã in — có thể đang nằm \
     trong bộ nhớ máy in, không huỷ được từ xa. Muốn bỏ hẳn: xoá lệnh trong hàng đợi Windows (nếu còn), tắt máy in \
     10 giây (MỌI hoá đơn trong bộ nhớ máy sẽ mất) → bật lại → kiểm khay → in lại cái cần. Rồi bấm \"Bỏ khỏi hàng đợi\".";

/// Câu hỏi xác nhận huỷ — NÊU SỐ HOÁ ĐƠN (bấm nhầm dòng thì đọc là biết).
pub fn chu_xac_nhan_huy(so_hoa_don: &str) -> String {
    format!("Huỷ lệnh in {}? Hoá đơn này sẽ KHÔNG được in.", so_hoa_don)
}

/// Câu hỏi xác nhận bỏ theo dõi.
pub fn chu_xac_nhan_bo(so_hoa_don: &str) -> String {
    format!(
        "Bỏ {} khỏi hàng đợi? Việc này KHÔNG chặn việc in — hoá đơn còn trong máy in vẫn sẽ in ra. \
         Hệ thống chỉ thôi theo dõi.",
        so_hoa_don
    )
}

/// Việc người dùng yêu cầu trên một hoá đơn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaiViec {
    /// `yeu-cau-huy` — `cho_in → da_huy`.
    Huy,
    /// `yeu-cau-bo-theo-doi` — `khong_ro → bo_qua`.
    BoTheoDoi,
}

impl LoaiViec {
    pub fn su_kien(self) -> &'static str {
        match self {
            LoaiViec::Huy => "yeu-cau-huy",
            LoaiViec::BoTheoDoi => "yeu-cau-bo-theo-doi",
        }
    }
}

/// Kết cục một yêu cầu — mỗi nhánh là một điều app BIẾT CHẮC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KetCuc {
    /// Server ack `ok:true` cho ĐÚNG id đã hỏi.
    Duoc { cach: String, noi_dung: String },
    /// Server ack `ok:false` — không làm được, kèm lý do.
    KhongDuoc { loi: String, noi_dung: String },
    /// Không lần nào rời máy (mất kết nối / server chưa hỗ trợ) — CHẮC CHẮN chưa làm.
    ChuaGui { ly_do: String },
    /// Đã gửi mà không có câu trả lời đọc được — server CÓ THỂ đã làm.
    ChuaRo { ly_do: String },
}

impl KetCuc {
    /// Dòng nhật ký `huy_ket_qua` / `bo_theo_doi` — bắt đầu `ok=true`/`ok=false`
    /// (hợp đồng §8.9: backend phân mức theo đầu dòng).
    pub fn dong_nhat_ky(&self, muc: &MucHangDoi) -> String {
        let dau = format!("so={} id={}", muc.so_hoa_don, muc.id);
        match self {
            KetCuc::Duoc { cach, .. } => format!("ok=true cach={} {}", if cach.is_empty() { "-" } else { cach }, dau),
            KetCuc::KhongDuoc { loi, noi_dung } => format!("ok=false loi={} {} noi_dung={}", loi, dau, noi_dung),
            KetCuc::ChuaGui { ly_do } => format!("ok=false chua_gui ly_do={} {}", ly_do, dau),
            KetCuc::ChuaRo { ly_do } => format!("ok=false chua_ro ly_do={} {}", ly_do, dau),
        }
    }
}

/// Trạng thái thao tác của MỘT hoá đơn trên giao diện.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ThaoTac {
    /// `luc` = lúc hộp mở (chống bấm đúp).
    XacNhanHuy { luc: Instant },
    XacNhanBo { luc: Instant },
    ViSao,
    /// `lan` = lần hỏi thứ mấy; `da_gui` = số lần yêu cầu đã THỰC SỰ rời máy.
    Dang { loai: LoaiViec, lan: u32, da_gui: u32 },
    Xong { loai: LoaiViec, luc: Instant, noi_dung: String },
    /// Không làm được / chưa gửi được — đỏ, giữ tới khi bấm ×.
    ThatBai { loai: LoaiViec, noi_dung: String },
    /// Đã gửi mà không có trả lời — vàng, giữ tới khi bấm ×. Câu chữ dựng lúc vẽ
    /// theo việc hoá đơn CÒN hay ĐÃ RỜI hàng đợi.
    ChuaRo { loai: LoaiViec },
}

impl ThaoTac {
    fn la_hop_thoai(&self) -> bool {
        matches!(self, ThaoTac::XacNhanHuy { .. } | ThaoTac::XacNhanBo { .. } | ThaoTac::ViSao)
    }

    /// Việc đang chạy / kết quả vừa xong — không mở hộp đè lên.
    fn dang_ban(&self) -> bool {
        matches!(self, ThaoTac::Dang { .. } | ThaoTac::Xong { .. })
    }
}

#[derive(Debug, Clone)]
struct MucThaoTac {
    /// Bản sao mục lúc bấm — dòng kết quả vẫn hiện khi server đã bỏ mục khỏi ảnh chụp.
    muc: MucHangDoi,
    /// Vị trí dòng lúc bấm — dòng đã rời ảnh chụp hiện lại ĐÚNG chỗ cũ, không nhảy.
    vi_tri: usize,
    tt: ThaoTac,
}

/// "Huỷ cả N lệnh đang tạm giữ".
#[derive(Debug, Clone, PartialEq, Eq)]
enum Loat {
    /// Đang hỏi xác nhận — danh sách chốt LÚC MỞ hộp xác nhận (mục mới tới sau
    /// không bị huỷ theo). `luc` = lúc mở (chống bấm đúp).
    XacNhan { ids: Vec<String>, luc: Instant },
    /// CHỈ đếm kết quả của `ids` — một lệnh huỷ lẻ chạy song song không được
    /// làm tổng kết nói "đã huỷ hết" (giám sát 0.2.6).
    Dang { ids: Vec<String>, xong: usize, duoc: usize },
    Xong { tong: usize, duoc: usize, luc: Instant },
}

/// Hàng đợi phía app — nằm trong `TrangThaiChung`.
#[derive(Debug, Clone, Default)]
pub struct HangDoiApp {
    /// Kết nối (gần nhất) báo `hoTro` có `hang_doi`.
    ho_tro: bool,
    anh: Option<HangDoiServer>,
    luc_nhan: Option<Instant>,
    /// Ảnh chụp nhận ở kết nối HIỆN TẠI (nối lại thì chờ ảnh mới mới cho bấm).
    cua_ket_noi_nay: bool,
    thao_tac: Vec<MucThaoTac>,
    loat: Option<Loat>,
}

/// Màu một dòng.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MauDong {
    /// `cho_in` chưa bị giữ — chờ lượt gửi.
    Cho,
    /// `cho_in` bị cầu dao giữ (máy in lỗi).
    TamGiu,
    /// `dang_gui` / `da_gui`.
    DangIn,
    /// `khong_ro`.
    ChuaXacNhan,
}

/// Dòng đang ở chế độ nào (UI vẽ theo đây).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheDo {
    BinhThuong,
    XacNhanHuy,
    XacNhanBo,
    ViSao,
    Dang,
    DaHuy,
    DaBo,
    ThatBai,
    ChuaRo,
}

/// Nút chính của dòng ở chế độ bình thường.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NutDong {
    Huy,
    ViSao,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DongHangDoi {
    pub id: String,
    /// "INV/2026/030110 · Anh Dev"
    pub tieu_de: String,
    /// Giờ vào hàng đợi (giờ máy).
    pub gio: String,
    /// Dòng 2 ở chế độ bình thường: `lyDo` của server.
    pub trang_thai: String,
    pub mau: MauDong,
    pub che_do: CheDo,
    pub nut: NutDong,
    /// Được bấm (đã nối + ảnh chụp của kết nối này).
    pub bat_nut: bool,
    /// Chữ của chế độ: câu xác nhận / kết quả / giải thích.
    pub thong_diep: String,
    /// Trong "Vì sao?": có nút "Bỏ khỏi hàng đợi".
    pub co_nut_bo: bool,
    /// Dòng báo lỗi / chưa rõ của lệnh HUỶ mà hoá đơn VẪN còn chờ huỷ được: nút "Huỷ lại".
    pub co_nut_huy_lai: bool,
}

/// Khối "HÀNG ĐỢI" dựng cho giao diện.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KhoiHangDoi {
    pub hien: bool,
    pub tieu_de: String,
    /// "có thể chưa cập nhật — app đang mất kết nối" …
    pub ghi_chu: String,
    /// Dải cam khi có lệnh tạm giữ (rỗng = không có).
    pub dai_tam_giu: String,
    /// Nút "Huỷ cả N" trên dải (0 = không hiện).
    pub so_huy_ca: usize,
    /// Chế độ của dải: 0 thường · 1 hỏi xác nhận · 2 đang huỷ · 3 xong hết · 4 xong có lỗi.
    pub loat_che_do: u8,
    pub loat_chu: String,
    /// Chữ nút xác nhận loạt: "Huỷ 10 lệnh in".
    pub loat_nut: String,
    pub dong: Vec<DongHangDoi>,
}

fn nhan_trang_thai(muc: &MucHangDoi) -> String {
    if !muc.ly_do.is_empty() {
        return muc.ly_do.clone();
    }
    match muc.trang_thai.as_str() {
        "cho_in" if muc.tam_giu => "Tạm giữ — chờ máy in hết lỗi".into(),
        "cho_in" => "Chờ gửi xuống máy in".into(),
        "dang_gui" => "Đang gửi xuống máy in".into(),
        "da_gui" => "Đã gửi — đang chờ máy in".into(),
        "khong_ro" => "Chưa xác nhận đã in — có thể đang nằm trong máy in".into(),
        khac => khac.to_string(),
    }
}

fn mau_cua(muc: &MucHangDoi) -> MauDong {
    match muc.trang_thai.as_str() {
        "cho_in" if muc.tam_giu => MauDong::TamGiu,
        "cho_in" => MauDong::Cho,
        "khong_ro" => MauDong::ChuaXacNhan,
        _ => MauDong::DangIn,
    }
}

fn huy_duoc(muc: &MucHangDoi) -> bool {
    muc.huy == "chac_chan" && muc.trang_thai == "cho_in"
}

fn la_chua_xac_nhan(muc: &MucHangDoi) -> bool {
    muc.nhom == "chua_xac_nhan" || muc.trang_thai == "khong_ro"
}

fn vi_sao(muc: &MucHangDoi) -> &'static str {
    if la_chua_xac_nhan(muc) {
        VI_SAO_CHUA_XAC_NHAN
    } else {
        VI_SAO_DANG_IN
    }
}

/// Câu dải cam (hợp đồng §8.10). `ma_may_in` = trạng thái máy in app đọc gần nhất.
pub fn chu_dai_tam_giu(so: usize, ma_may_in: Option<MaSuCo>) -> String {
    let (ly_do, viec, viec_thuong): (String, &str, &str) = match ma_may_in {
        Some(MaSuCo::HetGiay) => ("máy in Hết giấy".into(), "Nạp giấy", "nạp giấy"),
        Some(MaSuCo::KetGiay) => ("máy in Kẹt giấy".into(), "Gỡ giấy kẹt", "gỡ giấy kẹt"),
        Some(MaSuCo::MoNap) => ("nắp máy in đang mở".into(), "Đóng nắp", "đóng nắp"),
        Some(MaSuCo::Offline) => ("máy in mất kết nối".into(), "Bật lại máy in", "bật lại máy in"),
        Some(MaSuCo::KhongTimThayMayIn) => {
            ("không tìm thấy máy in trong Windows".into(), "Chọn lại máy in trong app", "chọn lại máy in")
        }
        Some(m) if m.chan_in() => (format!("máy in báo: {}", m.nhan()), "Xử lý xong máy in", "xử lý máy in"),
        // Máy vừa báo bình thường mà server còn giữ: cầu dao sắp đóng.
        Some(MaSuCo::BinhThuong) => {
            return format!(
                "{} hoá đơn đang chờ — máy in vừa hết lỗi, hệ thống sắp tự in. Hoá đơn nào không cần nữa: bấm Huỷ NGAY.",
                so
            )
        }
        // App chưa đọc được máy in (hoặc chỉ là cảnh báo mực): không đoán lý do.
        _ => {
            return format!(
                "{} hoá đơn đang được giữ lại vì máy in đang lỗi. Xử lý xong máy in là tự in. Hoá đơn nào không \
                 cần nữa: bấm Huỷ trước.",
                so
            )
        }
    };
    format!(
        "{} hoá đơn đang chờ — {}. {} là tự in. Hoá đơn nào không cần nữa: bấm Huỷ TRƯỚC khi {}.",
        so, ly_do, viec, viec_thuong
    )
}

impl HangDoiApp {
    /// Kết nối (gần nhất) có hàng đợi server.
    pub fn ho_tro(&self) -> bool {
        self.ho_tro
    }

    /// `cau-hinh` của kết nối hiện tại. Backend không hỗ trợ → ẩn khối (giữ các
    /// yêu cầu đang bay để luồng của chúng ghi được kết cục).
    pub fn nhan_cau_hinh(&mut self, ho_tro: bool) {
        self.ho_tro = ho_tro;
        if !ho_tro {
            self.anh = None;
            self.luc_nhan = None;
            self.cua_ket_noi_nay = false;
            self.thao_tac.retain(|m| matches!(m.tt, ThaoTac::Dang { .. }));
            self.loat = None;
        }
    }

    /// Kết nối mới mở: ảnh chụp cũ chỉ để xem, chờ ảnh của kết nối này mới cho bấm.
    pub fn ket_noi_moi(&mut self) {
        self.cua_ket_noi_nay = false;
        self.dong_hop_thoai();
    }

    fn dong_hop_thoai(&mut self) {
        self.thao_tac.retain(|m| !m.tt.la_hop_thoai());
        if matches!(self.loat, Some(Loat::XacNhan { .. })) {
            self.loat = None;
        }
    }

    fn tim_trong_anh(&self, id: &str) -> Option<(usize, &MucHangDoi)> {
        let anh = self.anh.as_ref()?;
        anh.cho_in.iter().chain(anh.chua_xac_nhan.iter()).enumerate().find(|(_, m)| m.id == id)
    }

    fn vi_tri_thao_tac(&self, id: &str) -> Option<usize> {
        self.thao_tac.iter().position(|m| m.muc.id == id)
    }

    /// Mục `print_job_id` có trong ảnh chụp TƯƠI (của kết nối này) không. `None`
    /// = chưa có ảnh tươi — không kết luận được gì.
    pub fn co_trong_anh(&self, print_job_id: &str) -> Option<bool> {
        (self.ho_tro && self.cua_ket_noi_nay && self.anh.is_some()).then(|| self.tim_trong_anh(print_job_id).is_some())
    }

    /// Yêu cầu của `id` vẫn còn chờ gửi (chưa bị Lưu cấu hình / kết nối mới xoá).
    pub fn con_dang(&self, id: &str) -> bool {
        self.thao_tac.iter().any(|m| m.muc.id == id && matches!(m.tt, ThaoTac::Dang { .. }))
    }

    /// Ảnh chụp mới từ server. Trả câu tóm tắt khi SỐ LƯỢNG đổi (để ghi nhật ký
    /// — không ghi một dòng mỗi 30 s).
    pub fn nhan_anh(&mut self, anh: HangDoiServer, bay_gio: Instant) -> Option<String> {
        let dem = |a: &HangDoiServer| (a.cho_in.len(), a.cho_in.iter().filter(|m| m.tam_giu).count(), a.chua_xac_nhan.len());
        let truoc = self.anh.as_ref().map(dem);
        let sau = dem(&anh);
        self.anh = Some(anh);
        self.luc_nhan = Some(bay_gio);
        self.cua_ket_noi_nay = true;
        // Hộp xác nhận / giải thích của mục không còn áp dụng thì đóng: vd đang
        // hỏi "Huỷ lệnh in này?" mà server vừa gửi hoá đơn xuống máy — câu
        // "sẽ KHÔNG được in" thành sai.
        let con_ap_dung: Vec<bool> = self
            .thao_tac
            .iter()
            .map(|m| match m.tt {
                ThaoTac::XacNhanHuy { .. } => self.tim_trong_anh(&m.muc.id).is_some_and(|(_, x)| huy_duoc(x)),
                ThaoTac::XacNhanBo { .. } => self.tim_trong_anh(&m.muc.id).is_some_and(|(_, x)| la_chua_xac_nhan(x)),
                ThaoTac::ViSao => self.tim_trong_anh(&m.muc.id).is_some_and(|(_, x)| !huy_duoc(x)),
                _ => true,
            })
            .collect();
        let mut i = 0;
        self.thao_tac.retain(|_| {
            i += 1;
            con_ap_dung[i - 1]
        });
        if let Some(Loat::XacNhan { ids, .. }) = &mut self.loat {
            let anh = self.anh.as_ref().expect("vừa gán");
            ids.retain(|id| anh.cho_in.iter().any(|m| &m.id == id && huy_duoc(m)));
            if ids.is_empty() {
                self.loat = None;
            }
        }
        (truoc != Some(sau)).then(|| format!("cho_in={} tam_giu={} chua_xac_nhan={}", sau.0, sau.1, sau.2))
    }

    fn co_the_bam(&self, da_noi: bool) -> bool {
        da_noi && self.ho_tro && self.cua_ket_noi_nay && self.anh.is_some()
    }

    fn mo(&mut self, id: &str, tt: ThaoTac) -> bool {
        let Some((vi_tri, muc)) = self.tim_trong_anh(id).map(|(i, m)| (i, m.clone())) else { return false };
        // Chỉ MỘT hộp thoại mở một lúc; dòng đang có việc / kết quả vừa xong thì
        // không mở đè. Dòng báo lỗi / chưa rõ thì ĐƯỢC mở lại ("Huỷ lại").
        if let Some(i) = self.vi_tri_thao_tac(id) {
            if self.thao_tac[i].tt.dang_ban() {
                return false;
            }
            self.thao_tac.remove(i);
        }
        self.dong_hop_thoai();
        self.thao_tac.push(MucThaoTac { muc, vi_tri, tt });
        true
    }

    /// Bấm "Huỷ" (hoặc "Huỷ lại") trên dòng → hỏi xác nhận tại chỗ.
    pub fn bam_huy(&mut self, id: &str, da_noi: bool, bay_gio: Instant) -> bool {
        if !self.co_the_bam(da_noi) || !self.tim_trong_anh(id).is_some_and(|(_, m)| huy_duoc(m)) {
            return false;
        }
        self.mo(id, ThaoTac::XacNhanHuy { luc: bay_gio })
    }

    /// Bấm "Vì sao?" (lệnh không huỷ được) → mở giải thích. Xem được cả khi mất kết nối.
    pub fn bam_vi_sao(&mut self, id: &str) -> bool {
        if self.tim_trong_anh(id).is_none_or(|(_, m)| huy_duoc(m)) {
            return false;
        }
        self.mo(id, ThaoTac::ViSao)
    }

    /// Bấm "Bỏ khỏi hàng đợi" trong phần giải thích → hỏi xác nhận.
    pub fn bam_bo(&mut self, id: &str, da_noi: bool, bay_gio: Instant) -> bool {
        if !self.co_the_bam(da_noi) || !self.tim_trong_anh(id).is_some_and(|(_, m)| la_chua_xac_nhan(m)) {
            return false;
        }
        self.mo(id, ThaoTac::XacNhanBo { luc: bay_gio })
    }

    /// "Giữ lại" / "Đóng" (hộp thoại) hoặc "×" (kết quả lỗi / chưa rõ).
    pub fn dong(&mut self, id: &str) {
        self.thao_tac.retain(|m| m.muc.id != id || m.tt.dang_ban());
    }

    /// Bấm nút xác nhận ("Huỷ lệnh in" / "Bỏ khỏi hàng đợi") → chuyển "Đang…".
    /// Trả mục để luồng gửi yêu cầu; `None` = không hợp lệ (bấm đúp, hộp vừa mở
    /// chưa đủ `CHONG_BAM_DUP`, hộp đã đóng, mất kết nối) — KHÔNG gửi gì.
    pub fn bat_dau(&mut self, id: &str, loai: LoaiViec, da_noi: bool, bay_gio: Instant) -> Option<MucHangDoi> {
        if !self.co_the_bam(da_noi) {
            return None;
        }
        let i = self.vi_tri_thao_tac(id)?;
        let luc_mo = match (loai, &self.thao_tac[i].tt) {
            (LoaiViec::Huy, ThaoTac::XacNhanHuy { luc }) | (LoaiViec::BoTheoDoi, ThaoTac::XacNhanBo { luc }) => *luc,
            _ => return None,
        };
        if bay_gio.saturating_duration_since(luc_mo) < CHONG_BAM_DUP {
            return None;
        }
        self.thao_tac[i].tt = ThaoTac::Dang { loai, lan: 1, da_gui: 0 };
        Some(self.thao_tac[i].muc.clone())
    }

    /// Luồng gửi báo đang ở lần hỏi thứ `lan`, đã có `da_gui` lần rời máy.
    pub fn bao_lan(&mut self, id: &str, lan: u32, da_gui: u32) {
        if let Some(m) = self.thao_tac.iter_mut().find(|m| m.muc.id == id) {
            if let ThaoTac::Dang { lan: l, da_gui: g, .. } = &mut m.tt {
                *l = lan;
                *g = da_gui;
            }
        }
    }

    /// Kết cục của một yêu cầu. Mục không còn (đã Lưu cấu hình khác) → bỏ qua.
    pub fn ket_thuc(&mut self, id: &str, loai: LoaiViec, kc: &KetCuc, bay_gio: Instant) {
        let Some(m) = self.thao_tac.iter_mut().find(|m| m.muc.id == id) else { return };
        if !matches!(m.tt, ThaoTac::Dang { .. }) {
            return;
        }
        m.tt = match (loai, kc) {
            (LoaiViec::Huy, KetCuc::Duoc { noi_dung, .. }) => ThaoTac::Xong {
                loai,
                luc: bay_gio,
                noi_dung: if noi_dung.is_empty() { "Đã huỷ — hoá đơn chắc chắn không in".into() } else { noi_dung.clone() },
            },
            (LoaiViec::BoTheoDoi, KetCuc::Duoc { .. }) => ThaoTac::Xong {
                loai,
                luc: bay_gio,
                // KHÔNG BAO GIỜ "Đã huỷ" (§8.5) — câu cố định, không lấy chữ server.
                noi_dung: "Đã bỏ khỏi hàng đợi — hệ thống KHÔNG biết hoá đơn đã in hay chưa. Kiểm giấy trước khi in lại."
                    .into(),
            },
            (LoaiViec::Huy, KetCuc::KhongDuoc { noi_dung, .. }) => ThaoTac::ThatBai {
                loai,
                noi_dung: format!("Không huỷ được: {}", if noi_dung.is_empty() { "server từ chối" } else { noi_dung }),
            },
            (LoaiViec::BoTheoDoi, KetCuc::KhongDuoc { noi_dung, .. }) => ThaoTac::ThatBai {
                loai,
                noi_dung: format!("Không bỏ được: {}", if noi_dung.is_empty() { "server từ chối" } else { noi_dung }),
            },
            (_, KetCuc::ChuaGui { ly_do }) => {
                let viec = if loai == LoaiViec::Huy { "huỷ" } else { "bỏ" };
                let vi_sao = if ly_do.contains("ho tro") {
                    "server ZaloCRM chưa hỗ trợ việc này từ app — làm trên trang ZaloCRM › Máy in".to_string()
                } else {
                    format!("không gửi được yêu cầu (app mất kết nối với ZaloCRM). Nối lại rồi bấm {} lại", viec)
                };
                ThaoTac::ThatBai { loai, noi_dung: format!("CHƯA {} — {}.", viec, vi_sao) }
            }
            (_, KetCuc::ChuaRo { .. }) => ThaoTac::ChuaRo { loai },
        };
        // Tiến độ "Huỷ cả N" — CHỈ id của loạt.
        if loai == LoaiViec::Huy {
            if let Some(Loat::Dang { ids, xong, duoc }) = &mut self.loat {
                if ids.iter().any(|x| x == id) {
                    *xong += 1;
                    if matches!(kc, KetCuc::Duoc { .. }) {
                        *duoc += 1;
                    }
                    if *xong >= ids.len() {
                        self.loat = Some(Loat::Xong { tong: ids.len(), duoc: *duoc, luc: bay_gio });
                    }
                }
            }
        }
    }

    /// Bấm "Huỷ cả N" trên dải cam → hỏi xác nhận; chốt danh sách NGAY LÚC NÀY.
    pub fn bam_huy_ca(&mut self, da_noi: bool, bay_gio: Instant) -> bool {
        if !self.co_the_bam(da_noi) || matches!(self.loat, Some(Loat::Dang { .. })) {
            return false;
        }
        let ids = self.ids_huy_ca();
        if ids.is_empty() {
            return false;
        }
        self.dong_hop_thoai();
        self.loat = Some(Loat::XacNhan { ids, luc: bay_gio });
        true
    }

    /// Mục tạm giữ huỷ được và chưa có việc gì đang chạy / kết quả vừa xong.
    fn ids_huy_ca(&self) -> Vec<String> {
        let Some(anh) = &self.anh else { return Vec::new() };
        anh.cho_in
            .iter()
            .filter(|m| m.tam_giu && huy_duoc(m))
            .filter(|m| self.vi_tri_thao_tac(&m.id).is_none_or(|i| !self.thao_tac[i].tt.dang_ban()))
            .map(|m| m.id.clone())
            .collect()
    }

    /// "Giữ lại" / "Đóng" trên dải.
    pub fn dong_loat(&mut self) {
        if !matches!(self.loat, Some(Loat::Dang { .. })) {
            self.loat = None;
        }
    }

    /// Bấm "Huỷ N lệnh in" (xác nhận loạt) → mọi dòng thành "Đang huỷ…". Trả các
    /// mục để một luồng gửi TUẦN TỰ.
    pub fn bat_dau_loat(&mut self, da_noi: bool, bay_gio: Instant) -> Vec<MucHangDoi> {
        if !self.co_the_bam(da_noi) {
            return Vec::new();
        }
        let Some(Loat::XacNhan { ids, luc }) = self.loat.clone() else { return Vec::new() };
        if bay_gio.saturating_duration_since(luc) < CHONG_BAM_DUP {
            return Vec::new();
        }
        self.dong_hop_thoai();
        let mut ra = Vec::new();
        for id in ids {
            let Some((vi_tri, muc)) = self.tim_trong_anh(&id).map(|(i, m)| (i, m.clone())) else { continue };
            if !huy_duoc(&muc) {
                continue;
            }
            match self.vi_tri_thao_tac(&id) {
                Some(i) if self.thao_tac[i].tt.dang_ban() => continue,
                Some(i) => {
                    self.thao_tac.remove(i);
                }
                None => {}
            }
            self.thao_tac.push(MucThaoTac {
                muc: muc.clone(),
                vi_tri,
                tt: ThaoTac::Dang { loai: LoaiViec::Huy, lan: 1, da_gui: 0 },
            });
            ra.push(muc);
        }
        self.loat = (!ra.is_empty()).then(|| Loat::Dang { ids: ra.iter().map(|m| m.id.clone()).collect(), xong: 0, duoc: 0 });
        ra
    }

    /// Xoá kết quả "đã xong" quá hạn (gọi mỗi nhịp giao diện).
    pub fn don_dep(&mut self, bay_gio: Instant) {
        self.thao_tac.retain(|m| match m.tt {
            ThaoTac::Xong { loai, luc, .. } => {
                let giu = if loai == LoaiViec::Huy { GIU_DA_HUY } else { GIU_DA_BO };
                bay_gio.saturating_duration_since(luc) < giu
            }
            _ => true,
        });
        if let Some(Loat::Xong { tong, duoc, luc }) = self.loat {
            if duoc == tong && bay_gio.saturating_duration_since(luc) >= GIU_TONG_KET {
                self.loat = None;
            }
        }
    }

    /// Dựng khối cho giao diện. `ma_may_in` = trạng thái máy in app đọc gần nhất
    /// (cho câu dải cam), `gio` = đổi ISO → giờ máy.
    pub fn khoi(&self, da_noi: bool, ma_may_in: Option<MaSuCo>, bay_gio: Instant, gio: &dyn Fn(&str) -> String) -> KhoiHangDoi {
        let Some(anh) = self.anh.as_ref().filter(|_| self.ho_tro) else { return KhoiHangDoi::default() };
        let bat = self.co_the_bam(da_noi);
        let mut dong: Vec<DongHangDoi> = Vec::new();
        let dong_cua = |muc: &MucHangDoi, tt: Option<&ThaoTac>, con_trong_anh: bool| -> DongHangDoi {
            let tieu_de = match &muc.ten_khach {
                Some(k) => format!("{} · {}", muc.so_hoa_don, k),
                None => muc.so_hoa_don.clone(),
            };
            let nut = if huy_duoc(muc) { NutDong::Huy } else { NutDong::ViSao };
            let (che_do, thong_diep) = match tt {
                None => (CheDo::BinhThuong, String::new()),
                // Hộp xác nhận khi mất kết nối: về bình thường (nút đã khoá).
                Some(ThaoTac::XacNhanHuy { .. }) if bat => (CheDo::XacNhanHuy, chu_xac_nhan_huy(&muc.so_hoa_don)),
                Some(ThaoTac::XacNhanBo { .. }) if bat => (CheDo::XacNhanBo, chu_xac_nhan_bo(&muc.so_hoa_don)),
                Some(ThaoTac::XacNhanHuy { .. } | ThaoTac::XacNhanBo { .. }) => (CheDo::BinhThuong, String::new()),
                Some(ThaoTac::ViSao) => (CheDo::ViSao, vi_sao(muc).to_string()),
                Some(ThaoTac::Dang { loai, lan, da_gui }) => {
                    let chu = if *loai == LoaiViec::Huy { "Đang huỷ…" } else { "Đang bỏ khỏi hàng đợi…" };
                    let chu = if *lan <= 1 {
                        chu.to_string()
                    } else if *da_gui == 0 {
                        format!("{} (chờ nối lại ZaloCRM để gửi yêu cầu)", chu)
                    } else {
                        format!("{} (ZaloCRM chưa trả lời — hỏi lại, lần {}/{})", chu, da_gui + 1, SO_LAN_GUI_TOI_DA)
                    };
                    (CheDo::Dang, chu)
                }
                Some(ThaoTac::Xong { loai: LoaiViec::Huy, noi_dung, .. }) => (CheDo::DaHuy, noi_dung.clone()),
                Some(ThaoTac::Xong { noi_dung, .. }) => (CheDo::DaBo, noi_dung.clone()),
                Some(ThaoTac::ThatBai { noi_dung, .. }) => (CheDo::ThatBai, noi_dung.clone()),
                // Chưa rõ: nói đúng điều ảnh chụp mới nhất cho biết về hoá đơn.
                Some(ThaoTac::ChuaRo { loai }) => {
                    let viec = if *loai == LoaiViec::Huy { "huỷ" } else { "bỏ" };
                    let chu = if con_trong_anh && *loai == LoaiViec::Huy && huy_duoc(muc) {
                        "Chưa rõ đã huỷ được chưa — ZaloCRM không trả lời. Hoá đơn VẪN đang chờ trong hàng đợi: \
                         bấm \"Huỷ lại\" để hỏi lại."
                            .to_string()
                    } else if con_trong_anh {
                        format!("Chưa rõ đã {} được chưa — ZaloCRM không trả lời. Hoá đơn vẫn còn trong hàng đợi.", viec)
                    } else {
                        format!(
                            "Chưa rõ đã {} được chưa — ZaloCRM không trả lời, và hoá đơn đã rời hàng đợi (đã huỷ hoặc \
                             đã gửi in). Xem Nhật ký in trên ZaloCRM.",
                            viec
                        )
                    };
                    (CheDo::ChuaRo, chu)
                }
            };
            let co_nut_huy_lai = con_trong_anh
                && huy_duoc(muc)
                && matches!(tt, Some(ThaoTac::ThatBai { loai: LoaiViec::Huy, .. } | ThaoTac::ChuaRo { loai: LoaiViec::Huy }));
            DongHangDoi {
                id: muc.id.clone(),
                tieu_de,
                gio: gio(&muc.tao),
                trang_thai: nhan_trang_thai(muc),
                mau: mau_cua(muc),
                che_do,
                nut,
                bat_nut: bat,
                thong_diep,
                co_nut_bo: che_do == CheDo::ViSao && la_chua_xac_nhan(muc),
                co_nut_huy_lai,
            }
        };
        let tt_cua = |id: &str| self.thao_tac.iter().find(|m| m.muc.id == id).map(|m| &m.tt);
        for muc in anh.cho_in.iter().chain(anh.chua_xac_nhan.iter()) {
            dong.push(dong_cua(muc, tt_cua(&muc.id), true));
        }
        // Mục đã rời ảnh chụp nhưng còn kết quả / việc đang chạy: hiện bản sao ở
        // ĐÚNG chỗ cũ (hộp thoại của mục đã rời thì thôi).
        let mut con_lai: Vec<&MucThaoTac> = self
            .thao_tac
            .iter()
            .filter(|m| !m.tt.la_hop_thoai() && self.tim_trong_anh(&m.muc.id).is_none())
            .collect();
        con_lai.sort_by_key(|m| m.vi_tri);
        for m in con_lai {
            let d = dong_cua(&m.muc, Some(&m.tt), false);
            dong.insert(m.vi_tri.min(dong.len()), d);
        }

        let so_cho = anh.cho_in.len();
        let so_chua_xn = anh.chua_xac_nhan.len();
        let so_tam_giu = anh.cho_in.iter().filter(|m| m.tam_giu).count();
        let hien = !dong.is_empty() || self.loat.is_some();
        let tieu_de = if so_chua_xn > 0 {
            format!("HÀNG ĐỢI ({}) · {} chưa xác nhận", so_cho, so_chua_xn)
        } else {
            format!("HÀNG ĐỢI ({})", so_cho)
        };
        let ghi_chu = if !da_noi {
            "có thể chưa cập nhật — app đang mất kết nối".to_string()
        } else if !self.cua_ket_noi_nay {
            "đang tải lại…".to_string()
        } else if self.luc_nhan.is_some_and(|t| bay_gio.saturating_duration_since(t) >= CU_SAU) {
            "có thể chưa cập nhật".to_string()
        } else {
            String::new()
        };
        let so_huy_ca = if bat { self.ids_huy_ca().len() } else { 0 };
        let loat_nut = match &self.loat {
            Some(Loat::XacNhan { ids, .. }) => format!("Huỷ {} lệnh in", ids.len()),
            _ => String::new(),
        };
        let (loat_che_do, loat_chu) = match &self.loat {
            None => (0, String::new()),
            Some(Loat::XacNhan { ids, .. }) if bat => (
                1,
                format!("Huỷ cả {} lệnh in đang tạm giữ? Các hoá đơn này sẽ KHÔNG được in.", ids.len()),
            ),
            Some(Loat::XacNhan { .. }) => (0, String::new()),
            Some(Loat::Dang { ids, xong, .. }) => (2, format!("Đang huỷ {}/{} lệnh in…", xong, ids.len())),
            Some(Loat::Xong { tong, duoc, .. }) if duoc == tong => (3, format!("Đã huỷ {}/{} lệnh in", duoc, tong)),
            Some(Loat::Xong { tong, duoc, .. }) => (
                4,
                format!(
                    "Đã huỷ {}/{} — {} lệnh KHÔNG huỷ được hoặc chưa rõ, xem các dòng đỏ/vàng bên dưới.",
                    duoc,
                    tong,
                    tong - duoc
                ),
            ),
        };
        KhoiHangDoi {
            hien,
            tieu_de,
            ghi_chu,
            dai_tam_giu: if so_tam_giu > 0 { chu_dai_tam_giu(so_tam_giu, ma_may_in) } else { String::new() },
            so_huy_ca,
            loat_che_do,
            loat_chu,
            loat_nut,
            dong,
        }
    }
}

/// Gửi MỘT yêu cầu tới khi có câu trả lời chắc chắn, hoặc hết hạn. Huỷ / bỏ
/// theo dõi lặp lại là AN TOÀN (server: `da_huy_truoc` / điều kiện `khong_ro`)
/// nên hết giờ chờ ack thì hỏi lại thay vì bỏ ngang ở "chưa rõ".
///
/// Ack chỉ được tin khi `id` của nó ĐÚNG `id` đã hỏi — ack trùng id của
/// rust_socketio (net.rs `CongSocket`) coi như không đọc được, hỏi lại.
///
/// `bao_lan(lần, số lần đã rời máy)`: báo giao diện. `con_tiep()`: `false` = người
/// dùng đã Lưu cấu hình khác / yêu cầu bị xoá — thôi, không gửi thêm.
/// `ngu`/`bay_gio` tiêm vào cho test.
pub fn gui_yeu_cau(
    dg: &DuongGui,
    loai: LoaiViec,
    id: &str,
    bao_lan: &mut dyn FnMut(u32, u32),
    con_tiep: &dyn Fn() -> bool,
    ngu: &mut dyn FnMut(Duration),
    bay_gio: &dyn Fn() -> Instant,
) -> KetCuc {
    let han = bay_gio() + HAN_HOI_LAI;
    let mut lan = 0_u32;
    let mut da_gui = 0_u32;
    let mut ly_do = String::from("da thoi yeu cau");
    while con_tiep() {
        lan += 1;
        bao_lan(lan, da_gui);
        match dg.gui_ack_ro(loai.su_kien(), serde_json::json!({ "printJobId": id }), CanHoTro::HangDoi, CHO_ACK) {
            Ok(v) => match bao_cao::doc_ket_qua_huy(&v) {
                Some(kq) if kq.id == id && kq.ok => return KetCuc::Duoc { cach: kq.cach, noi_dung: kq.noi_dung },
                Some(kq) if kq.id == id => return KetCuc::KhongDuoc { loi: kq.loi, noi_dung: kq.noi_dung },
                _ => {
                    da_gui += 1;
                    ly_do = "tra loi khong dung hoa don".into();
                }
            },
            Err(LoiGuiAck::ChuaGui(e)) => ly_do = e,
            Err(LoiGuiAck::SauKhiGui(e)) => {
                da_gui += 1;
                ly_do = e;
            }
        }
        if da_gui >= SO_LAN_GUI_TOI_DA || bay_gio() + NGHI_HOI_LAI >= han {
            break;
        }
        ngu(NGHI_HOI_LAI);
    }
    if da_gui > 0 {
        KetCuc::ChuaRo { ly_do }
    } else {
        KetCuc::ChuaGui { ly_do }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bao_cao::HoTro;
    use crate::hop_thu_di::CongGui;
    use serde_json::{json, Value};
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};

    fn muc(id: &str, trang_thai: &str, tam_giu: bool) -> MucHangDoi {
        MucHangDoi {
            id: id.into(),
            so_hoa_don: format!("INV/{}", id),
            ten_khach: Some("Anh Dev".into()),
            trang_thai: trang_thai.into(),
            nhom: if trang_thai == "khong_ro" { "chua_xac_nhan" } else { "cho_in" }.into(),
            ly_do: String::new(),
            tam_giu,
            tao: "2026-09-25T11:45:00.000Z".into(),
            huy: if trang_thai == "cho_in" { "chac_chan" } else { "khong" }.into(),
        }
    }

    fn anh(cho: Vec<MucHangDoi>, cxn: Vec<MucHangDoi>) -> HangDoiServer {
        HangDoiServer { cho_in: cho, chua_xac_nhan: cxn, cap_nhat: String::new() }
    }

    fn san_sang(a: HangDoiServer, t: Instant) -> HangDoiApp {
        let mut h = HangDoiApp::default();
        h.nhan_cau_hinh(true);
        h.nhan_anh(a, t);
        h
    }

    fn khoi(h: &HangDoiApp, da_noi: bool, t: Instant) -> KhoiHangDoi {
        h.khoi(da_noi, Some(MaSuCo::HetGiay), t, &|_| "18:45".into())
    }

    fn che_do(h: &HangDoiApp, t: Instant) -> Vec<(String, CheDo)> {
        khoi(h, true, t).dong.into_iter().map(|d| (d.id, d.che_do)).collect()
    }

    /// Sau khi hộp mở đủ lâu (vượt chống bấm đúp).
    fn sau(t: Instant) -> Instant {
        t + CHONG_BAM_DUP
    }

    /// Huỷ `id` tới "Đang huỷ…" (mở hộp lúc `t`, xác nhận sau chống bấm đúp).
    fn dang_huy(h: &mut HangDoiApp, id: &str, t: Instant) -> MucHangDoi {
        assert!(h.bam_huy(id, true, t), "mở hộp {id}");
        h.bat_dau(id, LoaiViec::Huy, true, sau(t)).expect("xác nhận")
    }

    fn duoc() -> KetCuc {
        KetCuc::Duoc { cach: "chua_gui".into(), noi_dung: String::new() }
    }

    /// Mười hoá đơn tạm giữ lúc hết giấy (ca HCM 25/09): hiện đủ, dải cam nói
    /// đúng việc, mỗi dòng có nút Huỷ.
    #[test]
    fn muoi_hoa_don_tam_giu_hien_du_va_dai_cam() {
        let t = Instant::now();
        let ds: Vec<_> = (0..10).map(|i| muc(&format!("j{i}"), "cho_in", true)).collect();
        let h = san_sang(anh(ds, vec![]), t);
        let k = khoi(&h, true, t);
        assert!(k.hien);
        assert_eq!(k.tieu_de, "HÀNG ĐỢI (10)");
        assert_eq!(k.dong.len(), 10);
        assert!(k.dong.iter().all(|d| d.nut == NutDong::Huy && d.bat_nut && d.mau == MauDong::TamGiu));
        assert_eq!(
            k.dai_tam_giu,
            "10 hoá đơn đang chờ — máy in Hết giấy. Nạp giấy là tự in. Hoá đơn nào không cần nữa: bấm Huỷ TRƯỚC khi nạp giấy."
        );
        assert_eq!(k.so_huy_ca, 10);
        assert_eq!(k.dong[0].tieu_de, "INV/j0 · Anh Dev");
        assert_eq!(k.dong[0].trang_thai, "Tạm giữ — chờ máy in hết lỗi", "server không gửi lyDo → câu dự phòng");
    }

    /// Luồng chuẩn: Huỷ → xác nhận (nêu số hoá đơn) → Đang huỷ… → Đã huỷ (5 s)
    /// → rời danh sách, kể cả khi server đã bỏ mục khỏi ảnh chụp.
    #[test]
    fn huy_thanh_cong_hien_5_giay_roi_bien_mat() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true), muc("b", "cho_in", true)], vec![]), t);
        assert!(h.bam_huy("a", true, t));
        assert_eq!(che_do(&h, t)[0].1, CheDo::XacNhanHuy);
        assert_eq!(khoi(&h, true, t).dong[0].thong_diep, "Huỷ lệnh in INV/a? Hoá đơn này sẽ KHÔNG được in.");
        let m = h.bat_dau("a", LoaiViec::Huy, true, sau(t)).expect("xác nhận hợp lệ");
        assert_eq!(m.id, "a");
        assert_eq!(h.bat_dau("a", LoaiViec::Huy, true, sau(t)), None, "bấm đôi không gửi hai lần");
        assert_eq!(che_do(&h, t)[0].1, CheDo::Dang);
        h.ket_thuc("a", LoaiViec::Huy, &duoc(), t);
        // Server đẩy ảnh mới không còn "a" — dòng vẫn hiện ở ĐÚNG chỗ cũ.
        h.nhan_anh(anh(vec![muc("b", "cho_in", true)], vec![]), t);
        let k = khoi(&h, true, t);
        assert_eq!(k.dong.iter().map(|d| (d.id.as_str(), d.che_do)).collect::<Vec<_>>(), vec![("a", CheDo::DaHuy), ("b", CheDo::BinhThuong)]);
        assert_eq!(k.dong[0].thong_diep, "Đã huỷ — hoá đơn chắc chắn không in");
        assert_eq!(k.tieu_de, "HÀNG ĐỢI (1)", "đếm theo server");
        h.don_dep(t + Duration::from_secs(4));
        assert_eq!(che_do(&h, t).len(), 2);
        h.don_dep(t + GIU_DA_HUY);
        assert_eq!(che_do(&h, t), vec![("b".to_string(), CheDo::BinhThuong)]);
    }

    /// Bấm đúp "Huỷ": cú thứ hai rơi đúng chỗ "Huỷ lệnh in" (danh sách tự cuộn) —
    /// trong `CHONG_BAM_DUP` sau khi hộp mở thì KHÔNG huỷ.
    #[test]
    fn bam_dup_khong_huy_ngay() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        assert!(h.bam_huy("a", true, t));
        assert_eq!(h.bat_dau("a", LoaiViec::Huy, true, t + Duration::from_millis(150)), None);
        assert_eq!(che_do(&h, t)[0].1, CheDo::XacNhanHuy, "hộp vẫn mở, chờ bấm thật");
        assert!(h.bat_dau("a", LoaiViec::Huy, true, sau(t)).is_some());
        // Loạt cũng vậy.
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true), muc("b", "cho_in", true)], vec![]), t);
        assert!(h.bam_huy_ca(true, t));
        assert!(h.bat_dau_loat(true, t + Duration::from_millis(100)).is_empty());
        assert_eq!(h.bat_dau_loat(true, sau(t)).len(), 2);
    }

    /// Không huỷ được: đỏ, giữ nguyên tới khi bấm ×, KHÔNG bao giờ nói "Đã huỷ".
    #[test]
    fn huy_that_bai_giu_do_toi_khi_dong() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        dang_huy(&mut h, "a", t);
        h.ket_thuc(
            "a",
            LoaiViec::Huy,
            &KetCuc::KhongDuoc { loi: "DANG_IN".into(), noi_dung: "Hoá đơn đang được gửi/in ở máy in".into() },
            t,
        );
        h.don_dep(t + Duration::from_secs(3600));
        let d = &khoi(&h, true, t).dong[0];
        assert_eq!(d.che_do, CheDo::ThatBai);
        assert_eq!(d.thong_diep, "Không huỷ được: Hoá đơn đang được gửi/in ở máy in");
        assert!(!d.thong_diep.contains("Đã huỷ"));
        h.dong("a");
        assert_eq!(che_do(&h, t)[0].1, CheDo::BinhThuong);
    }

    /// Chưa gửi được = CHẮC CHẮN chưa huỷ; gửi rồi không trả lời = CHƯA RÕ, câu
    /// chữ theo việc hoá đơn còn / đã rời hàng đợi; còn chờ thì có "Huỷ lại".
    #[test]
    fn chua_gui_va_chua_ro_noi_dung_su_that() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true), muc("b", "cho_in", true), muc("c", "cho_in", true)], vec![]), t);
        for id in ["a", "b", "c"] {
            dang_huy(&mut h, id, t);
        }
        h.ket_thuc("a", LoaiViec::Huy, &KetCuc::ChuaGui { ly_do: "chua ket noi".into() }, t);
        h.ket_thuc("b", LoaiViec::Huy, &KetCuc::ChuaRo { ly_do: "het gio".into() }, t);
        h.ket_thuc("c", LoaiViec::Huy, &KetCuc::ChuaGui { ly_do: "backend chua ho tro".into() }, t);
        let k = khoi(&h, true, t);
        assert_eq!(k.dong[0].che_do, CheDo::ThatBai);
        assert!(k.dong[0].thong_diep.starts_with("CHƯA huỷ — không gửi được"), "{}", k.dong[0].thong_diep);
        assert!(k.dong[0].co_nut_huy_lai);
        assert_eq!(k.dong[1].che_do, CheDo::ChuaRo);
        assert!(k.dong[1].thong_diep.contains("VẪN đang chờ"), "{}", k.dong[1].thong_diep);
        assert!(k.dong[1].co_nut_huy_lai);
        assert!(k.dong[2].thong_diep.contains("chưa hỗ trợ"), "{}", k.dong[2].thong_diep);
        // "Huỷ lại" mở lại hộp xác nhận trên dòng lỗi.
        assert!(h.bam_huy("b", true, t));
        assert_eq!(khoi(&h, true, t).dong[1].che_do, CheDo::XacNhanHuy);
        h.dong("b");
        // Server bỏ "a" khỏi ảnh chụp (đã in / huỷ nơi khác): chưa rõ → nói đã rời hàng đợi, không "Huỷ lại".
        let mut h2 = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        dang_huy(&mut h2, "a", t);
        h2.ket_thuc("a", LoaiViec::Huy, &KetCuc::ChuaRo { ly_do: "het gio".into() }, t);
        h2.nhan_anh(anh(vec![], vec![]), t);
        let d = &khoi(&h2, true, t).dong[0];
        assert!(d.thong_diep.contains("đã rời hàng đợi") && !d.co_nut_huy_lai, "{}", d.thong_diep);
    }

    /// Đang hỏi xác nhận mà server vừa gửi hoá đơn xuống máy: hộp đóng (câu
    /// "sẽ KHÔNG được in" thành sai), nút đổi thành "Vì sao?".
    #[test]
    fn xac_nhan_dong_khi_hoa_don_vua_duoc_gui() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        h.bam_huy("a", true, t);
        h.nhan_anh(anh(vec![muc("a", "dang_gui", false)], vec![]), t);
        let d = &khoi(&h, true, t).dong[0];
        assert_eq!((d.che_do, d.nut, d.mau), (CheDo::BinhThuong, NutDong::ViSao, MauDong::DangIn));
        assert_eq!(h.bat_dau("a", LoaiViec::Huy, true, sau(t)), None, "không gửi huỷ cho hoá đơn đã rời server");
        assert!(!h.bam_huy("a", true, t));
    }

    /// Mất kết nối: ghi chú, khoá nút, hộp xác nhận không bấm được; nối lại thì
    /// chờ ảnh chụp mới mới mở khoá.
    #[test]
    fn mat_ket_noi_khoa_nut_va_ghi_chu() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        h.bam_huy("a", true, t);
        let k = khoi(&h, false, t);
        assert_eq!(k.ghi_chu, "có thể chưa cập nhật — app đang mất kết nối");
        assert!(!k.dong[0].bat_nut);
        assert_eq!(k.dong[0].che_do, CheDo::BinhThuong);
        assert_eq!(k.so_huy_ca, 0);
        assert_eq!(h.bat_dau("a", LoaiViec::Huy, false, sau(t)), None);
        assert!(!h.bam_huy("a", false, t));
        h.ket_noi_moi();
        let k = khoi(&h, true, t);
        assert_eq!(k.ghi_chu, "đang tải lại…");
        assert!(!k.dong[0].bat_nut, "ảnh chụp cũ: chưa cho bấm");
        assert_eq!(h.co_trong_anh("a"), None, "ảnh cũ không dùng để kết luận");
        h.nhan_anh(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        assert!(khoi(&h, true, t).dong[0].bat_nut);
        assert_eq!(h.co_trong_anh("a"), Some(true));
        assert_eq!(h.co_trong_anh("x"), Some(false));
        assert_eq!(khoi(&h, true, t + CU_SAU).ghi_chu, "có thể chưa cập nhật");
    }

    /// Chưa xác nhận: "Vì sao?" → giải thích + "Bỏ khỏi hàng đợi" → xác nhận →
    /// kết quả KHÔNG BAO GIỜ là "Đã huỷ".
    #[test]
    fn chua_xac_nhan_vi_sao_roi_bo_theo_doi() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![], vec![muc("k", "khong_ro", false)]), t);
        let k = khoi(&h, true, t);
        assert_eq!(k.tieu_de, "HÀNG ĐỢI (0) · 1 chưa xác nhận");
        assert_eq!((k.dong[0].nut, k.dong[0].mau), (NutDong::ViSao, MauDong::ChuaXacNhan));
        assert!(!h.bam_huy("k", true, t), "khong_ro không có nút Huỷ");
        assert!(h.bam_vi_sao("k"));
        let d = &khoi(&h, true, t).dong[0];
        assert_eq!((d.che_do, d.co_nut_bo), (CheDo::ViSao, true));
        assert_eq!(d.thong_diep, VI_SAO_CHUA_XAC_NHAN);
        assert!(h.bam_bo("k", true, t));
        assert!(khoi(&h, true, t).dong[0].thong_diep.starts_with("Bỏ INV/k khỏi hàng đợi?"));
        assert_eq!(h.bat_dau("k", LoaiViec::Huy, true, sau(t)), None, "xác nhận BỎ không được dùng để HUỶ");
        h.bat_dau("k", LoaiViec::BoTheoDoi, true, sau(t)).unwrap();
        h.ket_thuc("k", LoaiViec::BoTheoDoi, &KetCuc::Duoc { cach: String::new(), noi_dung: "Đã huỷ".into() }, t);
        let d = &khoi(&h, true, t).dong[0];
        assert_eq!(d.che_do, CheDo::DaBo);
        assert!(!d.thong_diep.contains("huỷ") && !d.thong_diep.contains("Huỷ"), "{}", d.thong_diep);
        h.don_dep(t + GIU_DA_HUY);
        assert_eq!(che_do(&h, t)[0].1, CheDo::DaBo, "bỏ theo dõi giữ lâu hơn");
        h.don_dep(t + GIU_DA_BO);
        h.nhan_anh(anh(vec![], vec![]), t);
        assert!(!khoi(&h, true, t).hien);
    }

    /// Đang gửi: "Vì sao?" nói đúng câu DANG_IN, không có nút bỏ.
    #[test]
    fn dang_gui_vi_sao() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "dang_gui", false)], vec![]), t);
        assert!(h.bam_vi_sao("a"));
        let d = &khoi(&h, true, t).dong[0];
        assert_eq!((d.thong_diep.as_str(), d.co_nut_bo), (VI_SAO_DANG_IN, false));
        assert!(!h.bam_bo("a", true, t));
        h.dong("a");
        assert_eq!(khoi(&h, true, t).dong[0].che_do, CheDo::BinhThuong);
    }

    /// Chỉ một hộp thoại mở một lúc.
    #[test]
    fn mot_hop_thoai_mot_luc() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true), muc("b", "cho_in", true)], vec![]), t);
        h.bam_huy("a", true, t);
        h.bam_huy("b", true, t);
        assert_eq!(che_do(&h, t), vec![("a".into(), CheDo::BinhThuong), ("b".into(), CheDo::XacNhanHuy)]);
        assert_eq!(h.bat_dau("a", LoaiViec::Huy, true, sau(t)), None);
    }

    /// "Huỷ cả N": chốt danh sách lúc hỏi — hoá đơn tới SAU không bị huỷ theo;
    /// tổng kết đúng số.
    #[test]
    fn huy_ca_chot_danh_sach_luc_hoi_va_tong_ket() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true), muc("b", "cho_in", true)], vec![]), t);
        assert!(h.bam_huy_ca(true, t));
        assert_eq!(khoi(&h, true, t).loat_che_do, 1);
        assert!(khoi(&h, true, t).loat_chu.starts_with("Huỷ cả 2 lệnh"));
        assert_eq!(khoi(&h, true, t).loat_nut, "Huỷ 2 lệnh in");
        // Hoá đơn mới tới trong lúc hộp xác nhận mở.
        h.nhan_anh(anh(vec![muc("a", "cho_in", true), muc("b", "cho_in", true), muc("c", "cho_in", true)], vec![]), t);
        let ds = h.bat_dau_loat(true, sau(t));
        assert_eq!(ds.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(khoi(&h, true, t).loat_chu, "Đang huỷ 0/2 lệnh in…");
        h.ket_thuc("a", LoaiViec::Huy, &duoc(), t);
        h.ket_thuc("b", LoaiViec::Huy, &KetCuc::KhongDuoc { loi: "DANG_IN".into(), noi_dung: "x".into() }, t);
        let k = khoi(&h, true, t);
        assert_eq!(k.loat_che_do, 4);
        assert!(k.loat_chu.starts_with("Đã huỷ 1/2"));
        assert_eq!(k.dong.iter().find(|d| d.id == "c").unwrap().che_do, CheDo::BinhThuong);
        h.don_dep(t + Duration::from_secs(3600));
        assert_eq!(khoi(&h, true, t).loat_che_do, 4, "có lỗi thì giữ tổng kết tới khi bấm Đóng");
        h.dong_loat();
        assert_eq!(khoi(&h, true, t).loat_che_do, 0);
    }

    /// Giám sát 0.2.6: một lệnh huỷ LẺ chạy song song không được tính vào loạt —
    /// tổng kết chỉ nói "đã huỷ hết" khi CHÍNH các hoá đơn của loạt đã huỷ.
    #[test]
    fn huy_ca_khong_dem_lenh_le_song_song() {
        let t = Instant::now();
        let mut h = san_sang(
            anh(vec![muc("a", "cho_in", true), muc("b", "cho_in", true), muc("c", "cho_in", false)], vec![]),
            t,
        );
        dang_huy(&mut h, "c", t);
        assert!(h.bam_huy_ca(true, t));
        let ds = h.bat_dau_loat(true, sau(t));
        assert_eq!(ds.len(), 2);
        h.ket_thuc("c", LoaiViec::Huy, &duoc(), t);
        h.ket_thuc("a", LoaiViec::Huy, &duoc(), t);
        assert_eq!(khoi(&h, true, t).loat_chu, "Đang huỷ 1/2 lệnh in…", "c không thuộc loạt");
        h.ket_thuc("b", LoaiViec::Huy, &KetCuc::KhongDuoc { loi: "DANG_IN".into(), noi_dung: "x".into() }, t);
        assert!(khoi(&h, true, t).loat_chu.starts_with("Đã huỷ 1/2"));
    }

    #[test]
    fn huy_ca_thanh_cong_het_tu_tat() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        h.bam_huy_ca(true, t);
        let ds = h.bat_dau_loat(true, sau(t));
        h.ket_thuc(&ds[0].id, LoaiViec::Huy, &duoc(), t);
        assert_eq!(khoi(&h, true, t).loat_chu, "Đã huỷ 1/1 lệnh in");
        h.don_dep(t + GIU_TONG_KET);
        assert_eq!(khoi(&h, true, t).loat_che_do, 0);
    }

    /// Lưu cấu hình giữa chừng (trạng thái dựng lại): luồng gửi thấy yêu cầu không
    /// còn và thôi gửi.
    #[test]
    fn con_dang_sau_khi_luu() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        dang_huy(&mut h, "a", t);
        assert!(h.con_dang("a"));
        let h = HangDoiApp::default();
        assert!(!h.con_dang("a"));
    }

    /// Backend cũ (không `hang_doi`) → không có khối nào.
    #[test]
    fn backend_khong_ho_tro_an_khoi() {
        let t = Instant::now();
        let mut h = san_sang(anh(vec![muc("a", "cho_in", true)], vec![]), t);
        h.nhan_cau_hinh(false);
        assert!(!khoi(&h, true, t).hien);
        assert!(!h.bam_huy("a", true, t));
    }

    /// Dải cam theo mã máy in; máy đã hết lỗi mà server còn giữ → "sắp tự in";
    /// app chưa đọc được máy in → không đoán lý do.
    #[test]
    fn cau_dai_tam_giu_theo_ma() {
        assert!(chu_dai_tam_giu(2, Some(MaSuCo::KetGiay)).contains("Gỡ giấy kẹt là tự in"));
        assert!(chu_dai_tam_giu(2, Some(MaSuCo::BinhThuong)).contains("sắp tự in"));
        assert!(chu_dai_tam_giu(2, None).contains("đang được giữ lại vì máy in đang lỗi"));
        assert!(!chu_dai_tam_giu(2, None).contains("sắp"));
        for ma in [None, Some(MaSuCo::HetGiay), Some(MaSuCo::Offline), Some(MaSuCo::LoiMayIn), Some(MaSuCo::HetMuc)] {
            assert!(!chu_dai_tam_giu(3, ma).to_lowercase().contains("in lại"), "R1: không bảo NV in lại");
        }
    }

    #[test]
    fn nhan_anh_chi_tom_tat_khi_so_luong_doi() {
        let t = Instant::now();
        let mut h = HangDoiApp::default();
        h.nhan_cau_hinh(true);
        assert!(h.nhan_anh(anh(vec![muc("a", "cho_in", true)], vec![]), t).is_some());
        assert!(h.nhan_anh(anh(vec![muc("b", "cho_in", true)], vec![]), t).is_none());
        assert_eq!(h.nhan_anh(anh(vec![], vec![]), t).as_deref(), Some("cho_in=0 tam_giu=0 chua_xac_nhan=0"));
    }

    #[test]
    fn dong_nhat_ky_bat_dau_ok() {
        let m = muc("a", "cho_in", true);
        assert!(duoc().dong_nhat_ky(&m).starts_with("ok=true "));
        for kc in [
            KetCuc::KhongDuoc { loi: "DA_IN".into(), noi_dung: "x".into() },
            KetCuc::ChuaGui { ly_do: "x".into() },
            KetCuc::ChuaRo { ly_do: "x".into() },
        ] {
            assert!(kc.dong_nhat_ky(&m).starts_with("ok=false "));
        }
    }

    // ---------- gui_yeu_cau với cổng giả ----------

    /// Cổng giả: mỗi lần emit_ack lấy một phản hồi từ danh sách.
    struct CongKichBan {
        phan_hoi: Mutex<Vec<Result<Value, String>>>,
        da_gui: Mutex<Vec<(String, Value)>>,
    }
    impl CongGui for CongKichBan {
        fn emit(&self, _: &str, _: Value) -> Result<(), String> {
            Ok(())
        }
        fn emit_ack(&self, su_kien: &str, gia_tri: Value, _cho: Duration) -> Result<Value, String> {
            self.da_gui.lock().unwrap().push((su_kien.into(), gia_tri));
            let mut p = self.phan_hoi.lock().unwrap();
            if p.is_empty() { Err("het gio cho ack".into()) } else { p.remove(0) }
        }
    }

    fn duong(phan_hoi: Vec<Result<Value, String>>, hang_doi: bool) -> (DuongGui, Arc<CongKichBan>) {
        let dg = DuongGui::default();
        let cong = Arc::new(CongKichBan { phan_hoi: Mutex::new(phan_hoi), da_gui: Mutex::new(Vec::new()) });
        dg.mo_ket_noi(cong.clone(), Instant::now());
        dg.nhan_cau_hinh(HoTro { hang_doi, ..HoTro::default() });
        (dg, cong)
    }

    fn chay(dg: &DuongGui, loai: LoaiViec) -> (KetCuc, u32) {
        let t0 = Instant::now();
        let dong_ho = Cell::new(t0);
        let mut lan_cuoi = 0;
        let kc = gui_yeu_cau(
            dg,
            loai,
            "a",
            &mut |n, _| lan_cuoi = n,
            &|| true,
            &mut |d| dong_ho.set(dong_ho.get() + d + CHO_ACK),
            &|| dong_ho.get(),
        );
        (kc, lan_cuoi)
    }

    #[test]
    fn gui_ok_mot_lan() {
        let (dg, cong) = duong(vec![Ok(json!({"id": "a", "ok": true, "cach": "chua_gui", "noiDung": "Đã huỷ"}))], true);
        let (kc, lan) = chay(&dg, LoaiViec::Huy);
        assert_eq!(kc, KetCuc::Duoc { cach: "chua_gui".into(), noi_dung: "Đã huỷ".into() });
        assert_eq!(lan, 1);
        assert_eq!(cong.da_gui.lock().unwrap()[0], ("yeu-cau-huy".to_string(), json!({"printJobId": "a"})));
    }

    /// Giám sát 0.2.6 (C1): ack của yêu cầu KHÁC (lô nhật ký `{ok:true, soDong}`,
    /// hay huỷ hoá đơn khác) KHÔNG BAO GIỜ thành "Đã huỷ" — hỏi lại, lấy câu thật.
    #[test]
    fn ack_khong_dung_id_khong_bao_gio_la_da_huy() {
        let (dg, cong) = duong(
            vec![
                Ok(json!({"ok": true, "soDong": 5})),
                Ok(json!({"id": "khac", "ok": true, "cach": "chua_gui", "noiDung": "Đã huỷ"})),
                Ok(json!({"id": "a", "ok": false, "loi": "DANG_IN", "noiDung": "đang in"})),
            ],
            true,
        );
        let (kc, _) = chay(&dg, LoaiViec::Huy);
        assert_eq!(kc, KetCuc::KhongDuoc { loi: "DANG_IN".into(), noi_dung: "đang in".into() });
        assert_eq!(cong.da_gui.lock().unwrap().len(), 3);
        // Chỉ toàn ack lạ → chưa rõ, không bao giờ "được".
        let (dg, _) = duong(vec![Ok(json!({"ok": true, "soDong": 1})); 3], true);
        assert!(matches!(chay(&dg, LoaiViec::Huy).0, KetCuc::ChuaRo { .. }));
    }

    /// Hết giờ lần đầu, lần hai server trả `da_huy_truoc` → ĐÃ HUỶ chắc chắn.
    #[test]
    fn het_gio_hoi_lai_ra_cau_tra_loi_chac() {
        let (dg, _) = duong(
            vec![Err("het gio cho ack".into()), Ok(json!({"id": "a", "ok": true, "cach": "da_huy_truoc", "noiDung": ""}))],
            true,
        );
        let (kc, lan) = chay(&dg, LoaiViec::Huy);
        assert_eq!(kc, KetCuc::Duoc { cach: "da_huy_truoc".into(), noi_dung: String::new() });
        assert_eq!(lan, 2);
    }

    #[test]
    fn khong_duoc_tra_ngay_khong_hoi_lai() {
        let (dg, cong) = duong(vec![Ok(json!({"id": "a", "ok": false, "loi": "DA_IN", "noiDung": "đã in"}))], true);
        let (kc, _) = chay(&dg, LoaiViec::Huy);
        assert_eq!(kc, KetCuc::KhongDuoc { loi: "DA_IN".into(), noi_dung: "đã in".into() });
        assert_eq!(cong.da_gui.lock().unwrap().len(), 1);
    }

    #[test]
    fn gui_ma_khong_ai_tra_loi_la_chua_ro_toi_da_3_lan() {
        let (dg, cong) = duong(vec![], true);
        let (kc, _) = chay(&dg, LoaiViec::BoTheoDoi);
        assert!(matches!(kc, KetCuc::ChuaRo { .. }), "{kc:?}");
        let g = cong.da_gui.lock().unwrap();
        assert_eq!(g.len() as u32, SO_LAN_GUI_TOI_DA);
        assert_eq!(g[0].0, "yeu-cau-bo-theo-doi");
    }

    /// Mất kết nối suốt: không lần nào rời máy → CHƯA GỬI (chắc chắn chưa huỷ).
    #[test]
    fn khong_ket_noi_la_chua_gui() {
        let dg = DuongGui::default();
        let (kc, lan) = chay(&dg, LoaiViec::Huy);
        assert_eq!(kc, KetCuc::ChuaGui { ly_do: "chua ket noi".into() });
        assert!(lan >= 2, "chờ nối lại trong hạn");
    }

    #[test]
    fn backend_khong_ho_tro_la_chua_gui() {
        let (dg, cong) = duong(vec![], false);
        let (kc, _) = chay(&dg, LoaiViec::Huy);
        assert!(matches!(kc, KetCuc::ChuaGui { .. }));
        assert!(cong.da_gui.lock().unwrap().is_empty());
    }

    /// Yêu cầu bị xoá giữa chừng (Lưu cấu hình) → thôi gửi.
    #[test]
    fn thoi_gui_khi_yeu_cau_bi_xoa() {
        let (dg, cong) = duong(vec![], true);
        let kc = gui_yeu_cau(&dg, LoaiViec::Huy, "a", &mut |_, _| {}, &|| false, &mut |_| {}, &Instant::now);
        assert!(matches!(kc, KetCuc::ChuaGui { .. }));
        assert!(cong.da_gui.lock().unwrap().is_empty());
    }
}
