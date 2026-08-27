// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cửa sổ egui (trạng thái + cấu hình) và tray icon (khay hệ thống).
//!
//! VÌ SAO gộp UI + tray trong 1 module: cả hai đều chạy trên MAIN THREAD.
//!
//! Doc-comment đầu file crate tray-icon nói rõ:
//! - macOS: "an event loop must be running on the main thread so you also
//!   need to create the tray icon on the main thread"
//! - Windows/Linux: "it doesn't need to be the main thread but you have to
//!   create the tray icon on the SAME thread as the event loop"
//!
//! Đối chiếu với rust-daemon (bản mẫu weijia/68★): build_tray_icon() được gọi
//! ngay TRƯỚC khi chạy event loop, trên cùng 1 thread — họ không có window
//! nên gọi thẳng trong main(). App này CÓ window (eframe), nên câu hỏi mấu
//! chốt là: App::moi() — nơi tray được tạo — có thật sự chạy trên thread nào
//! và lúc nào so với vòng lặp sự kiện?
//!
//! ĐÃ KIỂM TRA TRỰC TIẾP SOURCE eframe 0.32.3 (native/wgpu_integration.rs):
//! app_creator (closure truyền cho eframe::run_native, nơi gọi App::moi) được
//! gọi từ bên trong ApplicationHandler::resumed() — hàm này được winit gọi
//! trong lúc event_loop.run_app() đang chạy, TRÊN CHÍNH THREAD gọi run_app()
//! (main.rs gọi ui::chay_ui() thẳng từ main(), không spawn thread riêng).
//! Tức là: tạo tray trong App::moi() ĐÃ ở đúng main thread, và còn đúng thời
//! điểm lý tưởng mà tray-icon khuyến nghị cho macOS (StartCause::Init /
//! Resumed — "the earliest you can create icons"), không phải "trước khi
//! event loop chạy" như lo ngại ban đầu. KHÔNG cần chuyển việc tạo tray ra
//! chay_ui() — giữ nguyên vị trí tạo trong App::moi(), chỉ sửa CÁCH tạo menu
//! + cách xử lý event cho khớp mẫu rust-daemon (xem 2 chỗ sửa thật bên dưới).
//!
//! 2 LỖI THẬT tìm thấy khi so với mẫu (không phải lỗi luồng thread):
//!
//! 1. Menu dùng MenuItem::new() (id số tự sinh) + so `ev.id ==
//!    self.tray_menu_mo.id().clone()` — chạy được nhưng phải giữ sống 2
//!    field MenuItem chỉ để so id, không tường minh. Mẫu rust-daemon dùng
//!    MenuItem::with_id("start", ...) — id là string đặt tên rõ ràng, so
//!    trực tiếp với hằng &str. Đã đổi theo mẫu: with_id("mo"/"thoat", ...).
//!
//! 2. TrayIconBuilder mặc định with_menu_on_left_click(true) (xem
//!    tray-icon 0.24.2 src/lib.rs dòng ~307: "default is true") — nghĩa là
//!    trên Windows, CLICK TRÁI cũng bật menu chuột phải, xung đột với yêu
//!    cầu "click icon → hiện cửa sổ". Thêm nữa, code cũ khớp MỌI
//!    TrayIconEvent::Click bất kể nút chuột nào (trái/phải/giữa) và mọi
//!    button_state (Up/Down) — click phải mở menu CŨNG kích hoạt luôn hiện
//!    cửa sổ. Đã sửa: with_menu_on_left_click(false) (chỉ phải mới mở menu)
//!    + chỉ xử lý Click khi button = Left và button_state = Up.

use crate::config::{self, Config};
use crate::net;
use crate::printing;
use crate::state::TrangThaiChung;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Id string của 2 mục menu tray — dùng chung giữa lúc DỰNG menu (with_id) và
/// lúc SO SÁNH event nhận về, theo đúng cách rust-daemon làm (match
/// event.id.as_ref() với hằng "start"/"stop"/"quit"). Đặt hằng ở đây để
/// tránh gõ nhầm chuỗi ở 2 nơi khác nhau.
const MENU_ID_MO: &str = "mo";
const MENU_ID_THOAT: &str = "thoat";

/// Đường dẫn config.ini thao tác trong UI (đọc lúc khởi động, ghi lúc bấm Lưu).
const CONFIG_PATH: &str = "config.ini";

