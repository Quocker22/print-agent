// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cửa sổ Slint (trạng thái + cấu hình) và tray icon (khay hệ thống).
//! Viết lại từ bản egui/eframe cũ (xem git log ui.rs trước task-5) — GIỮ
//! NGUYÊN mọi hành vi/bài học prod, chỉ đổi framework UI.
//!
//! VÌ SAO gộp UI + tray trong 1 module: cả hai đều PHẢI chạy trên MAIN THREAD
//! (doc-comment đầu crate tray-icon, xem bản egui cũ) — Slint event loop cũng
//! chạy trên main thread (chay_ui() được gọi thẳng từ main(), không spawn
//! thread riêng), nên dựng tray-icon ngay trong chay_ui() trước khi chạy event
//! loop là đúng thời điểm + đúng thread.
//!
//! 6 BÀI HỌC TỪ BẢN EGUI CŨ (task-5-context.md) — giữ nguyên ý, đổi cách làm:
//!
//! 1. Tray tạo trên CÙNG thread với event loop: dựng TrayIcon trong chay_ui()
//!    trước `window.show()`/`slint::run_event_loop_until_quit()`.
//! 2. Icon xanh/đỏ chỉ set khi trạng thái ĐỔI: giữ y icon_mau/icon_xanh/icon_do,
//!    so sánh với biến `tray_da_noi_hien_thi` trước khi gọi set_icon.
//! 3. Menu id string tường minh (MenuItem::with_id), so event.id == hằng &str.
//! 4. Poll tray/menu event trong 1 Timer::start(Repeated, ~300ms) — THAY cho
//!    nguon_repaint_nen (thread nền gọi ctx.request_repaint() của egui). Slint
//!    Timer chạy trong event loop, kể cả khi window đang ẩn (event loop vẫn
//!    sống — không có "ai đánh thức ai" như egui vì ta không dựa vào window
//!    event để wake, Timer tự có nhịp riêng độc lập với visibility).
//! 5. Menu "Cấu hình..." → window.show(); X (đóng) → window.hide() (không
//!    thoát) qua on_close_requested trả CloseRequestResponse::HideWindow;
//!    "Thoát" → slint::quit_event_loop() + std::process::exit(0).
//! 6. Khởi động ẩn: MainWindow không show() ngay. NGOẠI LỆ kỹ thuật: cần HWND
//!    thật để gọi taskbar_win::an_khoi_taskbar, mà Slint (xem thảo luận chính
//!    thức slint-ui/slint#5319, #3266) chỉ cấp window_handle() SAU KHI window
//!    đã được window manager tạo — tức sau show() + ít nhất 1 vòng event loop.
//!    Giải pháp CHUẨN (không có cách khác trong API công khai Slint 1.17):
//!    show() → Timer::single_shot(0ms) chạy NGAY vòng lặp kế tiếp → lấy HWND,
//!    gọi an_khoi_taskbar, rồi hide() ngay. Cửa sổ có thể nháy 1 frame cực
//!    ngắn (thường dưới ngưỡng mắt người nhận ra ở đa số máy) — đây là đánh
//!    đổi kỹ thuật CHẤP NHẬN ĐƯỢC để lấy HWND thật, đã ghi trong report.
//!
//! Software renderer: `renderer-software` là default feature của crate slint
//! (xem Cargo.toml + doc-comment ở đó) — ép chọn tường minh bằng
//! BackendSelector ngay đầu chay_ui(), TRƯỚC khi tạo MainWindow.

use crate::config::{self, Config};
use crate::hop_thu_di::DuongGui;
use crate::job;
use crate::net::{self, DieuKhienNet};
use crate::nhat_ky;
use crate::printing;
use crate::state::TrangThaiChung;
use crate::taskbar_win::{an_khoi_taskbar, nhay_cua_so};
use crate::view_model::{build_view_model, nen_bat_cua_so, CanhBao};
use raw_window_handle::HasWindowHandle;
use slint::{CloseRequestResponse, ModelRc, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

slint::include_modules!();

/// Id string của 2 mục menu tray BẤM ĐƯỢC — giữ nguyên từ bản egui cũ.
const MENU_ID_CAU_HINH: &str = "cauhinh";
const MENU_ID_THOAT: &str = "thoat";

/// Đường dẫn config.ini để GHI lúc bấm Lưu — phải CẠNH exe (khớp chỗ main.rs
/// đọc). Double-click từ Explorer có cwd khác nơi để exe, nên "config.ini"
/// tương đối sẽ ghi lạc chỗ rồi lần sau đọc không thấy. Fallback tương đối nếu
/// không lấy được đường dẫn exe.
fn config_path() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("config.ini");
        }
    }
    std::path::PathBuf::from("config.ini")
}

/// Vẽ icon HÌNH MÁY IN 32x32 (nền trong suốt), tô theo màu trạng thái `(r,g,b)`
/// — xanh = đã nối, đỏ = mất kết nối. Thay ô vuông đặc 1 màu của bản egui cũ
/// (anh Quốc: "icon xấu quá"). Tự dựng buffer RGBA trong code (không include_bytes!
/// PNG → không bước decode nào lỗi được, giữ ưu điểm bản cũ).
///
/// Hình: thân máy in (chữ nhật bo nhẹ) + khe giấy phía trên + tờ giấy trắng nhô
/// ra khỏi khe + 1 chấm đèn nhỏ. Toàn bộ nét vẽ dùng màu trạng thái để icon khay
/// nhìn là thấy ngay xanh/đỏ; giấy để trắng cho tương phản.
fn icon_may_in(r: u8, g: u8, b: u8) -> Icon {
    icon_may_in_ve(r, g, b, false)
}

