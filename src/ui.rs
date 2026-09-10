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

use crate::config::{self, Config, AGENT_TOKEN};
use crate::net;
use crate::printing;
use crate::state::TrangThaiChung;
use crate::taskbar_win::an_khoi_taskbar;
use crate::view_model::build_view_model;
use raw_window_handle::HasWindowHandle;
use slint::{CloseRequestResponse, ModelRc, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

slint::include_modules!();

/// Id string của 2 mục menu tray BẤM ĐƯỢC — giữ nguyên từ bản egui cũ.
const MENU_ID_CAU_HINH: &str = "cauhinh";
const MENU_ID_THOAT: &str = "thoat";

/// Đường dẫn config.ini thao tác trong UI (đọc lúc khởi động, ghi lúc bấm Lưu).
/// Giữ nguyên hằng từ bản egui cũ.
const CONFIG_PATH: &str = "config.ini";

/// Sinh icon RGBA đặc 1 màu, kích thước NxN — GIỮ NGUYÊN từ bản egui cũ (xem
/// doc-comment gốc: tự dựng buffer RGBA compile-time thay vì include_bytes! 1
/// PNG, không có bước decode nào có thể lỗi).
fn icon_mau(r: u8, g: u8, b: u8) -> Icon {
    const N: u32 = 32;
    let mut img = image::RgbaImage::new(N, N);
    for px in img.pixels_mut() {
        *px = image::Rgba([r, g, b, 255]);
    }
    Icon::from_rgba(img.into_raw(), N, N).expect("icon RGBA hợp lệ (buffer đúng NxN*4 byte)")
}

fn icon_xanh() -> Icon {
    icon_mau(0x2e, 0xa0, 0x4a) // xanh lá — đã nối server
}

fn icon_do() -> Icon {
    icon_mau(0xc0, 0x39, 0x2b) // đỏ — mất kết nối
}

/// Trạng thái tray-icon (icon/menu) — tách khỏi state chung vì chỉ dùng trong
/// vòng lặp UI (main thread), không chia sẻ với thread net.
struct Tray {
    tray_icon: Option<TrayIcon>,
    /// Icon hiện đang hiển thị — tránh set_icon() mỗi tick (bài học #2).
    da_noi_hien_thi: Option<bool>,
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
            da_noi_hien_thi: None,
            menu_trang_thai: Some(menu_trang_thai),
            menu_server: Some(menu_server),
            menu_may_in: Some(menu_may_in),
        }
    }

    /// Cập nhật icon + mục trạng thái trong menu — chỉ khi trạng thái ĐỔI
    /// (bài học #2, giữ nguyên logic bản egui cũ).
    fn cap_nhat(&mut self, da_noi: bool) {
        if self.da_noi_hien_thi == Some(da_noi) {
            return;
        }
        self.da_noi_hien_thi = Some(da_noi);
        if let Some(tray) = &self.tray_icon {
            let icon = if da_noi { icon_xanh() } else { icon_do() };
            let _ = tray.set_icon(Some(icon));
            let tooltip = if da_noi {
                "Incokit Print Agent — đã kết nối"
            } else {
                "Incokit Print Agent — mất kết nối"
            };
            let _ = tray.set_tooltip(Some(tooltip));
        }
        if let Some(mi) = &self.menu_trang_thai {
            let nhan = if da_noi { "● Đã kết nối" } else { "● Mất kết nối" };
            mi.set_text(nhan);
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
        self.da_noi_hien_thi = None;
    }
}

/// PDF giả tối thiểu hợp lệ dùng cho "In thử" — giữ nguyên từ bản egui cũ.
const PDF_GIA: &[u8] = b"%PDF-1.4\n% in thu tu Incokit Print Agent\n";

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
        })
        .collect();
    ModelRc::new(VecModel::from(hang))
}

/// Bơm ViewModel (build_view_model) vào properties MainWindow — điểm DUY NHẤT
/// map trạng thái → hiển thị, gọi từ Timer mỗi tick và ngay sau khi Lưu.
fn bom_view_model(w: &MainWindow, cfg: &Config, t: &TrangThaiChung) {
    let vm = build_view_model(cfg, t);
    w.set_da_noi(vm.da_noi);
    w.set_trang_thai_text(vm.trang_thai_text.into());
    w.set_server(vm.server.into());
    w.set_may_in(vm.may_in.into());
    w.set_jobs(jobs_sang_model(vm.jobs));
}