/// Sinh icon RGBA đặc 1 màu, kích thước NxN — dùng làm icon tray xanh/đỏ.
/// VÌ SAO tự vẽ thay vì nạp file .ico/.png: agent chỉ cần 2 trạng thái màu
/// đơn giản (chấm tròn không cần thiết ở kích thước khay hệ thống nhỏ xíu),
/// tự dựng buffer RGBA bằng crate `image` là đủ, khỏi phải đóng gói asset.
///
/// Đối chiếu điểm "nhúng icon qua include_bytes!" của mẫu rust-daemon
/// (load_icon() đọc include_bytes!(".../icon.png") rồi decode PNG lúc chạy):
/// mục tiêu của include_bytes! ở đó là "không phụ thuộc file ngoài đĩa" —
/// project này KHÔNG có sẵn file .ico/.png asset nào (chỉ có 2 file font),
/// nên tự dựng buffer RGBA NGAY TRONG BINARY (compile-time, không cả bước
/// decode PNG lúc runtime) đã thoả mãn đúng mục tiêu đó, thậm chí chặt hơn:
/// không có bước decode nào có thể lỗi. KHÔNG đổi sang include_bytes! của
/// một PNG vì sẽ phải tự vẽ + thêm asset mới ngoài phạm vi yêu cầu sửa tray.
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

/// Tăng dần mỗi lần người dùng bấm "Lưu" cấu hình — thread net cũ (đọc cfg cũ
/// qua Arc) vẫn còn sống (không có cách dừng an toàn giữa chừng socket.io
/// đang block), nhưng NÓ KHÔNG CÒN LÀ NGUỒN SỰ THẬT: ta so generation để biết
/// bản ghi trang_thai nào đến từ thread mới nhất. Đơn giản hoá bằng cách: mỗi
/// lần lưu, tạo Arc<Mutex<TrangThaiChung>> MỚI HOÀN TOÀN và trỏ App sang đó —
/// thread cũ tiếp tục ghi vào Mutex cũ (không ai đọc nữa), thread mới ghi vào
/// Mutex mới (App đang đọc). Thread cũ cuối cùng bị bỏ rơi nhưng vô hại vì
/// process gốc là single-agent (không tốn tài nguyên đáng kể khi idle).
struct App {
    cfg_dang_dung: Arc<Config>,
    trang_thai: Arc<Mutex<TrangThaiChung>>,

    // Form nhập trong tab Cấu hình — buffer riêng, chỉ ghi vào Config lúc bấm Lưu.
    form_server_url: String,
    form_org_id: String,
    form_token: String,
    form_printer_name: String,
    form_tray: String,
    form_paper_size: String,
    hien_token: bool,

    thong_bao_luu: Option<String>,
    thong_bao_in_thu: Option<String>,

    tray_icon: Option<TrayIcon>,
    /// Icon hiện tray đang hiển thị, để tránh gọi set_icon() mỗi frame (phí).
    tray_da_noi_hien_thi: Option<bool>,

    an_cua_so: bool,
}

