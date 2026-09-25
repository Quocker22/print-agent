# Server socket.io giả — hàng đợi / huỷ v5.1

Cho hai test đầu-cuối chạy tay trong `src/net.rs`:

```bash
cd tests/e2e-hang-doi && npm i socket.io@4 && node server.js &      # nghe 47812
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_dau_cuoi --nocapture
HD_URL=http://127.0.0.1:47812 cargo test -- --ignored hang_doi_khong_ack --nocapture   # ~70 s
```

Server làm đúng hợp đồng `HOP-DONG-HANG-DOI-HUY-v5.md` §8.7: `cau-hinh` có `hang_doi`, đẩy
`hang-doi` sau khi nối / sau mỗi yêu cầu, `lay-hang-doi`, `yeu-cau-huy` (ack KetQuaHuy; id
`im_lang` thì KHÔNG ack), `yeu-cau-bo-theo-doi` (ack), `dem` (đếm yêu cầu cho test).

Chụp màn hình giao diện (software renderer, ra .bmp):

```bash
CHUP_DIR=/tmp/chup cargo test -- --ignored chup_man_hinh_hang_doi
```
