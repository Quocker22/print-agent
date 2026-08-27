// SPDX-License-Identifier: AGPL-3.0-or-later
//! Trạng thái chia sẻ giữa thread net (socket.io) và UI (egui).
//!
//! VÌ SAO Arc<Mutex<..>> chứ không phải channel: UI cần ĐỌC LẠI trạng thái
//! hiện tại mỗi frame (egui vẽ lại ~60fps theo kiểu immediate-mode), không
//! phải "nhận sự kiện một lần" — Mutex đọc nhanh, khoá ngắn (chỉ trong lúc
//! đọc/ghi struct, không giữ khoá qua I/O mạng) nên không đáng lo tranh chấp.

/// Một dòng log job in gần đây, hiển thị trong UI "In gần đây".
#[derive(Debug, Clone)]
pub struct JobLog {
    pub so_hoa_don: String,
    /// Tên khách — server HIỆN KHÔNG gửi tên khách trong payload job (xem
    /// job.rs JobIn), nên luôn None ở bản này. Để None thay vì bịa; nếu sau
    /// này server thêm field tên khách thì mới điền được giá trị thật.
    pub khach: Option<String>,
    /// "da_in" | "loi" — khớp trang_thai trong job::KetQua.
    pub trang_thai: String,
    /// Thời điểm xử lý, định dạng giờ:phút:giây cho gọn UI (không cần chính xác ms).
    pub luc: String,
}

/// Trạng thái chung: thread net ghi, UI đọc mỗi frame.
#[derive(Debug, Clone, Default)]
pub struct TrangThaiChung {
    pub da_noi: bool,
    /// Job gần đây, MỚI NHẤT Ở ĐẦU (index 0). Giới hạn ~20 dòng để UI không phình.
    pub jobs: Vec<JobLog>,
    /// Thông báo lỗi/log ngắn gọn gần nhất (vd "nối thất bại: ...") — hiện ở UI
    /// cho người dùng biết lý do mất kết nối thay vì chỉ thấy chip đỏ mù mờ.
    pub thong_bao_cuoi: Option<String>,
}

/// Số job log tối đa giữ trong bộ nhớ — tránh Vec phình vô hạn khi agent chạy lâu ngày.
pub const MAX_JOB_LOG: usize = 20;

impl TrangThaiChung {
    /// Thêm một job log mới vào đầu danh sách, cắt bớt nếu vượt MAX_JOB_LOG.
    pub fn them_job(&mut self, log: JobLog) {
        self.jobs.insert(0, log);
        self.jobs.truncate(MAX_JOB_LOG);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn them_job_moi_nhat_len_dau() {
        let mut t = TrangThaiChung::default();
        t.them_job(JobLog { so_hoa_don: "1".into(), khach: None, trang_thai: "da_in".into(), luc: "10:00:00".into() });
        t.them_job(JobLog { so_hoa_don: "2".into(), khach: None, trang_thai: "da_in".into(), luc: "10:00:01".into() });
        assert_eq!(t.jobs[0].so_hoa_don, "2");
        assert_eq!(t.jobs[1].so_hoa_don, "1");
    }

    #[test]
    fn them_job_cat_bot_khi_vuot_gioi_han() {
        let mut t = TrangThaiChung::default();
        for i in 0..(MAX_JOB_LOG + 5) {
            t.them_job(JobLog { so_hoa_don: i.to_string(), khach: None, trang_thai: "da_in".into(), luc: "x".into() });
        }
        assert_eq!(t.jobs.len(), MAX_JOB_LOG);
        // mới nhất (index cuối cùng thêm vào) phải ở đầu
        assert_eq!(t.jobs[0].so_hoa_don, (MAX_JOB_LOG + 4).to_string());
    }
}