impl App {
    fn moi(cfg: Arc<Config>, trang_thai: Arc<Mutex<TrangThaiChung>>) -> Self {
        // Dùng with_id (id string tường minh) thay vì MenuItem::new (id số tự
        // sinh) — theo đúng mẫu rust-daemon (build_tray_icon(): MenuItem::
        // with_id("start", "Start Task", true, None)). Lợi ích: không cần giữ
        // sống struct MenuItem chỉ để so .id() lúc nhận event — so thẳng
        // event.id với hằng MENU_ID_MO/MENU_ID_THOAT ở xu_ly_su_kien_tray().
        let menu_mo = MenuItem::with_id(MENU_ID_MO, "Mở cửa sổ", true, None);
        let menu_thoat = MenuItem::with_id(MENU_ID_THOAT, "Thoát", true, None);
        let tray_menu = Menu::new();
        // Lỗi dựng menu tray chỉ nên xảy ra khi hệ thống thiếu hỗ trợ (vd Linux
        // thiếu gtk) — không panic, chỉ bỏ qua tray, cửa sổ chính vẫn chạy được.
        let _ = tray_menu.append(&menu_mo);
        let _ = tray_menu.append(&menu_thoat);

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("Incokit Print Agent — mất kết nối")
            .with_icon(icon_do())
            // VÌ SAO tắt menu-khi-click-trái: mặc định của tray-icon crate là
            // TRUE (xem lib.rs: "Whether to show the tray menu on left click
            // ... default is true") — trên Windows điều này khiến click trái
            // (vốn dùng để "hiện cửa sổ" theo yêu cầu) CŨNG bật menu chuột
            // phải, xung đột hành vi. Chỉ giữ menu ở click phải; click trái
            // dành riêng cho hành động hiện cửa sổ (xử lý ở xu_ly_su_kien_tray).
            .with_menu_on_left_click(false)
            .build()
            .ok(); // None nếu môi trường không hỗ trợ tray (vd CI headless) — không chặn app chạy.

        Self {
            form_server_url: cfg.server_url.clone(),
            form_org_id: cfg.org_id.clone(),
            form_token: cfg.token.clone(),
            form_printer_name: cfg.printer_name.clone(),
            form_tray: cfg.tray.clone(),
            form_paper_size: cfg.paper_size.clone(),
            cfg_dang_dung: cfg,
            trang_thai,
            hien_token: false,
            thong_bao_luu: None,
            thong_bao_in_thu: None,
            tray_icon,
            tray_da_noi_hien_thi: None,
            an_cua_so: false,
        }
    }

    /// Cập nhật icon tray theo trạng thái nối — chỉ gọi set_icon khi trạng thái
    /// đổi (tránh dựng lại icon mỗi frame, lãng phí dù nhỏ).
    fn cap_nhat_tray(&mut self, da_noi: bool) {
        if self.tray_da_noi_hien_thi == Some(da_noi) {
            return;
        }
        self.tray_da_noi_hien_thi = Some(da_noi);
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
    }

    /// Xử lý sự kiện click tray + menu (nhận qua channel toàn cục của tray-icon
    /// crate, đúng pattern rust-daemon: poll try_recv() mỗi vòng lặp thay vì
    /// đăng ký callback). Gọi mỗi frame — try_recv không chặn nên rẻ.
    fn xu_ly_su_kien_tray(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            // VÌ SAO khớp CHÍNH XÁC Left + Up: TrayIconEvent::Click bắn cho
            // MỌI nút chuột (trái/phải/giữa) và cả 2 trạng thái (Down rồi Up).
            // Khớp lỏng (như bản cũ `Click { .. }`) khiến click PHẢI — vốn chỉ
            // để mở menu — cũng vô tình kích hoạt "hiện cửa sổ". Left+Up là
            // thời điểm chuẩn của một cú click hoàn chỉnh (giống double-click
            // protection tự nhiên của OS).
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                self.an_cua_so = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            // So id STRING tường minh (mẫu rust-daemon: match event.id.as_ref())
            // thay vì so với MenuItem lưu sẵn — MenuId có impl PartialEq<&str>
            // nên so trực tiếp với hằng &str, không cần .as_ref()/.to_string().
            if ev.id == MENU_ID_MO {
                self.an_cua_so = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else if ev.id == MENU_ID_THOAT {
                // Thoát thật: đóng viewport gốc → eframe kết thúc vòng lặp.
                std::process::exit(0);
            }
        }
    }

