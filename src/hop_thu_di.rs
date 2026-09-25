// SPDX-License-Identifier: AGPL-3.0-or-later
//! Đường gửi lên backend + hộp thư đi (R4, giám sát 25/09).
//!
//! LỖI CŨ: `ViecIn.socket` giữ `RawClient` của kết nối đã GIAO job. rust_socketio
//! 0.6 tự nối lại bằng cách THAY kết nối bên trong — emit trên `RawClient` cũ
//! trả `IllegalActionBeforeOpen`, bị `let _` nuốt, file nhật ký vẫn ghi
//! `gui_server=co`. Kết quả in rơi mất, backend hết giờ chờ → `khong_ro`.
//!
//! NAY: mọi event (`ket-qua`, `su-co`, `trang-thai-may-in`) lấy cổng gửi của
//! kết nối HIỆN TẠI lúc gửi. Chưa có kết nối, emit lỗi, hoặc kết nối chưa báo
//! `hoTro` mà event cần `hoTro` → cất vào hộp thư đi (trần `TRAN_HOP_THU`, bỏ
//! thư cũ nhất); gửi lại khi kết nối mới gửi `cau-hinh` — LỌC LẠI theo `hoTro`
//! MỚI (backend có thể vừa bị lùi về bản cũ: gửi `khong_ro` cho nó là báo sai).
//! Nơi gọi ghi nhật ký ĐÚNG kết quả: `gui_server=co|xep_hang|bo`.
//!
//! Emit KHÔNG làm trong lúc giữ khoá: emit của engine.io là lời gọi CHẶN (gửi
//! websocket — app chỉ dùng websocket từ giám sát vòng 3, T1) — giữ khoá lúc
//! đó là chặn luôn callback "open"/"cau-hinh" của kết nối mới (chặn callback =
//! mất ping, bài học 14–17/09).

use crate::bao_cao::HoTro;
use crate::job::{self, KetQua};
use crate::nhat_ky;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Trần hộp thư đi — máy mất mạng cả buổi vẫn không phình bộ nhớ.
pub const TRAN_HOP_THU: usize = 200;
/// Kết nối mở chừng này mà không có `cau-hinh` → coi là backend bản cũ (R12).
pub const CHO_CAU_HINH: Duration = Duration::from_secs(10);

/// Điều kiện `hoTro` để một event được gửi (hợp đồng §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanHoTro {
    /// `ket-qua` `da_in`/`loi`, `thong-tin-app`: backend nào cũng hiểu.
    KhongCan,
    /// `ket-qua` `khong_ro` — và `da_in` MUỘN của theo dõi tiếp (R3).
    KhongRo,
    SuCo,
    TrangThaiMayIn,
    /// `nhat-ky-app` — KHÔNG BAO GIỜ vào hộp thư đi (trần 200 thư: nhật ký dồn
    /// vào là đẩy mất `ket-qua`); bộ đệm riêng ở nhat_ky.rs, gửi qua `gui_ack`.
    NhatKyApp,
    /// `lay-hang-doi`, `yeu-cau-huy`, `yeu-cau-bo-theo-doi` (hợp đồng v5.1 §8.7)
    /// — KHÔNG BAO GIỜ vào hộp thư đi: yêu cầu huỷ gửi muộn sau khi nối lại là
    /// huỷ một việc người dùng đã thôi chờ; gửi qua `gui_ngay`/`gui_ack_ro`.
    HangDoi,
}

impl CanHoTro {
    pub fn duoc_gui(self, h: HoTro) -> bool {
        match self {
            CanHoTro::KhongCan => true,
            CanHoTro::KhongRo => h.khong_ro,
            CanHoTro::SuCo => h.su_co,
            CanHoTro::TrangThaiMayIn => h.trang_thai_may_in,
            CanHoTro::NhatKyApp => h.nhat_ky_app,
            CanHoTro::HangDoi => h.hang_doi,
        }
    }