/// Như `icon_may_in`; `cham_than` = vẽ dấu "!" trắng trên thân (icon sự cố).
fn icon_may_in_ve(r: u8, g: u8, b: u8, cham_than: bool) -> Icon {
    const N: u32 = 32;
    let mau = image::Rgba([r, g, b, 255]);
    let trang = image::Rgba([255, 255, 255, 255]);
    let trong = image::Rgba([0, 0, 0, 0]);
    let mut img = image::RgbaImage::from_pixel(N, N, trong);

    let mut set = |x: i32, y: i32, px: image::Rgba<u8>| {
        if (0..N as i32).contains(&x) && (0..N as i32).contains(&y) {
            img.put_pixel(x as u32, y as u32, px);
        }
    };
    let fill = |set: &mut dyn FnMut(i32, i32, image::Rgba<u8>),
                x0: i32, y0: i32, x1: i32, y1: i32, px: image::Rgba<u8>| {
        for y in y0..=y1 {
            for x in x0..=x1 {
                set(x, y, px);
            }
        }
    };

    // Thân máy in: chữ nhật đặc (bo góc bằng cách chừa 4 pixel góc).
    fill(&mut set, 5, 13, 26, 24, mau);
    for &(cx, cy) in &[(5, 13), (26, 13), (5, 24), (26, 24)] {
        set(cx, cy, trong); // bo 4 góc thân
    }
    // Tờ giấy TRÊN (đầu vào) — nhô lên khỏi thân, để trắng.
    fill(&mut set, 9, 7, 22, 12, trang);
    // Viền giấy trên bằng màu trạng thái cho rõ nét trên nền sáng.
    fill(&mut set, 9, 7, 22, 7, mau);
    fill(&mut set, 9, 7, 9, 12, mau);
    fill(&mut set, 22, 7, 22, 12, mau);
    // Tờ giấy RA (đầu ra) — nhô xuống dưới thân, để trắng có viền.
    fill(&mut set, 10, 24, 21, 29, trang);
    fill(&mut set, 10, 29, 21, 29, mau);
    fill(&mut set, 10, 24, 10, 29, mau);
    fill(&mut set, 21, 24, 21, 29, mau);
    // Vài dòng "chữ" trên tờ giấy ra (nét màu) — gợi hình đơn in.
    fill(&mut set, 12, 26, 19, 26, mau);
    fill(&mut set, 12, 28, 17, 28, mau);
    // Chấm đèn nguồn trên thân (trắng) để icon sinh động.
    fill(&mut set, 23, 15, 24, 16, trang);
    if cham_than {
        // "!" giữa thân: vạch dọc + chấm.
        fill(&mut set, 15, 14, 16, 19, trang);
        fill(&mut set, 15, 21, 16, 22, trang);
    }

    Icon::from_rgba(img.into_raw(), N, N).expect("icon RGBA hợp lệ (buffer đúng NxN*4 byte)")
}

fn icon_xanh() -> Icon {
    icon_may_in(0x2e, 0xa0, 0x4a) // xanh lá — đã nối server
}

fn icon_do() -> Icon {
    icon_may_in(0xc0, 0x39, 0x2b) // đỏ — mất kết nối
}

/// Cam + dấu "!" — máy in có sự cố. KHÁC màu đỏ "mất kết nối" để NV không
/// nhầm hai chuyện (sự cố thì phải ra máy in, mất kết nối thì không).
fn icon_canh_bao() -> Icon {
    icon_may_in_ve(0xe6, 0x7e, 0x22, true)
}

/// Danh sách khay cố định cho ComboBox "Khay" — khớp printing::tray_sang_bin
/// (tray-1..tray-4 là các bin hợp lệ driver thường có).
const DANH_SACH_KHAY: &[&str] = &["tray-1", "tray-2", "tray-3", "tray-4"];

/// Liệt kê tên máy in đã cài trên máy (Windows) để đổ vào ComboBox "Máy in" —
/// NV chọn thay vì gõ tay (gõ sai 1 ký tự là in fail; tên PHẢI khớp Get-Printer
/// vì lệnh in dùng đúng tên đó). Dùng `Get-Printer` qua PowerShell: cùng nguồn
/// tên với đường in thật, không cần thêm Win32 EnumPrinters API.
///
/// Trả Vec rỗng nếu chạy được lệnh nhưng không có máy in, hoặc nếu gọi lỗi
/// (không phải Windows / PowerShell không có) — UI sẽ ghép giá trị config hiện
/// tại vào để không mất cấu hình cũ.
#[cfg(windows)]
fn liet_ke_may_in() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000; // đừng nháy cửa sổ console
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-Printer | Select-Object -ExpandProperty Name",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(not(windows))]
fn liet_ke_may_in() -> Vec<String> {
    Vec::new()
}

