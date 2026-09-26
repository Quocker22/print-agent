# Server socket.io giả — hàng đợi / huỷ v5.1

Cho hai test đầu-cuối chạy tay trong `src/net.rs`:

```bash
cd tests/e2e-hang-doi && npm i socket.io@4 && node server.js &      # nghe 47812
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_dau_cuoi --nocapture
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_khong_ack --nocapture   # ~70 s
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored noi_lai_dau_cuoi --nocapture      # ~30 s (0.2.7)
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored gui_ket_do --nocapture            # ~45 s (0.2.7)
cargo test -- --ignored connect_ho_den --nocapture                                       # ~50 s, không cần server
```

Chạy TỪNG lệnh (không `--include-ignored` song song): các test này dùng chung bộ đếm toàn cục
`SO_NOI_TREO` / số lần thức (`thuc_day`). Server tự về trạng thái ban đầu khi có kết nối đầu tiên.

`noi_lai_dau_cuoi` (0.2.7) chạy `khoi_chay` THẬT qua một proxy TCP trong test: lần nối ĐẦU rơi vào
"hố đen" (nhận TCP, không bao giờ trả lời — như TLS treo khi máy vừa thức, Wi-Fi đổi) → app phải bỏ
sau `rust_engineio::HAN_DUNG_KET_NOI` (25 s, bản vá cục bộ) rồi nối lại được, không còn luồng treo; sau đó
giả lập "máy vừa ngủ dậy" → app phải bỏ kết nối cũ và nối lại ngay (proxy đếm thêm kết nối TCP).
`gui_ket_do` (0.2.7): nối xong, proxy NGỪNG ĐỌC từ app → một lần gửi phải trả lỗi trong `HAN_GUI` (20 s),
các lần sau lỗi ngay, luồng poll báo `error` (app nối lại).

Server làm đúng hợp đồng `HOP-DONG-HANG-DOI-HUY-v5.md` §8.7: `cau-hinh` có `hang_doi`, đẩy
`hang-doi` sau khi nối / sau mỗi yêu cầu, `lay-hang-doi`, `yeu-cau-huy` (ack KetQuaHuy; id
`im_lang` thì KHÔNG ack), `yeu-cau-bo-theo-doi` (ack), `dem` (đếm yêu cầu cho test).

Chụp màn hình giao diện (software renderer, ra .bmp):

```bash
CHUP_DIR=/tmp/chup cargo test -- --ignored chup_man_hinh_hang_doi
```
