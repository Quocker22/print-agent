// SPDX-License-Identifier: AGPL-3.0-or-later
//! Nhận ra máy tính VỪA THỨC DẬY sau khi ngủ (0.2.7) — để nối lại ZaloCRM
//! NGAY, không đợi.
//!
//! VÌ SAO: ngủ quá ~45 s thì server đã bỏ kết nối (ping timeout) nhưng phía
//! app, kết nối cũ trông vẫn "đã nối" tới khi hết hạn ping của thư viện (tới
//! 45 s SAU khi thức); đang chờ nối lại thì còn phải chờ nốt backoff (tới 30 s).
//! Suốt lúc đó server coi máy in offline — hoá đơn gửi tới bị tính lượt thử.
//!
//! CÁCH: một luồng thức mỗi `NHIP` (2 s) so đồng hồ tường (`SystemTime`) và
//! đồng hồ đơn điệu (`Instant`) với lần trước. Tiến trình bị treo lâu hơn
//! nhịp + `NGUONG` = máy vừa ngủ (hoặc bị đóng băng lâu — nối lại cũng đúng).
//! Dùng CẢ HAI đồng hồ vì trên Windows `Instant` (QPC) có thể không đếm thời
//! gian ngủ; đồng hồ tường thì đếm nhưng có thể bị chỉnh giờ — chỉnh LÙI bị bỏ
//! qua, chỉnh TIẾN nhiều nhất gây một lần nối lại thừa (vô hại).
//! Không cần cửa sổ hay đăng ký thông báo nguồn của Windows — chạy như nhau
//! trên máy ngủ S3, Modern Standby, ngủ đông.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Nhịp của luồng canh.
pub const NHIP: Duration = Duration::from_secs(2);
/// Bị treo quá nhịp chừng này trở lên thì coi là vừa ngủ dậy.
pub const NGUONG: Duration = Duration::from_secs(15);

/// Số lần thức dậy từ lúc app chạy — nơi cần biết (vòng nối lại) so số này.
static LAN: AtomicU64 = AtomicU64::new(0);

pub fn lan_thuc_day() -> u64 {
    LAN.load(Ordering::SeqCst)
}

/// Test đầu-cuối: giả lập "máy vừa ngủ dậy".
#[cfg(test)]
pub fn gia_lap_thuc_day() {
    LAN.fetch_add(1, Ordering::SeqCst);
}

/// Một nhịp: `(tuong, don_dieu)` trôi qua từ lần trước (đồng hồ tường chạy
/// LÙI → `None`). Trả thời gian ngủ ước lượng nếu vượt `nhip + NGUONG`.
pub fn xet(tuong: Option<Duration>, don_dieu: Duration, nhip: Duration) -> Option<Duration> {
    vuot(tuong, don_dieu, nhip, NGUONG)
}

fn vuot(tuong: Option<Duration>, don_dieu: Duration, nhip: Duration, nguong: Duration) -> Option<Duration> {
    let troi = tuong.map_or(don_dieu, |t| t.max(don_dieu));
    (troi >= nhip + nguong).then(|| troi.saturating_sub(nhip))
}

/// Hai lần đọc máy in cách nhau quá nhịp chừng này = QUAN SÁT BỊ GIÁN ĐOẠN.
pub const NGUONG_GIAN_DOAN: Duration = Duration::from_secs(10);

/// Canh gián đoạn quan sát của MỘT vòng đọc máy in (0.2.7, review Codex): máy
/// tính ngủ / tiến trình bị treo GIỮA hai lần đọc → "đã thấy máy chạy" trước đó
/// và "máy rảnh" sau đó KHÔNG còn nối tiếp (máy in có thể đã bị tắt/bật, in hoá
/// đơn khác…) — người dùng phải coi kết luận "đã in" là chưa chắc. Tự phát hiện
/// ngay ở lần đọc đầu sau khi thức (không chờ luồng `thuc-day` 2 s — tránh
/// kết luận trên mẫu đầu tiên trước khi luồng đó kịp tăng số lần thức).
#[derive(Debug, Default)]
pub struct CanhGianDoan {
    truoc: Option<(SystemTime, Instant)>,
    lan: u64,
    /// Đã từng gián đoạn (dính — bằng chứng cũ không bao giờ "hết cũ").
    pub co: bool,
}

impl CanhGianDoan {
    pub fn moi() -> Self {
        Self { lan: lan_thuc_day(), ..Self::default() }
    }

