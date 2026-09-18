// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kiểm chứng THẬT trên Windows: ghi registry → đọc lại → xoá → đọc lại.
//! Không mock, đụng `HKCU\...\Run` thật rồi trả về nguyên trạng.
//!
//! Vì sao cần bài e2e riêng: `tu_khoi_dong.rs` có unit test cho phần dựng dòng
//! lệnh, nhưng phần ghi/đọc registry chỉ có thể tin khi chạy thật. Bug ở đây
//! hỏng ÂM THẦM — app không lên sau reboot, không ai biết cho tới khi khách gọi
//! (đúng chuyện đã xảy ra với máy HCM 15–18/09).
//!
//! Trên không-Windows bài này tự bỏ qua (`dat`/`dang_bat` là bản giả no-op).

#[path = "../src/tu_khoi_dong.rs"]
mod tu_khoi_dong;

#[cfg(windows)]
#[test]
fn bat_roi_tat_tu_khoi_dong_ghi_va_xoa_duoc_registry_that() {
    // Nhớ trạng thái ban đầu để trả về đúng như cũ sau khi test xong — máy build
    // có thể đang bật thật, không được để test làm đổi cấu hình của máy.
    let ban_dau = tu_khoi_dong::dang_bat();

    tu_khoi_dong::dat(true).expect("bật được");
    assert!(tu_khoi_dong::dang_bat(), "bật xong phải đọc lại thấy đang bật");

    tu_khoi_dong::dat(false).expect("tắt được");
    assert!(!tu_khoi_dong::dang_bat(), "tắt xong phải đọc lại thấy đã tắt");

    // Tắt hai lần liên tiếp KHÔNG được lỗi: xoá mục không tồn tại nghĩa là kết
    // quả mong muốn (không tự chạy) vốn đã đạt được, không phải sự cố.
    tu_khoi_dong::dat(false).expect("tắt lần hai không được lỗi");

    if ban_dau {
        tu_khoi_dong::dat(true).expect("trả về trạng thái ban đầu");
    }
}