/// Ghép giá trị `hien_tai` (từ config) vào đầu `ds` nếu chưa có — để ComboBox
/// luôn hiển thị đúng cấu hình đang lưu kể cả khi máy in đó không còn trong
/// danh sách liệt kê (đã tháo, đổi tên...). Trả về ModelRc để set vào .slint.
fn model_co_gia_tri_hien_tai(ds: &[String]) -> ModelRc<slint::SharedString> {
    let hang: Vec<slint::SharedString> = ds.iter().map(|s| s.as_str().into()).collect();
    ModelRc::new(VecModel::from(hang))
}

/// Danh sách cho ComboBox + VỊ TRÍ của giá trị cấu hình trong đó (0.2.3). Giá
/// trị chưa có trong danh sách (máy in đã tháo/đổi tên) được ghép vào đầu.
/// VÌ SAO cần vị trí: ComboBox chỉ gắn `current-value` thì tự nhảy về mục đầu
/// (vd "Microsoft XPS Document Writer") — NV bấm Lưu là đổi máy in (HCM 25/09:
/// app nối lên với máy XPS lúc 15:19).
fn ds_va_vi_tri(mut ds: Vec<String>, hien_tai: &str) -> (Vec<String>, i32) {
    if !hien_tai.is_empty() && !ds.iter().any(|s| s == hien_tai) {
        ds.insert(0, hien_tai.to_string());
    }
    let vi_tri = ds.iter().position(|s| s == hien_tai).unwrap_or(0) as i32;
    (ds, vi_tri)
}

/// Giá trị đang chọn: theo VỊ TRÍ trong danh sách Rust giữ; vị trí hỏng thì lấy
/// chữ ComboBox đang hiện.
fn gia_tri_chon(ds: &[String], vi_tri: i32, chu_hien: &str) -> String {
    usize::try_from(vi_tri).ok().and_then(|i| ds.get(i)).cloned().unwrap_or_else(|| chu_hien.trim().to_string())
}

/// Trạng thái tray-icon (icon/menu) — tách khỏi state chung vì chỉ dùng trong
/// vòng lặp UI (main thread), không chia sẻ với thread net.
struct Tray {
    tray_icon: Option<TrayIcon>,
    /// (đã nối, tiêu đề cảnh báo đang báo, pha nháy) đang hiển thị — tránh
    /// set_icon() mỗi tick (bài học #2); chỉ gọi khi bộ ba này đổi.
    hien_thi: Option<(bool, Option<String>, bool)>,
    /// Dựng sẵn một lần: lúc nháy set_icon ~0,6 s/lần, không dựng lại RGBA +
    /// HICON mỗi lần (Icon clone chỉ tăng Arc — tray-icon tự DestroyIcon bản cũ).
    icon_xanh: Icon,
    icon_mat_noi: Icon,
    icon_su_co: Icon,
    menu_trang_thai: Option<MenuItem>,
    menu_server: Option<MenuItem>,
    menu_may_in: Option<MenuItem>,
}

impl Tray {
    fn moi(cfg: &Config) -> Self {
        // 3 mục ĐẦU: info-only (enabled=false) — hiện trạng thái/server/máy in
        // ngay trong menu tray không cần mở cửa sổ. Giữ nguyên bản egui cũ.
        let menu_trang_thai = MenuItem::new("● Mất kết nối", false, None);
        let menu_server = MenuItem::new(format!("Server: {}", cfg.server_url), false, None);
        let menu_may_in = MenuItem::new(
            format!("Máy in: {} ({}, khay {})", cfg.printer_name, cfg.paper_size, cfg.tray),
            false,
            None,
        );
        let menu_cau_hinh = MenuItem::with_id(MENU_ID_CAU_HINH, "Cấu hình...", true, None);
        let menu_thoat = MenuItem::with_id(MENU_ID_THOAT, "Thoát", true, None);

        let tray_menu = Menu::new();
        let _ = tray_menu.append(&menu_trang_thai);
        let _ = tray_menu.append(&menu_server);
        let _ = tray_menu.append(&menu_may_in);
        let _ = tray_menu.append(&PredefinedMenuItem::separator());
        let _ = tray_menu.append(&menu_cau_hinh);
        let _ = tray_menu.append(&menu_thoat);

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("Incokit Print Agent — mất kết nối")
            .with_icon(icon_do())
            .with_menu_on_left_click(true)
            .build()
            .ok(); // None nếu môi trường không hỗ trợ tray — không chặn app chạy.

        Self {
            tray_icon,
            hien_thi: None,
            icon_xanh: icon_xanh(),
            icon_mat_noi: icon_do(),
            icon_su_co: icon_canh_bao(),
            menu_trang_thai: Some(menu_trang_thai),
            menu_server: Some(menu_server),
            menu_may_in: Some(menu_may_in),
        }
    }

