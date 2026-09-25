# Server socket.io giả — hàng đợi / huỷ v5.1

Cho hai test đầu-cuối chạy tay trong `src/net.rs`:

```bash
cd tests/e2e-hang-doi && npm i socket.io@4 && node server.js &      # nghe 47812
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_dau_cuoi --nocapture
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_khong_ack --nocapture   # ~70 s
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored noi_lai_dau_cuoi --nocapture      # ~40 s (0.2.7)
```

`noi_lai_dau_cuoi` (0.2.7) chạy `khoi_chay` THẬT qua một proxy TCP trong test: lần nối ĐẦU rơi vào
"hố đen" (nhận TCP, không bao giờ trả lời — như TLS treo khi máy vừa thức, Wi-Fi đổi) → app phải bỏ
sau `HAN_NOI` rồi nối lại được; sau đó giả lập "máy vừa ngủ dậy" → app phải bỏ kết nối cũ và nối lại
ngay (`dem.noi` tăng).

Server làm đúng hợp đồng `HOP-DONG-HANG-DOI-HUY-v5.md` §8.7: `cau-hinh` có `hang_doi`, đẩy
`hang-doi` sau khi nối / sau mỗi yêu cầu, `lay-hang-doi`, `yeu-cau-huy` (ack KetQuaHuy; id
`im_lang` thì KHÔNG ack), `yeu-cau-bo-theo-doi` (ack), `dem` (đếm yêu cầu cho test).

Chụp màn hình giao diện (software renderer, ra .bmp):

```bash
CHUP_DIR=/tmp/chup cargo test -- --ignored chup_man_hinh_hang_doi
```
