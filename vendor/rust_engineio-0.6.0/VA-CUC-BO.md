# rust_engineio 0.6.0 — bản vá cục bộ của print-agent

Nguồn: crates.io `rust_engineio` 0.6.0 (giấy phép MIT, © 2021 Bastian Kersting — xem trường `license`
trong `Cargo.toml`), chép nguyên văn từ `~/.cargo/registry`, dùng qua `[patch.crates-io]` ở
`print-agent/Cargo.toml`.

Đã sửa (0.2.7, 26/09):
- `src/transports/websocket.rs`, `src/transports/websocket_secure.rs`: bước dựng transport
  (`connect_async` / `connect_async_tls_with_config` — TCP + TLS + HTTP 101) bọc
  `tokio::time::timeout(HAN_DUNG_KET_NOI)` (25 s). Bản gốc KHÔNG có hạn: đo 26/09, `connect()` tới một
  máy nhận TCP rồi im lặng (như mạng chết giữa chừng khi PC vừa ngủ dậy / Wi-Fi đổi) sau 180 s vẫn chưa
  trả về — vòng nối lại của app đứng hẳn ("máy sleep thì mất kết nối luôn"). Quá hạn thì future bị huỷ,
  socket bị đóng, trả `Error::IncompleteIo(TimedOut)`.
- `src/lib.rs`: hằng `HAN_DUNG_KET_NOI` + hàm lỗi.
- `Cargo.toml`: bỏ `[[bench]]` + `[dev-dependencies]` (máy build .207 offline, không cần).

Không đổi gì khác. Nâng rust_socketio/rust_engineio thì bỏ thư mục này + mục `[patch.crates-io]`,
và KIỂM LẠI bản mới đã có hạn ở bước dựng kết nối chưa (test `noi_lai_dau_cuoi_connect_treo_va_thuc_day`).