/// Chạy UI Slint + tray icon. Gọi từ main() SAU KHI đã spawn thread net.
/// Cửa sổ khởi động ẨN (mô hình tray-first) — xem bài học #6 ở doc-comment
/// đầu file cho lý do show() ngắn 1 nhịp để lấy HWND rồi hide() ngay.
pub fn chay_ui(cfg: Arc<Config>, trang_thai: Arc<Mutex<TrangThaiChung>>) -> Result<(), slint::PlatformError> {
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

    // cfg/trang_thai hiện đang dùng — RefCell vì chỉ đụng trên main thread
    // (Timer + callback Slint đều chạy main thread), đổi được khi bấm Lưu
    // (spawn thread net mới trỏ Mutex mới — xem doc-comment struct App bản cũ,
    // giữ nguyên chiến lược "Mutex mới mỗi lần Lưu" thay vì cố dừng thread cũ).
    let cfg_dang_dung: Rc<RefCell<Arc<Config>>> = Rc::new(RefCell::new(cfg.clone()));
    let trang_thai_dang_doc: Rc<RefCell<Arc<Mutex<TrangThaiChung>>>> =
        Rc::new(RefCell::new(trang_thai.clone()));

    // Bơm giá trị ban đầu vào form Cấu hình (org_id hiển thị ở field "Mã shop"
    // — context.md: "Token KHÔNG lấy từ form (dùng config::AGENT_TOKEN)", nên
    // form không có field token, khớp .slint hiện tại (f_server/f_ma_shop/
    // f_may_in/f_tray, không có f_token)).
    window.set_f_server(cfg.server_url.clone().into());
    window.set_f_ma_shop(cfg.org_id.clone().into());
    window.set_f_may_in(cfg.printer_name.clone().into());
    window.set_f_tray(cfg.tray.clone().into());
    bom_view_model(&window, &cfg, &trang_thai.lock().expect("mutex trang_thai không bị poison"));

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

    // Bài học #4 (on_luu): đọc form → dựng Config mới (token = AGENT_TOKEN,
    // KHÔNG lấy từ form — ràng buộc chốt trong context.md) → ghi config.ini →
    // spawn thread net MỚI với Mutex trạng thái MỚI → trỏ mọi tham chiếu đang
    // dùng (cfg_dang_dung/trang_thai_dang_doc) sang cái mới → cập nhật tray.
    {
        let w_weak = window.as_weak();
        let cfg_dang_dung = cfg_dang_dung.clone();
        let trang_thai_dang_doc = trang_thai_dang_doc.clone();
        let tray = tray.clone();
        window.on_luu(move || {
            let Some(w) = w_weak.upgrade() else { return };

            let cfg_moi = Config {
                server_url: w.get_f_server().trim().to_string(),
                token: AGENT_TOKEN.to_string(),
                org_id: w.get_f_ma_shop().trim().to_string(),
                printer_name: w.get_f_may_in().trim().to_string(),
                tray: w.get_f_tray().trim().to_string(),
                paper_size: cfg_dang_dung.borrow().paper_size.clone(),
            };

            if cfg_moi.server_url.is_empty()
                || cfg_moi.org_id.is_empty()
                || cfg_moi.printer_name.is_empty()
            {
                // Giữ nguyên thông báo bản egui cũ (validate field bắt buộc).
                w.set_trang_thai_text(
                    "Thiếu field bắt buộc (server_url/mã shop/máy in)".into(),
                );
                return;
            }

            match std::fs::write(CONFIG_PATH, config::ghi_config(&cfg_moi)) {
                Ok(()) => {
                    let cfg_moi = Arc::new(cfg_moi);
                    let trang_thai_moi = Arc::new(Mutex::new(TrangThaiChung::default()));
                    {
                        let cfg_net = cfg_moi.clone();
                        let trang_thai_net = trang_thai_moi.clone();
                        std::thread::spawn(move || net::chay_net(cfg_net, trang_thai_net));
                    }
                    tray.borrow_mut().cap_nhat_cfg(&cfg_moi);
                    *cfg_dang_dung.borrow_mut() = cfg_moi.clone();
                    *trang_thai_dang_doc.borrow_mut() = trang_thai_moi.clone();
                    bom_view_model(
                        &w,
                        &cfg_moi,
                        &trang_thai_moi.lock().expect("mutex trang_thai không bị poison"),
                    );
                }
                Err(e) => {
                    w.set_trang_thai_text(format!("Ghi config.ini lỗi: {}", e).into());
                }
            }
        });
    }

    // Bài học #4 (on_in_thu): in_pdf với PDF giả, tôn trọng AGENT_DRY_RUN —
    // giữ nguyên hành vi bản egui cũ (không tự ép dry-run ở đây).
    {
        let cfg_dang_dung = cfg_dang_dung.clone();
        window.on_in_thu(move || {
            let cfg = cfg_dang_dung.borrow().clone();
            let kq = printing::in_pdf(PDF_GIA, &cfg.printer_name, &cfg.paper_size, &cfg.tray, 1);
            if let Err(e) = kq {
                eprintln!("[print-agent] in thử lỗi: {}", e);
            }
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
                if let Ok(handle) = w.window().window_handle().window_handle() {
                    if let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() {
                        an_khoi_taskbar(h.hwnd.get());
                    }
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
        let trang_thai_dang_doc = trang_thai_dang_doc.clone();
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
                let t = trang_thai_dang_doc.borrow().clone();
                let da_noi = {
                    let t = t.lock().expect("mutex trang_thai không bị poison");
                    bom_view_model(&w, &cfg, &t);
                    t.da_noi
                };
                tray.borrow_mut().cap_nhat(da_noi);
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
