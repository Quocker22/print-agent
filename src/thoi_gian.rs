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

/// (năm, tháng, ngày) lịch Gregory → số ngày kể từ 1970-01-01.
fn so_ngay_tu_ngay(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Giây epoch của một chuỗi ISO UTC `YYYY-MM-DDTHH:MM[:SS…]` (server gửi
/// `toISOString()`). Chuỗi lạ → `None`.
pub fn giay_tu_iso(iso: &str) -> Option<i64> {
    let b = iso.as_bytes();
    if b.len() < 16 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let so = |a: usize, z: usize| iso.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi) = (so(0, 4)?, so(5, 7)?, so(8, 10)?, so(11, 13)?, so(14, 16)?);
    let s = if b.len() >= 19 && b[16] == b':' { so(17, 19)? } else { 0 };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(so_ngay_tu_ngay(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + s)
}

/// Giờ hiện của mốc ISO UTC theo giờ máy (`lech_phut` = giờ máy − UTC): "HH:MM"
/// nếu cùng ngày với `bay_gio` (giờ máy), khác ngày thì "DD/MM HH:MM". Chuỗi lạ → "".
pub fn gio_may_tu_iso(iso: &str, lech_phut: i64, bay_gio: SystemTime) -> String {
    let Some(giay) = giay_tu_iso(iso) else { return String::new() };
    let dia_phuong = giay + lech_phut * 60;
    let (_, m, d) = ngay_tu_so_ngay(dia_phuong.div_euclid(86_400));
    let s = dia_phuong.rem_euclid(86_400);
    let hom_nay = (tach(bay_gio).0 + lech_phut * 60).div_euclid(86_400);
    if dia_phuong.div_euclid(86_400) == hom_nay {
        format!("{:02}:{:02}", s / 3_600, (s % 3_600) / 60)
    } else {
        format!("{:02}/{:02} {:02}:{:02}", d, m, s / 3_600, (s % 3_600) / 60)
    }
}

/// Giờ máy lệch UTC bao nhiêu phút (Việt Nam: 420). Đọc lại mỗi lần gọi — máy
/// đổi múi giờ thì giờ hiện đổi theo, không phải khởi động lại app.
#[cfg(windows)]
pub fn lech_gio_may_phut() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    // SAFETY: hai hàm chỉ trả SYSTEMTIME, không có điều kiện trước.
    let (dp, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let phut = |t: &windows::Win32::Foundation::SYSTEMTIME| {
        so_ngay_tu_ngay(t.wYear as i64, t.wMonth as i64, t.wDay as i64) * 1_440 + t.wHour as i64 * 60 + t.wMinute as i64
    };
    // Hai lần đọc có thể vắt qua ranh phút — làm tròn về bội 15 phút (múi giờ nào cũng vậy).
    let lech = phut(&dp) - phut(&utc);
    (lech as f64 / 15.0).round() as i64 * 15
}

/// Ngoài Windows (test / Mac): giờ Việt Nam.
#[cfg(not(windows))]
pub fn lech_gio_may_phut() -> i64 {
    420
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

    #[test]
    fn iso_sang_gio_may() {
        // 2026-09-25T11:45:00Z = 18:45 giờ VN.
        let bay_gio = luc(giay_tu_iso("2026-09-25T12:00:00.000Z").unwrap() as u64);
        assert_eq!(gio_may_tu_iso("2026-09-25T11:45:00.000Z", 420, bay_gio), "18:45");
        // 16:59Z hôm trước (VN 23:59 ngày 24) → khác ngày.
        assert_eq!(gio_may_tu_iso("2026-09-24T16:59:00.000Z", 420, bay_gio), "24/09 23:59");
        // 17:00Z ngày 24 = 00:00 ngày 25 VN → cùng ngày.
        assert_eq!(gio_may_tu_iso("2026-09-24T17:00:00Z", 420, bay_gio), "00:00");
        assert_eq!(gio_may_tu_iso("rác", 420, bay_gio), "");
        assert_eq!(gio_may_tu_iso("2026-13-01T00:00:00Z", 420, bay_gio), "");
        // Khứ hồi với iso_utc.
        let t = luc(1_790_000_000);
        assert_eq!(giay_tu_iso(&iso_utc(t)), Some(1_790_000_000));
        assert_eq!(giay_tu_iso("2000-02-29T00:00:00Z"), Some(951_782_400));
    }
}