    fn tab_trang_thai(&self, ui: &mut egui::Ui, da_noi: bool, t: &TrangThaiChung) {
        ui.horizontal(|ui| {
            ui.strong("Incokit Print Agent");
            let (mau, nhan) = if da_noi {
                (egui::Color32::from_rgb(0x2e, 0xa0, 0x4a), "Đã kết nối")
            } else {
                (egui::Color32::from_rgb(0xc0, 0x39, 0x2b), "Mất kết nối")
            };
            ui.label(egui::RichText::new(format!("● {}", nhan)).color(mau).strong());
        });
        if let Some(tb) = &t.thong_bao_cuoi {
            ui.colored_label(egui::Color32::from_rgb(0xc0, 0x39, 0x2b), tb);
        }
        ui.separator();

        ui.label(format!("Server: {}", self.cfg_dang_dung.server_url));
        ui.label(format!(
            "Máy in: {} ({}, khay {})",
            self.cfg_dang_dung.printer_name, self.cfg_dang_dung.paper_size, self.cfg_dang_dung.tray
        ));

        ui.separator();
        ui.strong("In gần đây");
        if t.jobs.is_empty() {
            ui.label("Chưa có job nào.");
        } else {
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for j in &t.jobs {
                    ui.horizontal(|ui| {
                        ui.label(format!("#{}", j.so_hoa_don));
                        if let Some(k) = &j.khach {
                            ui.label(k);
                        }
                        let (mau, nhan) = if j.trang_thai == "da_in" {
                            (egui::Color32::from_rgb(0x2e, 0xa0, 0x4a), "Đã in")
                        } else {
                            (egui::Color32::from_rgb(0xc0, 0x39, 0x2b), "Lỗi")
                        };
                        ui.label(egui::RichText::new(nhan).color(mau).strong());
                        ui.weak(&j.luc);
                    });
                }
            });
        }
    }

    fn tab_cau_hinh(&mut self, ui: &mut egui::Ui) -> Option<Config> {
        let mut cfg_moi: Option<Config> = None;

        egui::Grid::new("form-cau-hinh").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Server URL");
            ui.text_edit_singleline(&mut self.form_server_url);
            ui.end_row();

            ui.label("Org ID");
            ui.text_edit_singleline(&mut self.form_org_id);
            ui.end_row();

            ui.label("Token");
            ui.horizontal(|ui| {
                if self.hien_token {
                    ui.text_edit_singleline(&mut self.form_token);
                } else {
                    let mut an = "*".repeat(self.form_token.chars().count());
                    ui.add(egui::TextEdit::singleline(&mut an).interactive(false));
                }
                if ui.small_button(if self.hien_token { "Ẩn" } else { "Hiện" }).clicked() {
                    self.hien_token = !self.hien_token;
                }
            });
            ui.end_row();

            ui.label("Máy in");
            ui.text_edit_singleline(&mut self.form_printer_name);
            ui.end_row();

            ui.label("Khay");
            ui.text_edit_singleline(&mut self.form_tray);
            ui.end_row();

            ui.label("Khổ giấy");
            ui.text_edit_singleline(&mut self.form_paper_size);
            ui.end_row();
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Lưu").clicked() {
                let cfg = Config {
                    server_url: self.form_server_url.trim().to_string(),
                    token: self.form_token.trim().to_string(),
                    org_id: self.form_org_id.trim().to_string(),
                    printer_name: self.form_printer_name.trim().to_string(),
                    tray: self.form_tray.trim().to_string(),
                    paper_size: self.form_paper_size.trim().to_string(),
                };
                if cfg.server_url.is_empty() || cfg.token.is_empty() || cfg.org_id.is_empty() || cfg.printer_name.is_empty() {
                    self.thong_bao_luu = Some("Thiếu field bắt buộc (server_url/token/org_id/máy in).".into());
                } else {
                    match std::fs::write(CONFIG_PATH, config::ghi_config(&cfg)) {
                        Ok(()) => {
                            self.thong_bao_luu = Some("Đã lưu. Đang nối lại với cấu hình mới...".into());
                            cfg_moi = Some(cfg);
                        }
                        Err(e) => {
                            self.thong_bao_luu = Some(format!("Ghi config.ini lỗi: {}", e));
                        }
                    }
                }
            }

            if ui.button("In thử").clicked() {
                // In thử = 1 job giả qua đúng đường in_pdf thật (tôn trọng
                // AGENT_DRY_RUN nếu người dùng đặt env đó — không tự ép dry-run
                // ở đây, để "in thử" phản ánh đúng cấu hình máy in thật).
                let pdf_gia = b"%PDF-1.4\n% in thu tu Incokit Print Agent\n";
                let kq = printing::in_pdf(
                    pdf_gia,
                    &self.form_printer_name,
                    &self.form_paper_size,
                    &self.form_tray,
                    1,
                );
                self.thong_bao_in_thu = Some(match kq {
                    Ok(()) => "In thử: đã gửi lệnh in thành công.".to_string(),
                    Err(e) => format!("In thử lỗi: {}", e),
                });
            }
        });

        if let Some(tb) = &self.thong_bao_luu {
            ui.label(tb);
        }
        if let Some(tb) = &self.thong_bao_in_thu {
            ui.label(tb);
        }

        cfg_moi
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.xu_ly_su_kien_tray(ctx);

        // Đóng nút X → ẨN cửa sổ thay vì thoát hẳn (kiểu Tailscale: chạy nền,
        // chỉ thoát thật qua menu tray "Thoát"). CancelClose huỷ yêu cầu đóng.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.an_cua_so = true;
        }

        let (da_noi, t_clone) = {
            let t = self.trang_thai.lock().expect("mutex trang_thai không bị poison");
            (t.da_noi, t.clone())
        };
        self.cap_nhat_tray(da_noi);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Incokit Print Agent");
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                let (mau, nhan) = if da_noi {
                    (egui::Color32::from_rgb(0x2e, 0xa0, 0x4a), "Đã kết nối")
                } else {
                    (egui::Color32::from_rgb(0xc0, 0x39, 0x2b), "Mất kết nối")
                };
                ui.label(egui::RichText::new(format!("● {}", nhan)).color(mau).strong());
            });
            ui.separator();

            egui::CollapsingHeader::new("Trạng thái").default_open(true).show(ui, |ui| {
                self.tab_trang_thai(ui, da_noi, &t_clone);
            });

            ui.add_space(8.0);

            let mut cfg_moi = None;
            egui::CollapsingHeader::new("Cấu hình").default_open(false).show(ui, |ui| {
                cfg_moi = self.tab_cau_hinh(ui);
            });

            if let Some(cfg) = cfg_moi {
                // Khởi động lại thread net với config mới: tạo Mutex trạng thái
                // MỚI (xem doc-comment struct App) và spawn chay_net trỏ vào đó.
                let cfg = Arc::new(cfg);
                let trang_thai_moi = Arc::new(Mutex::new(TrangThaiChung::default()));
                {
                    let cfg = cfg.clone();
                    let trang_thai_moi = trang_thai_moi.clone();
                    std::thread::spawn(move || net::chay_net(cfg, trang_thai_moi));
                }
                self.cfg_dang_dung = cfg;
                self.trang_thai = trang_thai_moi;
                self.tray_da_noi_hien_thi = None; // ép vẽ lại icon theo trạng thái mới
            }
        });

        // Job mới có thể đến bất cứ lúc nào từ thread net — repaint định kỳ để
        // UI hiện "In gần đây" kịp thời (không phụ thuộc thao tác chuột/phím).
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// Nạp font Be Vietnam Pro (có dấu tiếng Việt) — font mặc định egui THIẾU
/// dấu Việt (ã, ế, ơ… ra ô vuông). Nhúng thẳng vào binary qua include_bytes.
fn cai_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "viet".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/font-regular.ttf"
        ))),
    );
    // Đặt font Việt LÊN ĐẦU cả hai họ (proportional + monospace) để mọi chữ
    // dùng nó trước, fallback font gốc cho ký tự nó không có.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().insert(0, "viet".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Chạy UI egui + tray icon. Gọi từ main() SAU KHI đã spawn thread net.
