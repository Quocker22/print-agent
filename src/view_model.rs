// SPDX-License-Identifier: AGPL-3.0-or-later
//! Logic THUẦN map trạng thái/config → dữ liệu hiển thị. Tách khỏi Slint để test
//! không cần render. ui.rs gọi build_view_model rồi bơm vào Slint properties.
use crate::config::Config;
use crate::state::TrangThaiChung;

pub struct JobRow {
    pub so_hoa_don: String,
    pub khach: String,
    pub badge: String,
    pub da_in: bool,
}

pub struct ViewModel {
    pub trang_thai_text: String,
    pub da_noi: bool,
    pub server: String,
    pub may_in: String,
    pub jobs: Vec<JobRow>,
}

pub fn build_view_model(cfg: &Config, t: &TrangThaiChung) -> ViewModel {
    ViewModel {
        trang_thai_text: if t.da_noi { "Đã kết nối".into() } else { "Mất kết nối".into() },
        da_noi: t.da_noi,
        server: cfg.server_url.clone(),
        may_in: format!("{} · {}", cfg.printer_name, cfg.paper_size),
        jobs: t.jobs.iter().map(|j| JobRow {
            so_hoa_don: j.so_hoa_don.clone(),
            khach: j.khach.clone().unwrap_or_default(),
            badge: if j.trang_thai == "da_in" { "Đã in".into() } else { "Lỗi".into() },
            da_in: j.trang_thai == "da_in",
        }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TrangThaiChung, JobLog};
    use crate::config::Config;

    fn cfg() -> Config {
        Config { server_url: "zalocrm.incokit.com".into(), token: "".into(),
                 printer_name: "HP 4003".into(),
                 tray: "tray-1".into(), paper_size: "A5".into() }
    }

    #[test]
    fn da_noi_ra_text_xanh() {
        let t = TrangThaiChung { da_noi: true, jobs: vec![], thong_bao_cuoi: None };
        let vm = build_view_model(&cfg(), &t);
        assert_eq!(vm.trang_thai_text, "Đã kết nối");
        assert!(vm.da_noi);
        assert_eq!(vm.may_in, "HP 4003 · A5");
    }

    #[test]
    fn mat_noi_ra_text_do() {
        let t = TrangThaiChung { da_noi: false, jobs: vec![], thong_bao_cuoi: None };
        assert_eq!(build_view_model(&cfg(), &t).trang_thai_text, "Mất kết nối");
    }

    #[test]
    fn job_map_badge_dung() {
        let t = TrangThaiChung { da_noi: true, thong_bao_cuoi: None, jobs: vec![
            JobLog { so_hoa_don: "INV/1".into(), khach: Some("Anh A".into()), trang_thai: "da_in".into(), luc: "10:00".into() },
            JobLog { so_hoa_don: "INV/2".into(), khach: None, trang_thai: "loi".into(), luc: "10:01".into() },
        ]};
        let vm = build_view_model(&cfg(), &t);
        assert_eq!(vm.jobs.len(), 2);
        assert_eq!(vm.jobs[0].badge, "Đã in"); assert!(vm.jobs[0].da_in);
        assert_eq!(vm.jobs[0].khach, "Anh A");
        assert_eq!(vm.jobs[1].badge, "Lỗi"); assert!(!vm.jobs[1].da_in);
        assert_eq!(vm.jobs[1].khach, "");
    }
}