    /// Cập nhật icon + mục trạng thái trong menu — chỉ khi trạng thái ĐỔI
    /// (bài học #2, giữ nguyên logic bản egui cũ).
    ///
    /// Có sự cố mức lỗi (hợp đồng v2 §4.4): icon NHÁY giữa icon sự cố và icon
    /// kết nối theo `pha_nhay`; tooltip + dòng trạng thái trong menu nêu sự cố.
    /// Cảnh báo vàng (mực yếu) chỉ đổi chữ, không nháy.
    fn cap_nhat(&mut self, da_noi: bool, canh_bao: Option<&CanhBao>, pha_nhay: bool) {
        let nhay = pha_nhay && canh_bao.is_some_and(|c| c.loi);
        let khoa_cb = canh_bao.map(|c| c.tieu_de.clone());
        if self.hien_thi.as_ref().is_some_and(|(d, k, n)| *d == da_noi && *k == khoa_cb && *n == nhay) {
            return;
        }
        let doi_chu = self.hien_thi.as_ref().map(|(d, k, _)| (*d, k.clone())) != Some((da_noi, khoa_cb.clone()));
        self.hien_thi = Some((da_noi, khoa_cb, nhay));
        let ket_noi = if da_noi { "đã kết nối" } else { "mất kết nối" };
        if let Some(tray) = &self.tray_icon {
            let icon = if nhay {
                &self.icon_su_co
            } else if da_noi {
                &self.icon_xanh
            } else {
                &self.icon_mat_noi
            };
            let _ = tray.set_icon(Some(icon.clone()));
            if doi_chu {
                let tooltip = match canh_bao {
                    Some(cb) => format!("Incokit Print Agent — {} — {}", ket_noi, cb.tieu_de),
                    None => format!("Incokit Print Agent — {}", ket_noi),
                };
                let _ = tray.set_tooltip(Some(tooltip));
            }
        }
        if doi_chu {
            if let Some(mi) = &self.menu_trang_thai {
                let nhan = if da_noi { "● Đã kết nối" } else { "● Mất kết nối" };
                match canh_bao {
                    Some(cb) => mi.set_text(format!("{} — {}", nhan, cb.tieu_de)),
                    None => mi.set_text(nhan),
                }
            }
        }
    }

    /// Cập nhật 2 mục info-only "Server:"/"Máy in:" sau khi Lưu cấu hình mới
    /// — giữ nguyên hành vi bản egui cũ (nhánh cfg_moi trong update()).
    fn cap_nhat_cfg(&mut self, cfg: &Config) {
        if let Some(mi) = &self.menu_server {
            mi.set_text(format!("Server: {}", cfg.server_url));
        }
        if let Some(mi) = &self.menu_may_in {
            mi.set_text(format!("Máy in: {} ({}, khay {})", cfg.printer_name, cfg.paper_size, cfg.tray));
        }
        // Ép vẽ lại icon + text trạng thái theo config mới (thread net mới
        // chưa kịp báo da_noi=true/false) — giữ nguyên bản egui cũ.
        self.hien_thi = None;
    }
}

/// Map view_model::JobRow (logic thuần, đã test) → ui::JobRow (struct Slint
/// sinh ra từ .slint) — CHỈ đổi kiểu String→SharedString qua .into(), không
/// tự map lại logic hiển thị (đúng ràng buộc: dùng build_view_model).
fn jobs_sang_model(jobs: Vec<crate::view_model::JobRow>) -> ModelRc<JobRow> {
    let hang: Vec<JobRow> = jobs
        .into_iter()
        .map(|j| JobRow {
            so_hoa_don: j.so_hoa_don.into(),
            khach: j.khach.into(),
            badge: j.badge.into(),
            da_in: j.da_in,
            khong_ro: j.khong_ro,
            dang_xu_ly: j.dang_xu_ly,
            luc: j.luc.into(),
        })
        .collect();
    ModelRc::new(VecModel::from(hang))
}

/// Bơm ViewModel (build_view_model) vào properties MainWindow — điểm DUY NHẤT
/// map trạng thái → hiển thị, gọi từ Timer mỗi tick và ngay sau khi Lưu.
/// Trả cảnh báo đang hiện để tray/nháy cửa sổ dùng chung đúng một nguồn.
fn bom_view_model(w: &MainWindow, cfg: &Config, t: &TrangThaiChung) -> Option<CanhBao> {
    let vm = build_view_model(cfg, t);
    w.set_da_noi(vm.da_noi);
    w.set_trang_thai_text(vm.trang_thai_text.into());
    w.set_server(vm.server.into());
    w.set_may_in(vm.may_in.into());
    w.set_jobs(jobs_sang_model(vm.jobs));
    w.set_canh_bao_hien(vm.canh_bao.is_some());
    if let Some(cb) = &vm.canh_bao {
        w.set_canh_bao_loi(cb.loi);
        // Font nhúng (Be Vietnam Pro) không có glyph ⚠ — cửa sổ đã vẽ dấu "!"
        // bằng hình tròn, nên bỏ ký tự này ở đây (khay/tooltip giữ nguyên).
        w.set_canh_bao_tieu_de(cb.tieu_de.trim_start_matches('⚠').trim_start().into());
        w.set_canh_bao_chi_tiet(cb.chi_tiet.as_str().into());
    }
    w.set_thong_bao_phu(vm.thong_bao_phu.unwrap_or_default().into());
    vm.canh_bao
}