    /// `ket-qua`: `khong_ro` cần hoTro (chống in đôi §0.4), `da_in`/`loi` thì không.
    pub fn cua_ket_qua(kq: &KetQua) -> Self {
        if kq.trang_thai == job::KHONG_RO {
            CanHoTro::KhongRo
        } else {
            CanHoTro::KhongCan
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThuDi {
    pub su_kien: &'static str,
    pub gia_tri: Value,
    pub can: CanHoTro,
}

/// Kết quả gửi — đúng chữ ghi vào nhật ký `gui_server=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KetQuaGui {
    /// Đã emit thành công qua kết nối hiện tại.
    Co,
    /// Cất vào hộp thư đi, gửi khi có kết nối/`cau-hinh`.
    XepHang,
    /// Không gửi: backend của kết nối hiện tại không hỗ trợ event/trạng thái này.
    Bo,
}

impl KetQuaGui {
    pub fn chu(self) -> &'static str {
        match self {
            KetQuaGui::Co => "co",
            KetQuaGui::XepHang => "xep_hang",
            KetQuaGui::Bo => "bo",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuyetGui {
    GuiNgay,
    XepHang,
    Bo,
}

/// Quyết gửi một thư theo trạng thái kết nối — THUẦN.
/// `ho_tro`: `None` = kết nối chưa gửi `cau-hinh` (và chưa bị kết luận là bản cũ).
pub fn quyet_gui(co_ket_noi: bool, ho_tro: Option<HoTro>, can: CanHoTro) -> QuyetGui {
    match (co_ket_noi, ho_tro) {
        (false, _) => QuyetGui::XepHang,
        // Chưa biết hoTro: event ai cũng hiểu thì gửi luôn, còn lại chờ `cau-hinh`.
        (true, None) if can == CanHoTro::KhongCan => QuyetGui::GuiNgay,
        (true, None) => QuyetGui::XepHang,
        (true, Some(h)) if can.duoc_gui(h) => QuyetGui::GuiNgay,
        (true, Some(_)) => QuyetGui::Bo,
    }
}

/// Hộp thư đi có trần — THUẦN.
#[derive(Debug, Default)]
pub struct HopThuDi {
    ds: VecDeque<ThuDi>,
}

impl HopThuDi {
    /// Cất một thư; trả thư CŨ NHẤT bị bỏ nếu vượt trần. `trang-thai-may-in`
    /// chỉ giữ bản mới nhất — trạng thái cũ gửi muộn chỉ làm nhật ký backend
    /// nhảy qua lại (ngay sau `cau-hinh` app còn gửi trạng thái hiện tại).
    pub fn cat(&mut self, thu: ThuDi) -> Option<ThuDi> {
        if thu.su_kien == "trang-thai-may-in" {
            self.ds.retain(|t| t.su_kien != "trang-thai-may-in");
        }
        self.ds.push_back(thu);
        if self.ds.len() > TRAN_HOP_THU {
            self.ds.pop_front()
        } else {
            None
        }
    }

    /// Trả thư về ĐẦU hộp (gửi lại lỗi giữa chừng) — giữ đúng thứ tự cũ.
    fn tra_ve_dau(&mut self, thu: Vec<ThuDi>) -> Vec<ThuDi> {
        for t in thu.into_iter().rev() {
            self.ds.push_front(t);
        }
        let mut bo = Vec::new();
        while self.ds.len() > TRAN_HOP_THU {
            bo.extend(self.ds.pop_front());
        }
        bo
    }

    pub fn lay_het(&mut self) -> Vec<ThuDi> {
        self.ds.drain(..).collect()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.ds.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.ds.is_empty()
    }
}

/// Cổng emit của MỘT kết nối (thật: `RawClient`, xem net.rs). Trait để test
/// đường gửi mà không cần socket.
pub trait CongGui: Send + Sync {
    fn emit(&self, su_kien: &str, gia_tri: Value) -> Result<(), String>;

    /// Emit kèm ack, CHỜ tối đa `cho` lấy giá trị ack đầu tiên. Mặc định: không
    /// hỗ trợ (cổng giả trong test cũ).
    fn emit_ack(&self, _su_kien: &str, _gia_tri: Value, _cho: Duration) -> Result<Value, String> {
        Err("cong khong ho tro ack".into())
    }
}

/// Lỗi của `gui_ack_ro`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoiGuiAck {
    /// Chưa rời máy: chưa kết nối / backend chưa báo hỗ trợ.
    ChuaGui(String),
    /// Đã emit (hoặc emit lỗi giữa chừng) mà không có ack — server có thể đã làm.
    SauKhiGui(String),
}

#[derive(Default)]
struct TrangThaiGui {
    cong: Option<Arc<dyn CongGui>>,
    /// Tăng mỗi lần "open" — để "đóng" của kết nối cũ tới trễ không xoá nhầm kết nối mới.
    the_he: u64,
    ho_tro: Option<HoTro>,
    /// Kết nối HIỆN TẠI đã nhận `cau-hinh` thật (không phải `HoTro` mặc định
    /// do `kiem_ban_cu` gán cho backend cũ).
    co_cau_hinh: bool,
    luc_mo: Option<Instant>,
    hop_thu: HopThuDi,
}

/// Đường gửi DÙNG CHUNG cho mọi luồng — và cho mọi lần chạy `chay_net` (bấm
/// Lưu dựng kết nối mới, R7): kết quả của job đang in dở lúc Lưu vẫn đi được
/// qua kết nối mới.
#[derive(Default)]
pub struct DuongGui {
    inner: Mutex<TrangThaiGui>,
    /// Số yêu cầu hàng đợi (huỷ / bỏ theo dõi) đang chạy — luồng gửi nhật ký
    /// nhường (một ack sống mỗi kết nối: lô nhật ký chậm không được bắt lệnh huỷ chờ).
    uu_tien: std::sync::atomic::AtomicUsize,
}

/// Giữ quyền ưu tiên tới khi rơi khỏi phạm vi.
pub struct UuTien<'a>(&'a DuongGui);

impl Drop for UuTien<'_> {
    fn drop(&mut self) {
        self.0.uu_tien.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl DuongGui {
    /// Yêu cầu hàng đợi bắt đầu — luồng nhật ký nhường tới khi thả.
    pub fn giu_uu_tien(&self) -> UuTien<'_> {
        self.uu_tien.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        UuTien(self)
    }

    /// Có yêu cầu hàng đợi đang chạy không.
    pub fn co_uu_tien(&self) -> bool {
        self.uu_tien.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    fn khoa(&self) -> std::sync::MutexGuard<'_, TrangThaiGui> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn cat(&self, thu: ThuDi) {
        if let Some(bo) = self.khoa().hop_thu.cat(thu) {
            nhat_ky::ghi("hop_thu_tran", &format!("bo thu cu nhat: {}", bo.su_kien));
        }
    }

    /// Gửi một event qua kết nối HIỆN TẠI (hoặc xếp hàng / bỏ theo `hoTro`).
    pub fn gui(&self, su_kien: &'static str, gia_tri: Value, can: CanHoTro) -> KetQuaGui {
        let (quyet, cong) = {
            let k = self.khoa();
            (quyet_gui(k.cong.is_some(), k.ho_tro, can), k.cong.clone())
        };
        match (quyet, cong) {
            (QuyetGui::Bo, _) => KetQuaGui::Bo,
            (QuyetGui::GuiNgay, Some(cong)) => match cong.emit(su_kien, gia_tri.clone()) {
                Ok(()) => KetQuaGui::Co,
                Err(e) => {
                    eprintln!("[print-agent] emit {} lỗi ({}) — cất vào hộp thư đi", su_kien, e);
                    self.cat(ThuDi { su_kien, gia_tri, can });
                    KetQuaGui::XepHang
                }
            },
            _ => {
                self.cat(ThuDi { su_kien, gia_tri, can });
                KetQuaGui::XepHang
            }
        }
    }

    /// Cổng của kết nối hiện tại nếu nó đã báo hỗ trợ `can`.
    fn cong_ho_tro(&self, can: CanHoTro) -> Result<Arc<dyn CongGui>, String> {
        let k = self.khoa();
        match (&k.cong, k.ho_tro) {
            (Some(c), Some(h)) if can.duoc_gui(h) => Ok(c.clone()),
            (None, _) => Err("chua ket noi".into()),
            _ => Err("backend chua ho tro".into()),
        }
    }

    /// Gửi NGAY kèm ack qua kết nối hiện tại, KHÔNG cất hộp thư đi: chưa kết nối /
    /// kết nối chưa báo hỗ trợ `can` → `Err` để người gọi (bộ đệm nhật ký) tự giữ
    /// lại. Không giữ khoá trong lúc chờ ack.
    pub fn gui_ack(&self, su_kien: &str, gia_tri: Value, can: CanHoTro, cho: Duration) -> Result<Value, String> {
        self.gui_ack_ro(su_kien, gia_tri, can, cho).map_err(|e| match e {
            LoiGuiAck::ChuaGui(s) | LoiGuiAck::SauKhiGui(s) => s,
        })
    }

    /// Như `gui_ack` nhưng nói rõ lỗi xảy ra TRƯỚC hay SAU khi yêu cầu có thể đã
    /// rời máy — huỷ lệnh in cần biết: chưa gửi = CHẮC CHẮN chưa huỷ; đã gửi mà
    /// không có trả lời = CHƯA RÕ (server có thể đã huỷ).
    pub fn gui_ack_ro(&self, su_kien: &str, gia_tri: Value, can: CanHoTro, cho: Duration) -> Result<Value, LoiGuiAck> {
        let cong = self.cong_ho_tro(can).map_err(LoiGuiAck::ChuaGui)?;
        cong.emit_ack(su_kien, gia_tri, cho).map_err(LoiGuiAck::SauKhiGui)
    }

    /// Gửi NGAY không ack, KHÔNG cất hộp thư đi (chưa kết nối / chưa hỗ trợ → `Err`).
    pub fn gui_ngay(&self, su_kien: &str, gia_tri: Value, can: CanHoTro) -> Result<(), String> {
        self.cong_ho_tro(can)?.emit(su_kien, gia_tri)
    }

    /// Kết nối hiện tại đã nhận `cau-hinh` báo hỗ trợ `can` — trả thế hệ kết nối.
    pub fn the_he_ho_tro(&self, can: CanHoTro) -> Option<u64> {
        let k = self.khoa();
        match (&k.cong, k.ho_tro) {
            (Some(_), Some(h)) if k.co_cau_hinh && can.duoc_gui(h) => Some(k.the_he),
            _ => None,
        }
    }

    /// Callback "open": kết nối mới — QUÊN hoTro của kết nối trước (§2). Trả thế hệ.
    pub fn mo_ket_noi(&self, cong: Arc<dyn CongGui>, bay_gio: Instant) -> u64 {
        let mut k = self.khoa();
        k.the_he += 1;
        k.cong = Some(cong);
        k.ho_tro = None;
        k.co_cau_hinh = false;
        k.luc_mo = Some(bay_gio);
        k.the_he
    }

    /// Kết nối thế hệ `the_he` không dùng được nữa (bị cho nghỉ, R7). Thế hệ
    /// đã cũ thì bỏ qua — không xoá nhầm kết nối mới.
    pub fn dong_ket_noi(&self, the_he: u64) {
        let mut k = self.khoa();
        if k.the_he == the_he {
            k.cong = None;
            k.ho_tro = None;
            k.co_cau_hinh = false;
            k.luc_mo = None;
        }
    }

    /// Callback "cau-hinh": ghi hoTro của kết nối hiện tại.
    pub fn nhan_cau_hinh(&self, ho_tro: HoTro) {
        let mut k = self.khoa();
        k.ho_tro = Some(ho_tro);
        k.co_cau_hinh = true;
    }

    /// Backend của kết nối HIỆN TẠI là bản mới (đã gửi `cau-hinh`) — có cầu
    /// dao và luật "không tiêu lượt thử" (hợp đồng v4 §1/§3.1). Chỉ khi đó app
    /// mới được TỪ CHỐI in trước khi gọi Sumatra (T2): backend cũ tiêu một
    /// lượt mỗi lần `loi`, vài phút là `that_bai`. Chưa có kết nối / chưa có
    /// `cau-hinh` / đã kết luận bản cũ → `false`.
    pub fn backend_moi(&self) -> bool {
        let k = self.khoa();
        k.cong.is_some() && k.co_cau_hinh
    }

    /// R12: kết nối mở đã `CHO_CAU_HINH` mà chưa có `cau-hinh` → kết luận
    /// backend bản cũ (hoTro rỗng — giữ hành vi cũ: `khong_ro` không bao giờ
    /// gửi). Trả `true` đúng MỘT lần mỗi kết nối.
    pub fn kiem_ban_cu(&self, bay_gio: Instant) -> bool {
        let mut k = self.khoa();
        let ban_cu =
            k.cong.is_some() && k.ho_tro.is_none() && k.luc_mo.is_some_and(|t| bay_gio.saturating_duration_since(t) >= CHO_CAU_HINH);
        if ban_cu {
            k.ho_tro = Some(HoTro::default());
        }
        ban_cu
    }

    /// Gửi lại hộp thư đi theo trạng thái kết nối HIỆN TẠI (gọi sau `cau-hinh`,
    /// hoặc sau khi kết luận bản cũ). Trả từng thư kèm kết quả để ghi nhật ký.
    pub fn xa_hop_thu(&self) -> Vec<(ThuDi, KetQuaGui)> {
        let (thu, cong, ho_tro) = {
            let mut k = self.khoa();
            (k.hop_thu.lay_het(), k.cong.clone(), k.ho_tro)
        };
        let mut ra = Vec::with_capacity(thu.len());
        let mut con: Vec<ThuDi> = Vec::new();
        let mut dut = false;
        for t in thu {
            if dut {
                con.push(t);
                continue;
            }
            match (quyet_gui(cong.is_some(), ho_tro, t.can), &cong) {
                (QuyetGui::Bo, _) => ra.push((t, KetQuaGui::Bo)),
                (QuyetGui::GuiNgay, Some(c)) => match c.emit(t.su_kien, t.gia_tri.clone()) {
                    Ok(()) => ra.push((t, KetQuaGui::Co)),
                    Err(_) => {
                        // Kết nối lại hỏng giữa chừng — giữ nguyên phần còn lại, đúng thứ tự.
                        dut = true;
                        con.push(t);
                    }
                },
                _ => con.push(t),
            }
        }
        if !con.is_empty() {
            for bo in self.khoa().hop_thu.tra_ve_dau(con) {
                nhat_ky::ghi("hop_thu_tran", &format!("bo thu cu nhat: {}", bo.su_kien));
            }
        }
        ra
    }

    #[cfg(test)]
    fn so_thu_cho(&self) -> usize {
        self.khoa().hop_thu.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn du() -> HoTro {
        HoTro { khong_ro: true, su_co: true, trang_thai_may_in: true, nhat_ky_app: false, hang_doi: false }
    }

    #[test]
    fn quyet_gui_theo_ket_noi_va_ho_tro() {
        use CanHoTro::*;
        assert_eq!(quyet_gui(false, Some(du()), KhongCan), QuyetGui::XepHang, "chưa có kết nối → xếp hàng");
        assert_eq!(quyet_gui(true, None, KhongCan), QuyetGui::GuiNgay, "da_in/loi không cần chờ cau-hinh");
        assert_eq!(quyet_gui(true, None, KhongRo), QuyetGui::XepHang, "khong_ro chờ biết hoTro");
        assert_eq!(quyet_gui(true, Some(du()), KhongRo), QuyetGui::GuiNgay);
        assert_eq!(quyet_gui(true, Some(HoTro::default()), KhongRo), QuyetGui::Bo, "§0.4: backend cũ không nhận khong_ro");
        assert_eq!(quyet_gui(true, Some(HoTro::default()), SuCo), QuyetGui::Bo);
        assert_eq!(quyet_gui(true, Some(HoTro::default()), TrangThaiMayIn), QuyetGui::Bo);
        assert_eq!(quyet_gui(true, Some(HoTro::default()), KhongCan), QuyetGui::GuiNgay);
    }

    fn thu(i: usize) -> ThuDi {
        ThuDi { su_kien: "ket-qua", gia_tri: json!({"jobId": i.to_string()}), can: CanHoTro::KhongCan }
    }

    #[test]
    fn hop_thu_co_tran_bo_cu_nhat() {
        let mut h = HopThuDi::default();
        for i in 0..TRAN_HOP_THU {
            assert!(h.cat(thu(i)).is_none());
        }
        let bo = h.cat(thu(999)).unwrap();
        assert_eq!(bo.gia_tri["jobId"], "0", "bỏ thư CŨ NHẤT");
        assert_eq!(h.len(), TRAN_HOP_THU);
        let ds = h.lay_het();
        assert_eq!(ds.first().unwrap().gia_tri["jobId"], "1");
        assert_eq!(ds.last().unwrap().gia_tri["jobId"], "999");
        assert!(h.is_empty());
    }

    #[test]
    fn hop_thu_chi_giu_trang_thai_may_in_moi_nhat() {
        let mut h = HopThuDi::default();
        let tt = |ma: &str| ThuDi { su_kien: "trang-thai-may-in", gia_tri: json!({"trangThai": ma}), can: CanHoTro::TrangThaiMayIn };
        h.cat(tt("het_giay"));
        h.cat(thu(1));
        h.cat(tt("binh_thuong"));
        let ds = h.lay_het();
        assert_eq!(ds.len(), 2);
        assert_eq!(ds[1].gia_tri["trangThai"], "binh_thuong");
    }

    /// Cổng giả: ghi mọi emit; `hong` = emit lỗi (kết nối đã bị thư viện thay).
    #[derive(Default)]
    struct CongGia {
        da_gui: Mutex<Vec<(String, Value)>>,
        hong: AtomicBool,
    }
    impl CongGui for CongGia {
        fn emit(&self, su_kien: &str, gia_tri: Value) -> Result<(), String> {
            if self.hong.load(Ordering::SeqCst) {
                return Err("IllegalActionBeforeOpen".into());
            }
            self.da_gui.lock().unwrap().push((su_kien.to_string(), gia_tri));
            Ok(())
        }
    }

    /// Lỗi R4 cũ: emit trên socket đã chết bị nuốt, nhật ký vẫn "co". Nay: emit
    /// lỗi → xep_hang, và thư đi qua kết nối MỚI khi có `cau-hinh`.
    #[test]
    fn emit_loi_thi_xep_hang_roi_gui_qua_ket_noi_moi() {
        let dg = DuongGui::default();
        let cu = Arc::new(CongGia::default());
        let t0 = Instant::now();
        dg.mo_ket_noi(cu.clone(), t0);
        dg.nhan_cau_hinh(du());
        cu.hong.store(true, Ordering::SeqCst);
        let kq = dg.gui("ket-qua", json!({"jobId": "1790251200000-7", "trangThai": "da_in"}), CanHoTro::KhongCan);
        assert_eq!(kq, KetQuaGui::XepHang, "emit lỗi KHÔNG được báo là đã gửi");
        assert_eq!(dg.so_thu_cho(), 1);

        let moi = Arc::new(CongGia::default());
        dg.mo_ket_noi(moi.clone(), t0);
        dg.nhan_cau_hinh(du());
        let ra = dg.xa_hop_thu();
        assert_eq!(ra.len(), 1);
        assert_eq!(ra[0].1, KetQuaGui::Co);
        assert_eq!(moi.da_gui.lock().unwrap()[0].1["jobId"], "1790251200000-7");
        assert!(cu.da_gui.lock().unwrap().is_empty());
        assert_eq!(dg.so_thu_cho(), 0);
    }

    /// Lọc lại theo hoTro MỚI: backend vừa bị lùi về bản cũ → khong_ro/su-co bị bỏ.
    #[test]
    fn xa_hop_thu_loc_lai_theo_ho_tro_moi() {
        let dg = DuongGui::default();
        dg.gui("ket-qua", json!({"trangThai": "khong_ro"}), CanHoTro::KhongRo);
        dg.gui("su-co", json!({"loai": "het_giay"}), CanHoTro::SuCo);
        dg.gui("ket-qua", json!({"trangThai": "loi"}), CanHoTro::KhongCan);
        assert_eq!(dg.so_thu_cho(), 3, "chưa có kết nối → xếp hàng hết");

        let moi = Arc::new(CongGia::default());
        let t0 = Instant::now();
        dg.mo_ket_noi(moi.clone(), t0);
        // Chưa có cau-hinh: xả ngay chỉ gửi được thư không cần hoTro.
        let ra = dg.xa_hop_thu();
        assert_eq!(ra.iter().map(|(t, k)| (t.su_kien, *k)).collect::<Vec<_>>(), vec![("ket-qua", KetQuaGui::Co)]);
        assert_eq!(dg.so_thu_cho(), 2);
        // R12: 10 s không cau-hinh → bản cũ → phần còn lại bị BỎ, không bao giờ gửi khong_ro.
        assert!(!dg.kiem_ban_cu(t0 + Duration::from_secs(9)));
        assert!(dg.kiem_ban_cu(t0 + CHO_CAU_HINH));
        assert!(!dg.kiem_ban_cu(t0 + CHO_CAU_HINH * 2), "chỉ báo một lần mỗi kết nối");
        let ra = dg.xa_hop_thu();
        assert!(ra.iter().all(|(_, k)| *k == KetQuaGui::Bo), "{:?}", ra);
        assert_eq!(moi.da_gui.lock().unwrap().len(), 1);
        assert_eq!(dg.gui("ket-qua", json!({"trangThai": "khong_ro"}), CanHoTro::KhongRo), KetQuaGui::Bo);
    }

    /// T2: chỉ kết nối đã nhận `cau-hinh` THẬT mới là backend mới; bản cũ
    /// (kết luận sau 10 s), chưa có `cau-hinh`, mất kết nối, kết nối MỚI → không.
    #[test]
    fn t2_backend_moi_chi_khi_ket_noi_hien_tai_co_cau_hinh() {
        let dg = DuongGui::default();
        assert!(!dg.backend_moi(), "chưa có kết nối");
        let t0 = Instant::now();
        let the_he = dg.mo_ket_noi(Arc::new(CongGia::default()), t0);
        assert!(!dg.backend_moi(), "chưa có cau-hinh");
        assert!(dg.kiem_ban_cu(t0 + CHO_CAU_HINH));
        assert!(!dg.backend_moi(), "server bản cũ (không gửi cau-hinh) → không từ chối in");
        dg.mo_ket_noi(Arc::new(CongGia::default()), t0);
        dg.nhan_cau_hinh(du());
        assert!(dg.backend_moi());
        dg.dong_ket_noi(the_he);
        assert!(dg.backend_moi(), "đóng kết nối CŨ không đụng kết nối mới");
        let the_he = dg.mo_ket_noi(Arc::new(CongGia::default()), t0);
        assert!(!dg.backend_moi(), "kết nối mới quên cau-hinh của kết nối trước");
        dg.nhan_cau_hinh(du());
        dg.dong_ket_noi(the_he);
        assert!(!dg.backend_moi(), "mất kết nối");
    }

    #[test]
    fn dong_ket_noi_cu_khong_xoa_ket_noi_moi() {
        let dg = DuongGui::default();
        let t0 = Instant::now();
        let the_he_cu = dg.mo_ket_noi(Arc::new(CongGia::default()), t0);
        let moi = Arc::new(CongGia::default());
        dg.mo_ket_noi(moi.clone(), t0);
        dg.dong_ket_noi(the_he_cu);
        assert_eq!(dg.gui("ket-qua", json!({}), CanHoTro::KhongCan), KetQuaGui::Co);
        assert_eq!(moi.da_gui.lock().unwrap().len(), 1);
        let the_he_moi = dg.mo_ket_noi(moi, t0);
        dg.dong_ket_noi(the_he_moi);
        assert_eq!(dg.gui("ket-qua", json!({}), CanHoTro::KhongCan), KetQuaGui::XepHang);
    }

    #[test]
    fn xa_hop_thu_dut_giua_chung_giu_thu_tu() {
        let dg = DuongGui::default();
        for i in 0..3 {
            dg.gui("ket-qua", json!({"jobId": i}), CanHoTro::KhongCan);
        }
        let hong = Arc::new(CongGia::default());
        hong.hong.store(true, Ordering::SeqCst);
        dg.mo_ket_noi(hong, Instant::now());
        assert!(dg.xa_hop_thu().is_empty());
        let moi = Arc::new(CongGia::default());
        dg.mo_ket_noi(moi.clone(), Instant::now());
        dg.xa_hop_thu();
        let ids: Vec<i64> = moi.da_gui.lock().unwrap().iter().map(|(_, v)| v["jobId"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![0, 1, 2], "gửi lại đúng thứ tự");
    }
}
