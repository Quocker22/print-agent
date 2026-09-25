// SPDX-License-Identifier: AGPL-3.0-or-later
//! Thời điểm dạng ISO-8601 UTC (`luc` của các event, dòng nhật ký) và ngày UTC
//! (tên file nhật ký).
//!
//! VÌ SAO không thêm chrono/time: chỉ cần ĐỊNH DẠNG một mốc UTC, không cần múi
//! giờ hay phân tích chuỗi — thuật toán lịch `civil_from_days` (Howard Hinnant,
//! cùng họ với `days_from_civil` đang dùng trong spooler.rs) là vài dòng, đúng
//! cho mọi năm, không kéo thêm crate vào exe.
//!
//! VÌ SAO UTC chứ không giờ máy: cùng quy ước với DB backend (`print_jobs`,
//! `print_logs` lưu UTC — handoff §5.2, "cộng 7 ra giờ VN"), tra chéo nhật ký
//! app với nhật ký server không phải quy đổi giờ lệch nhau.

use std::time::{SystemTime, UNIX_EPOCH};

/// Số ngày kể từ 1970-01-01 → (năm, tháng, ngày) lịch Gregory.
fn ngay_tu_so_ngay(so_ngay: i64) -> (i64, u32, u32) {
    let z = so_ngay + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// (giây, mili-giây) kể từ epoch. Đồng hồ máy lùi về trước 1970 (hỏng pin CMOS)
/// thì ra 0 thay vì panic — một mốc sai còn hơn làm chết luồng in.
fn tach(t: SystemTime) -> (i64, u32) {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() as i64, d.subsec_millis())
}

/// "2026-09-24T03:05:01.123Z"
pub fn iso_utc(t: SystemTime) -> String {
    let (giay, ms) = tach(t);
    let (y, m, d) = ngay_tu_so_ngay(giay.div_euclid(86_400));
    let s = giay.rem_euclid(86_400);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, s / 3_600, (s % 3_600) / 60, s % 60, ms)
}

/// "2026-09-24" — ngày UTC của `t`.
pub fn ngay_utc(t: SystemTime) -> String {
    let (giay, _) = tach(t);
    let (y, m, d) = ngay_tu_so_ngay(giay.div_euclid(86_400));
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Ngày UTC cách `t` về trước `so_ngay` ngày.
pub fn ngay_utc_truoc(t: SystemTime, so_ngay: u64) -> String {
    let lui = std::time::Duration::from_secs(so_ngay * 86_400);
    ngay_utc(t.checked_sub(lui).unwrap_or(UNIX_EPOCH))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn luc(giay: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(giay)
    }

    #[test]
    fn iso_utc_dung_voi_moc_da_biet() {
        assert_eq!(iso_utc(luc(0)), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_utc(luc(1_727_170_000)), "2024-09-24T09:26:40.000Z");
        assert_eq!(iso_utc(luc(4_102_444_799)), "2099-12-31T23:59:59.000Z");
        assert_eq!(iso_utc(luc(1_790_000_000) + Duration::from_millis(7)), "2026-09-21T14:13:20.007Z");
    }

    #[test]
    fn ngay_nhuan_dung() {
        assert_eq!(ngay_utc(luc(951_782_400)), "2000-02-29");
        assert_eq!(ngay_utc(luc(1_709_164_800)), "2024-02-29");
    }

    #[test]
    fn ngay_truoc_14_ngay() {
        // 2026-09-24 12:00 UTC lùi 14 ngày → 2026-09-10
        let t = luc(1_790_000_000) + Duration::from_secs(3 * 86_400 - 14 * 3_600 - 13 * 60 - 20 + 12 * 3_600);
        assert_eq!(ngay_utc(t), "2026-09-24");
        assert_eq!(ngay_utc_truoc(t, 14), "2026-09-10");
    }

    #[test]
    fn truoc_1970_khong_panic() {
        assert_eq!(ngay_utc(UNIX_EPOCH - Duration::from_secs(10)), "1970-01-01");
        assert_eq!(ngay_utc_truoc(luc(5), 14), "1970-01-01");
    }
}