/// HWND thật của cửa sổ (chỉ có SAU khi window manager tạo cửa sổ — xem bài
/// học #6). `None` trên máy không phải Windows.
fn hwnd_cua(w: &MainWindow) -> Option<isize> {
    let handle = w.window().window_handle();
    let handle = handle.window_handle().ok()?;
    match handle.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Chạy UI Slint + tray icon. Gọi từ main() SAU KHI đã spawn thread net
/// (`net_dang_chay` = điều khiển của nó, None khi chưa có config hợp lệ).
/// Cửa sổ khởi động ẨN (mô hình tray-first) — xem bài học #6 ở doc-comment
/// đầu file cho lý do show() ngắn 1 nhịp để lấy HWND rồi hide() ngay.
pub fn chay_ui(
    cfg: Arc<Config>,
    trang_thai: Arc<Mutex<TrangThaiChung>>,
    duong_gui: Arc<DuongGui>,
    net_dang_chay: Option<DieuKhienNet>,
) -> Result<(), slint::PlatformError> {
    // Ép software renderer TRƯỚC khi tạo MainWindow — máy shop GPU ảo VMware,
    // GPU render sẽ chết như egui/wgpu cũ (xem context: "Software renderer
    // BẮT BUỘC — gốc bệnh egui"). renderer-software là default feature crate
    // slint (Cargo.toml) nên chỉ cần CHỌN nó tường minh thay vì tự dò backend.
    slint::BackendSelector::new()
        .renderer_name("software".to_string())
        .select()
        .map_err(|e| eprintln!("[print-agent] không ép được software renderer: {e} — dùng mặc định"))
        .ok();

    let window = MainWindow::new()?;

    // cfg hiện đang dùng — RefCell vì chỉ đụng trên main thread (Timer +
    // callback Slint đều chạy main thread), đổi được khi bấm Lưu. Thread net
    // CŨ thì DỪNG HẲN qua `net_dang_chay` (R7a) — bản trước để nó chạy tiếp:
    // hai kết nối cùng token, hai worker in.
    //
    // `trang_thai` thì GIỮ NGUYÊN một Arc suốt đời app (T7, giám sát vòng 3):
    // bản trước dựng Mutex MỚI mỗi lần Lưu — kết quả của job worker cũ đang in
    // dở, `xac_nhan_in_sau`/`mat_dau` của luồng theo dõi tiếp (danh sách sống
    // qua Lưu) rơi vào trạng thái cũ không ai hiện: NV không thấy dải, "In gần
    // đây" mất dòng. Lưu chỉ đổi cấu hình + quên kết nối/máy in cũ
    // (`TrangThaiChung::doi_cau_hinh`).
    let cfg_dang_dung: Rc<RefCell<Arc<Config>>> = Rc::new(RefCell::new(cfg.clone()));
    let net_dang_chay: Rc<RefCell<Option<DieuKhienNet>>> = Rc::new(RefCell::new(net_dang_chay));

    // Bơm giá trị ban đầu vào form Cấu hình — token giờ dán tay từ trang
    // ZaloCRM (gen 1 token riêng mỗi máy), KHÔNG còn hằng nhúng lúc build,
    // nên form có field f_token đọc/ghi thẳng vào Config.token.
    window.set_f_server(cfg.server_url.clone().into());
    window.set_f_token(cfg.token.clone().into());
    // 2 ComboBox: đổ danh sách TRƯỚC (máy in thật từ Get-Printer, khay cố định),
    // ghép giá trị config hiện tại vào nếu thiếu để không mất cấu hình cũ, RỒI
    // mới set current-value = giá trị config (ComboBox current-value <=> f_*).
    let (ds_may_in, i_may_in) = ds_va_vi_tri(liet_ke_may_in(), &cfg.printer_name);
    let (ds_khay, i_khay) = ds_va_vi_tri(DANH_SACH_KHAY.iter().map(|s| s.to_string()).collect(), &cfg.tray);
    window.set_ds_may_in(model_co_gia_tri_hien_tai(&ds_may_in));
    window.set_ds_khay(model_co_gia_tri_hien_tai(&ds_khay));
    // Thứ tự: danh sách → vị trí → chữ (ComboBox tự đặt chữ theo vị trí khi nạp).
    window.set_i_may_in(i_may_in);
    window.set_i_khay(i_khay);
    window.set_f_may_in(cfg.printer_name.clone().into());
    window.set_f_tray(cfg.tray.clone().into());
    let ds_may_in = Rc::new(ds_may_in);
    let ds_khay = Rc::new(ds_khay);
    // Đọc registry để ô tick hiện ĐÚNG CHIỀU thực tế, không mặc định false —
    // hiển thị sai làm người dùng tưởng chưa bật rồi bấm tắt mất (xem
    // tu_khoi_dong::dang_bat, nó còn đối chiếu đúng đường dẫn exe đang chạy).
    window.set_f_tu_khoi_dong(crate::tu_khoi_dong::dang_bat());
    // Mutex hỏng (một luồng panic lúc giữ khoá) vẫn đọc tiếp được dữ liệu —
    // giao diện KHÔNG được chết theo (R11e).
    bom_view_model(&window, &cfg, &trang_thai.lock().unwrap_or_else(|p| p.into_inner()));

    // Tray phải dựng trên CÙNG thread + TRƯỚC khi event loop chạy (bài học #1).
    let tray = Rc::new(RefCell::new(Tray::moi(&cfg)));

    // Bài học #5: X đóng → ẨN, không thoát. weak handle tránh cycle Rc (window
    // giữ callback, callback không được giữ ngược window bằng strong ref).
    {
        let w_weak = window.as_weak();
        window.window().on_close_requested(move || {
            if let Some(w) = w_weak.upgrade() {
                let _ = w.hide();
            }
            CloseRequestResponse::HideWindow
        });
    }

    // Tự khởi động cùng Windows — áp dụng NGAY khi bấm, không chờ nút Lưu.
    //
    // VÌ SAO CÓ NÚT NÀY: máy in HCM im lặng hơn 3 ngày (15–18/09) chỉ vì không
    // ai bật lại app sau khi tắt máy. Bot vẫn nhận lệnh in, vẫn nói "đã xếp
    // hàng in", job chết lặng sau 5 phút, người phát hiện đầu tiên là KHÁCH.
    //
    // Ghi lỗi ra UI thay vì nuốt lặng: bấm nút mà không thấy gì đổi thì người
    // dùng tưởng đã xong, và lần sau máy vẫn không tự lên.
    {
        let w_weak = window.as_weak();
        window.on_doi_tu_khoi_dong(move |bat| {
            let Some(w) = w_weak.upgrade() else { return };
            match crate::tu_khoi_dong::dat(bat) {
                Ok(()) => {
                    w.set_loi_tu_khoi_dong(Default::default());
                    // Đọc lại registry thay vì tin giá trị vừa ghi — nguồn sự
                    // thật là registry, không phải ý định của ta.
                    w.set_f_tu_khoi_dong(crate::tu_khoi_dong::dang_bat());
                }
                Err(e) => {
                    eprintln!("[print-agent] đặt tự khởi động thất bại: {:#}", e);
                    w.set_loi_tu_khoi_dong(format!("Không đặt được: {}", e).into());
                    // Trả ô tick về trạng thái THẬT, không để nó hiện đã bật
                    // trong khi registry chưa ghi được.
                    w.set_f_tu_khoi_dong(crate::tu_khoi_dong::dang_bat());
                }
            }
        });
    }

    // Bài học #4 (on_luu): đọc form → dựng Config mới (token đọc THẲNG từ
    // form f_token — mỗi máy dán token riêng gen từ trang ZaloCRM, không còn
    // hằng nhúng lúc build) → ghi config.ini → DỪNG HẲN thread net cũ rồi
    // chạy thread net MỚI trên CÙNG trạng thái (R7a/T7 — việc chờ nằm trên
    // thread net mới, giao diện không bị chặn) → cập nhật tray.
    {
        let w_weak = window.as_weak();
        let cfg_dang_dung = cfg_dang_dung.clone();
        let trang_thai = trang_thai.clone();
        let tray = tray.clone();
        let net_dang_chay = net_dang_chay.clone();
        let duong_gui = duong_gui.clone();
        let ds_may_in = ds_may_in.clone();
        let ds_khay = ds_khay.clone();
        window.on_luu(move || {
            let Some(w) = w_weak.upgrade() else { return };

            let cfg_moi = Config {
                server_url: w.get_f_server().trim().to_string(),
                token: w.get_f_token().trim().to_string(),
                printer_name: gia_tri_chon(&ds_may_in, w.get_i_may_in(), &w.get_f_may_in()),
                tray: gia_tri_chon(&ds_khay, w.get_i_khay(), &w.get_f_tray()),
                paper_size: cfg_dang_dung.borrow().paper_size.clone(),
            };

            if cfg_moi.server_url.is_empty()
                || cfg_moi.token.is_empty()
                || cfg_moi.printer_name.is_empty()
            {
                // Giữ nguyên thông báo bản egui cũ (validate field bắt buộc).
                w.set_trang_thai_text(
                    "Thiếu field bắt buộc (server_url/token/máy in)".into(),
                );
                return;
            }

            match std::fs::write(config_path(), config::ghi_config(&cfg_moi)) {
                Ok(()) => {
                    let cfg_moi = Arc::new(cfg_moi);
                    let cu = net_dang_chay.borrow_mut().take();
                    // `khoi_chay` bật cờ dừng của bản cũ NGAY (đồng bộ) — từ
                    // đây bản cũ thôi ghi trạng thái kết nối — rồi mới đặt lại.
                    *net_dang_chay.borrow_mut() =
                        Some(net::khoi_chay(cfg_moi.clone(), trang_thai.clone(), duong_gui.clone(), cu));
                    let mut t = trang_thai.lock().unwrap_or_else(|p| p.into_inner());
                    t.doi_cau_hinh();
                    tray.borrow_mut().cap_nhat_cfg(&cfg_moi);
                    *cfg_dang_dung.borrow_mut() = cfg_moi.clone();
                    bom_view_model(&w, &cfg_moi, &t);
                }
                Err(e) => {
                    w.set_trang_thai_text(format!("Ghi config.ini lỗi: {}", e).into());
                }
            }
        });
    }

    // Bài học #4 (on_in_thu): in_pdf với PDF 1 trang HỢP LỆ (R11d — bản cũ
    // PDF_GIA không phải PDF, Sumatra từ chối), tôn trọng AGENT_DRY_RUN.
    // Chạy trên LUỒNG RIÊNG (R11d): Sumatra + theo dõi spooler tới ~15 s, chạy
    // trên luồng giao diện là cửa sổ đơ, NV tưởng app treo rồi tắt đi.
    {
        let cfg_dang_dung = cfg_dang_dung.clone();
        let dang_in_thu = Arc::new(AtomicBool::new(false));
        window.on_in_thu(move || {
            // Bấm liền nhiều lần chỉ in một bản.
            if dang_in_thu.swap(true, Ordering::SeqCst) {
                return;
            }
            let cfg = cfg_dang_dung.borrow().clone();
            let co = dang_in_thu.clone();
            let da_spawn = std::thread::Builder::new().name("in-thu".into()).spawn(move || {
                // "in-thu" (test) không phải job thật từ server nên không có
                // job_id — dùng id tạm chỉ để spooler.rs có chuỗi khớp
                // document name khi poll.
                let kq = printing::in_pdf(
                    &printing::pdf_in_thu(), &cfg.printer_name, &cfg.paper_size, &cfg.tray, 1, "in-thu", None, None, &|_| {},
                );
                let chu = match &kq {
                    job::KetQuaIn::DaIn => "da_in".to_string(),
                    job::KetQuaIn::Loi(e) => format!("loi {}", e),
                    job::KetQuaIn::KhongRo(ly_do) => format!("khong_ro {}", ly_do),
                };
                eprintln!("[print-agent] in thử: {}", chu);
                nhat_ky::ghi("in_thu", &format!("may_in={} {}", cfg.printer_name, chu));
                co.store(false, Ordering::SeqCst);
            });
            if da_spawn.is_err() {
                dang_in_thu.store(false, Ordering::SeqCst);
            }
        });
    }

    // Nút "Nhật ký" (0.2.3, chủ yêu cầu): mở file nhật ký HÔM NAY bằng Notepad —
    // NV/kỹ thuật không phải gõ lệnh PowerShell. Chưa có file (chưa có dòng nào
    // hôm nay) thì mở thư mục nhật ký. Chỉ mở để ĐỌC, không đụng nội dung.
    window.on_mo_nhat_ky(move || {
        let ket_qua = match nhat_ky::file_hom_nay() {
            Some(f) if f.exists() => std::process::Command::new("notepad.exe").arg(&f).spawn().map(|_| ()),
            _ => match nhat_ky::thu_muc_nhat_ky() {
                Some(d) => {
                    let _ = std::fs::create_dir_all(&d);
                    std::process::Command::new("explorer.exe").arg(&d).spawn().map(|_| ())
                }
                None => Ok(()),
            },
        };
        if let Err(e) = ket_qua {
            eprintln!("[print-agent] không mở được nhật ký: {}", e);
        }
    });

    // Nút "Đã hiểu" trên dải cảnh báo (R1): NV đã đọc — tắt dải hoá đơn, ẩn
    // dải sự cố máy in tới khi trạng thái máy in đổi.
    {
        let w_weak = window.as_weak();
        let cfg_dang_dung = cfg_dang_dung.clone();
        let trang_thai = trang_thai.clone();
        window.on_da_hieu(move || {
            let Some(w) = w_weak.upgrade() else { return };
            let mut t = trang_thai.lock().unwrap_or_else(|p| p.into_inner());
            // Tắt DẢI ĐANG HIỆN (T6) — dải kế tiếp (nếu có) hiện lên.
            t.da_hieu();
            bom_view_model(&w, &cfg_dang_dung.borrow(), &t);
        });
    }

    // Bài học #6 (tiếp): single_shot RIÊNG, TÁCH khỏi timer polling 300ms bên
    // dưới — nếu gộp chung vào tick đầu của timer 300ms thì cửa sổ sẽ nháy ở
    // taskbar tới 300ms (khoảng thời gian window có HWND thật nhưng CHƯA kịp
    // ẩn taskbar). single_shot(0ms) chạy ở vòng lặp NGAY SAU show() — độ trễ
    // chỉ còn đúng 1 nhịp event loop (thường dưới 16ms ở 60fps), không phải
    // 300ms. Đây là lý do tách riêng thay vì dùng chung biến da_hien_lan_dau
    // trong timer polling.
    {
        let w_weak = window.as_weak();
        Timer::single_shot(std::time::Duration::from_millis(0), move || {
            if let Some(w) = w_weak.upgrade() {
                if let Some(hwnd) = hwnd_cua(&w) {
                    an_khoi_taskbar(hwnd);
                }
                let _ = w.hide();
            }
        });
    }

    // Bài học #3 (menu tray) trong Timer::start (bài học #4: poll trong Timer
    // thay cho nguon_repaint_nen — Timer chạy trong event loop kể cả window
    // ẩn, đây là điểm khác biệt kỹ thuật với egui, không cần "đánh thức" gì
    // thêm vì Slint Timer có nhịp riêng độc lập với visibility của window).
    let timer = Timer::default();
    {
        let w_weak = window.as_weak();
        let tray = tray.clone();
        let cfg_dang_dung = cfg_dang_dung.clone();
        let trang_thai = trang_thai.clone();
        // Nháy icon khay: đổi pha mỗi 2 tick (~0,6 s) — đủ nhanh để mắt bắt,
        // đủ chậm để không tốn CPU dựng icon.
        let so_tick = Cell::new(0_u32);
        // Cảnh báo ở tick trước (tiêu đề) + lần gần nhất TỰ mở cửa sổ (view_model::nen_bat_cua_so).
        let canh_bao_truoc: RefCell<Option<String>> = RefCell::new(None);
        let lan_bat_cuoi: Cell<Option<Instant>> = Cell::new(None);
        timer.start(TimerMode::Repeated, std::time::Duration::from_millis(300), move || {
            // Rút cạn TrayIconEvent (bài học #4: không đọc nội dung, chỉ để
            // channel không phình — with_menu_on_left_click(true) tự lo phần
            // bật menu khi click, giống bản egui cũ).
            while TrayIconEvent::receiver().try_recv().is_ok() {}

            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                let Some(w) = w_weak.upgrade() else { continue };
                if ev.id == MENU_ID_CAU_HINH {
                    let _ = w.show();
                } else if ev.id == MENU_ID_THOAT {
                    // Thoát thật (bài học #5) — thread net tự dọn theo process.
                    slint::quit_event_loop().ok();
                    std::process::exit(0);
                }
            }

            // Bơm trạng thái mới nhất vào properties + cập nhật tray mỗi tick
            // — kể cả khi window đang ẩn (rẻ: chỉ set property, Slint không
            // vẽ lại khi ẩn). Giữ nguyên nhịp cập nhật "In gần đây" gần-real-
            // time như bản egui cũ (request_repaint_after 500ms → đây 300ms).
            if let Some(w) = w_weak.upgrade() {
                let cfg = cfg_dang_dung.borrow().clone();
                let (da_noi, canh_bao) = {
                    let t = trang_thai.lock().unwrap_or_else(|p| p.into_inner());
                    let canh_bao = bom_view_model(&w, &cfg, &t);
                    (t.da_noi, canh_bao)
                };
                so_tick.set(so_tick.get().wrapping_add(1));
                let pha_nhay = (so_tick.get() / 2).is_multiple_of(2);
                tray.borrow_mut().cap_nhat(da_noi, canh_bao.as_ref(), pha_nhay);

                // BÁO NGAY trên máy shop: sự cố mới → mở cửa sổ (có dải đỏ) và
                // nháy thanh tiêu đề. App chỉ sống ở khay, mà Windows 10/11
                // thường giấu icon khay vào "^" — chỉ nháy icon là NV có thể
                // không bao giờ thấy.
                let bay_gio = Instant::now();
                if nen_bat_cua_so(canh_bao_truoc.borrow().as_deref(), canh_bao.as_ref(), lan_bat_cuoi.get(), bay_gio) {
                    let _ = w.show();
                    if let Some(hwnd) = hwnd_cua(&w) {
                        nhay_cua_so(hwnd);
                    }
                    lan_bat_cuoi.set(Some(bay_gio));
                }
                *canh_bao_truoc.borrow_mut() = canh_bao.map(|c| c.tieu_de);
            }
        });
    }

    // show() TRƯỚC run_event_loop_until_quit(): cần ít nhất 1 vòng lặp với
    // window đã tạo để window_handle() trả về HWND thật (bài học #6) — Timer
    // ở trên ẩn nó lại ngay trong tick đầu tiên, trước khi user kịp nhìn thấy
    // ở đa số máy.
    //
    // BUG THẬT tìm thấy lúc verify (KHÔNG phải đoán): dùng slint::run_event_
    // loop() (mặc định) làm app THOÁT ÊM (exit 0) ngay sau khi Timer::single_
    // shot hide() cửa sổ — vì run_event_loop() "runs until the last window is
    // closed" (doc chính thức) và app này CHỦ Ý không có window nào hiện (chỉ
    // sống ở tray) sau bước ẩn ban đầu, nên Slint coi "window cuối đã đóng" và
    // tự kết thúc event loop → main() trả Ok → process thoát, tray-icon biến
    // mất ("Error removing system tray icon" trong log lúc teardown). Đã xác
    // minh THẬT bằng cách build + chạy trên Win (không phải suy đoán từ code):
    // process die trong <5s dù không panic (exit code 0). Slint có sẵn đúng
    // hàm cho use-case "tray-only, không window nào hiện vẫn phải sống":
    // slint::run_event_loop_until_quit() — "continues to run even when no
    // windows or system tray icons are visible, until quit_event_loop() is
    // called" (doc chính thức, cùng chữ ký Result<(), PlatformError>). Đổi
    // sang hàm này — khớp đúng mô hình tray-first (bài học #5/#6).
    window.show()?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}

#[cfg(test)]
mod tests_chon {
    use super::*;

    /// HCM 25/09: ô Máy in tự nhảy về mục đầu ("Microsoft XPS Document Writer"),
    /// bấm Lưu là đổi máy in. Vị trí phải trỏ đúng máy đang cấu hình.
    #[test]
    fn vi_tri_dung_may_dang_cau_hinh() {
        let ds = vec!["Microsoft XPS Document Writer".to_string(), "Fax".into(), "HP Laser 103 107 108".into()];
        let (ds, i) = ds_va_vi_tri(ds, "HP Laser 103 107 108");
        assert_eq!(i, 2);
        assert_eq!(gia_tri_chon(&ds, i, "Microsoft XPS Document Writer"), "HP Laser 103 107 108");
        // Máy trong cấu hình không còn trong Windows → ghép vào đầu, vị trí 0.
        let (ds, i) = ds_va_vi_tri(vec!["Fax".into()], "HP cu");
        assert_eq!((ds[0].as_str(), i), ("HP cu", 0));
        // Vị trí hỏng → chữ đang hiện.
        assert_eq!(gia_tri_chon(&ds, 9, " Fax "), "Fax");
        assert_eq!(gia_tri_chon(&ds, -1, "Fax"), "Fax");
    }
}
