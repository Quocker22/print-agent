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
    let troi = tuong.map_or(don_dieu, |t| t.max(don_dieu));
    (troi >= nhip + NGUONG).then(|| troi.saturating_sub(nhip))
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