    /// Gọi ở MỖI lần đọc; `nhip` = nhịp chờ dài nhất giữa hai lần đọc. Trả
    /// `true` nếu lần này VỪA phát hiện gián đoạn.
    pub fn buoc(&mut self, nhip: Duration) -> bool {
        self.buoc_voi((SystemTime::now(), Instant::now()), lan_thuc_day(), nhip)
    }

    fn buoc_voi(&mut self, nay: (SystemTime, Instant), lan: u64, nhip: Duration) -> bool {
        let gian = match self.truoc {
            None => false,
            Some((tuong, don_dieu)) => {
                vuot(nay.0.duration_since(tuong).ok(), nay.1.saturating_duration_since(don_dieu), nhip, NGUONG_GIAN_DOAN)
                    .is_some()
            }
        } || lan != self.lan;
        self.truoc = Some(nay);
        self.lan = lan;
        self.co |= gian;
        gian
    }
}

/// Luồng canh suốt đời app. `khi_thuc(ngu)` chạy trên luồng này ngay sau khi
/// tăng số lần thức — phải nhanh, không chặn.
pub fn khoi_dong(khi_thuc: impl Fn(Duration) + Send + 'static) {
    let da_spawn = std::thread::Builder::new().name("thuc-day".into()).spawn(move || {
        let mut truoc = (SystemTime::now(), Instant::now());
        loop {
            std::thread::sleep(NHIP);
            let nay = (SystemTime::now(), Instant::now());
            let tuong = nay.0.duration_since(truoc.0).ok();
            let don_dieu = nay.1.saturating_duration_since(truoc.1);
            truoc = nay;
            if let Some(ngu) = xet(tuong, don_dieu, NHIP) {
                LAN.fetch_add(1, Ordering::SeqCst);
                khi_thuc(ngu);
            }
        }
    });
    if let Err(e) = da_spawn {
        crate::nhat_ky::ghi("thuc_day_loi", &format!("khong tao duoc luong: {}", e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn nhip_binh_thuong_khong_phai_thuc_day() {
        assert_eq!(xet(Some(s(2)), s(2), NHIP), None);
        assert_eq!(xet(Some(s(10)), s(10), NHIP), None, "máy bận vài giây");
        assert_eq!(xet(Some(s(16)), s(16), NHIP), None, "chưa tới ngưỡng");
    }

    #[test]
    fn canh_gian_doan_giua_hai_lan_doc() {
        let t0 = (SystemTime::now(), Instant::now());
        let sau = |giay: u64| (t0.0 + s(giay), t0.1 + s(giay));
        let mut c = CanhGianDoan::default();
        assert!(!c.buoc_voi(t0, 0, Duration::from_millis(500)), "lần đầu chỉ lấy mốc");
        assert!(!c.buoc_voi(sau(1), 0, Duration::from_millis(500)));
        assert!(!c.buoc_voi(sau(9), 0, Duration::from_millis(500)), "máy bận 8 s chưa tính");
        assert!(c.buoc_voi(sau(30), 0, Duration::from_millis(500)), "ngủ 21 s");
        assert!(c.co);
        assert!(!c.buoc_voi(sau(31), 0, Duration::from_millis(500)));
        assert!(c.co, "dính — bằng chứng cũ không hết cũ");
        // Instant không đếm lúc ngủ: chỉ đồng hồ tường thấy.
        let mut c = CanhGianDoan::default();
        c.buoc_voi(t0, 0, Duration::from_millis(500));
        assert!(c.buoc_voi((t0.0 + s(600), t0.1 + s(1)), 0, Duration::from_millis(500)));
        // Luồng thức-dậy đã đếm một lần thức giữa hai lần đọc.
        let mut c = CanhGianDoan::default();
        c.buoc_voi(t0, 3, Duration::from_millis(500));
        assert!(c.buoc_voi(sau(1), 4, Duration::from_millis(500)));
    }

    #[test]
    fn ngu_nhan_ra_du_instant_co_dem_hay_khong() {
        // Instant KHÔNG đếm thời gian ngủ (chỉ đồng hồ tường thấy).
        assert_eq!(xet(Some(s(3602)), s(2), NHIP), Some(s(3600)));
        // Instant CÓ đếm.
        assert_eq!(xet(Some(s(602)), s(602), NHIP), Some(s(600)));
        // Đồng hồ tường chỉnh LÙI trong lúc ngủ → dựa Instant.
        assert_eq!(xet(None, s(602), NHIP), Some(s(600)));
        assert_eq!(xet(None, s(2), NHIP), None, "chỉnh giờ lùi lúc thức: không phải ngủ");
    }
}