/// Cửa sổ khởi động ẨN nếu đã có config.ini hợp lệ (chạy nền kiểu Tailscale);
/// hiện ngay nếu đây là lần đầu chưa có config (buộc người dùng nhập).
pub fn chay_ui(cfg: Arc<Config>, trang_thai: Arc<Mutex<TrangThaiChung>>, hien_ngay: bool) -> eframe::Result {
    let viewport = egui::ViewportBuilder::default()
        .with_title("Incokit Print Agent")
        .with_inner_size([520.0, 560.0])
        .with_visible(hien_ngay)
        // KHÔNG hiện trên taskbar — giống Tailscale, chỉ sống ở khay hệ thống.
        // Hỗ trợ tuỳ platform (xem ghi chú trong report-ui.md): egui expose
        // with_taskbar(false) ở API, nhưng winit/OS quyết định có tôn trọng
        // hay không — trên macOS ứng dụng NSApplication thường vẫn có icon
        // Dock trừ khi đổi ActivationPolicy (không làm ở bản này).
        .with_taskbar(false);

    let options = eframe::NativeOptions {
        viewport,
        // Wgpu (DirectX tren Windows) thay cho glow/OpenGL: may ao/RDP shop
        // thuong thieu OpenGL 2.0 (loi "egui_glow requires opengl 2.0+").
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Incokit Print Agent",
        options,
        Box::new(move |cc| {
            cai_font(&cc.egui_ctx);
            Ok(Box::new(App::moi(cfg, trang_thai)))
        }),
    )
}
