# Spec: Làm lại UI print-agent bằng Slint

> Chốt với anh Quốc 10/09/2026. Nhánh `feat/ui-slint`.

## 1. Vấn đề

UI hiện tại (egui/eframe) "siêu lỗi, khó nhìn, không thao tác được" — nhất là trên
máy ảo/RDP shop (egui render GPU, máy ảo yếu GPU → lỗi). Cần làm lại UI, GIỮ lõi.

## 2. Ràng buộc (đã verify)

- **Đập UI, giữ lõi.** `net.rs`/`printing.rs`/`config.rs`/`state.rs`/`job.rs` = 0 dòng
  dính UI (chỉ comment). Giao diện UI↔lõi: `Arc<Mutex<TrangThaiChung>>` (da_noi, jobs,
  thong_bao_cuoi). Chỉ viết lại `ui.rs` + đổi deps egui→Slint.
- **Tray-only, ẩn HOÀN TOÀN khỏi taskbar Windows** (NV tò mò thấy icon là tắt →
  hỏng in). Giữ icon khay xanh/đỏ.
- **Chạy trên RDP/máy ảo Windows yếu GPU** → Slint software renderer (không cần GPU).
- **KHÔNG service** (anh chốt kiểu 1): tray-only, chạy trong session người dùng.

## 3. Chọn Slint (đã research)

- Tauri LOẠI: skipTaskbar bug Windows chưa fix (#10422).
- Slint: widget dựng sẵn (đẹp hơn egui), **software renderer** (chắc chạy RDP),
  ẩn taskbar qua winit backend / raw Win32.
- Ẩn taskbar: KHÔNG framework nào làm sẵn — phải tự Win32 `WS_EX_TOOLWINDOW` +
  hide→setstyle→show (~15 dòng windows-rs), giống nhau mọi framework.

## 4. Thiết kế UI (nguồn: print-agent.pen, đã đọc)

Một cửa sổ dọc, rộng 400px, nền `#F4F6F9`, card trắng bo góc, font Inter, xanh
chủ đạo `#2563EB`:

1. **Titlebar:** icon printer + "Incokit Print Agent" + chip trạng thái
   "● Đã kết nối" (xanh `#16A34A`) / "● Mất kết nối" (đỏ `#C0392B`).
2. **Status card:** Server (`zalocrm.incokit.com`), Máy in (`HP LaserJet Pro 4003 · A5`).
3. **IN GẦN ĐÂY:** list job — mã HĐ + tên khách + badge "Đã in" (xanh) / "Lỗi" (đỏ).
   Nguồn: `trang_thai.jobs` (Vec<JobLog>).
4. **CẤU HÌNH** (thu gọn được, mặc định đóng cho gọn): Server URL, **Mã shop**,
   Máy in, Khay.
5. **Actions:** "In thử" (viền) + "Lưu" (xanh đậm).

## 5. Config — "Mã shop" = org_id, token nhúng (đã verify backend)

Backend (agent-ws.ts): agent auth = `token` (hằng chung `AI_MAY_IN_AGENT_TOKEN`,
so `authToken !== token`) + `orgId` (phân biệt shop, route job). Vậy:

- **"Mã shop" NV nhập = `org_id`** (cái phân biệt shop). Chỉ 1 ô, đúng thiết kế.
- **`token` = hằng nhúng trong binary** (const, hoặc `AGENT_TOKEN` compile-time env).
  NV không nhập. Rủi ro: token lộ nếu mở exe — chấp nhận (agent nội bộ chỉ in, không
  đọc dữ liệu nhạy cảm). Đổi token định kỳ nếu cần chặt.
- Giữ: `server_url`, `printer_name`, `tray`, `paper_size`. config.ini giữ format cũ
  (config.rs không đổi nhiều — chỉ token thành hằng).

## 6. Kiến trúc thread (giữ như hiện tại)

```
main thread: Slint event loop (winit backend) → cửa sổ + tray-icon
             ↕ Arc<Mutex<TrangThaiChung>>
thread net:  socket.io (rust_socketio) — nhận job, in PDF, ghi trạng thái
```
- Slint dùng winit backend (để gọi được Win32 ẩn taskbar + tích hợp tray-icon).
- Tray-icon crate (đang dùng, 0.24) GIỮ — nó độc lập framework UI.
- Cửa sổ khởi động ẨN (tray-only), bấm menu tray "Cấu hình..." mới hiện.

## 7. Hành vi giữ nguyên (từ egui cũ)

- Icon tray xanh (đã nối) / đỏ (mất kết nối), tooltip động.
- Menu tray: trạng thái/server/máy in (info) + "Cấu hình..." + "Thoát".
- Nút X → ẩn về tray (không thoát). "Thoát" trong menu mới thoát thật.
- "Lưu" → ghi config.ini + spawn lại thread net với config mới.
- "In thử" → in 1 PDF giả qua in_pdf thật (kiểm máy in).
- Font Việt: Slint nhúng font Be Vietnam Pro (có dấu) như egui cũ.

## 8. Rủi ro — ĐÃ VERIFY trên máy Win thật (10/09, qua WinRM 192.168.18.207)

- ✅ **Slint BUILD được** trên Windows Server 2019 + GPU VMware SVGA 3D (cargo có sẵn).
- ✅ **Slint software-render (SLINT_BACKEND=winit-software) CHẠY + RENDER ĐẸP** trên
  GPU ảo VMware — cửa sổ card/chip/nút mượt nét không vỡ (ảnh anh Quốc xác nhận),
  không crash, stderr sạch. GIẢI ĐÚNG gốc bệnh egui (egui GPU-render chết trên VMware).
  → Bản thật PHẢI set backend software (env hoặc trong code) để chắc chạy máy shop.
- ⏳ CÒN verify khi có bản thật: (a) Ẩn taskbar Win32 (WS_EX_TOOLWINDOW) — icon KHÔNG
  hiện taskbar + Alt+Tab; (b) tray-icon + Slint cùng event loop — menu tray phản hồi;
  (c) font Việt nhúng hiện đủ dấu (demo bỏ dấu để test render, bản thật nhúng Be Vietnam Pro).

## 9. Test

- Lõi (net/print/config) KHÔNG đổi → test cũ giữ pass.
- UI Slint: logic thuần (map TrangThaiChung → view model) tách khỏi render để test;
  render tự nó khó test tự động (kiểm mắt trên Windows).

## 9b. Deploy — nguồn sự thật là GIT, không cp thẳng chỗ chạy

Bài học từ session RAG (10/09): deploy thẳng file vào chỗ chạy (WinRM cp / rsync
đè) là KHÔNG BỀN — lần build/pull kế tiếp xoá mất. print-agent bản mới:
- Nguồn sự thật = git repo (~/Documents/workspaces/print-agent-rs, nhánh feat/ui-slint),
  push GitHub Quocker22/print-agent.
- Máy Win (server build + máy shop): `git pull` + `cargo build --release`, KHÔNG cp
  lẻ file .rs vào máy. Verify qua WinRM chỉ để TEST (build/run thử), không phải deploy.

## 10. KHÔNG làm trong spec này (YAGNI)

- Route HN/HCM đa máy in — việc riêng, sau (cần sửa cả backend registry).
- Installer 1-click — sau.
- Đổi giao thức socket.io — giữ nguyên.
