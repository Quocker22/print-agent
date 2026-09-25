# print-agent (Rust)

Agent in hoa don cho ZaloCRM. Chay tren PC Windows o shop: nhan job in tu server
qua socket.io, giai ma PDF, in qua **driver Windows** (SumatraPDF) — chon khay + kho A5.

**1 file .exe, khong can Python/runtime.** Khac ban Python cu (dinh loi Python/OpenSSL
tren may la): ban Rust bien ra 1 exe doc lap, copy la chay.

## Build (tren may Windows, 1 lan)

1. Cai Rust: https://rustup.rs (tai `rustup-init.exe`, chay, chon mac dinh).
2. Mo PowerShell moi, trong thu muc nay:
   ```
   cargo build --release
   ```
   → file `target\release\print-agent.exe`.

## Cai SumatraPDF (de in that)

Tai https://www.sumatrapdfreader.org → cai. Agent goi `SumatraPDF.exe` (trong PATH),
hoac dat bien moi truong `SUMATRA_PATH=C:\duong-dan\SumatraPDF.exe`.

## Chay

1. Copy `config.ini.example` → `config.ini`, dien server_url/token/org_id/printer_name.
2. Chay thu (foreground):
   ```
   .\target\release\print-agent.exe config.ini
   ```
3. Test **dry-run** (khong can may in): dat `AGENT_DRY_RUN=1` truoc khi chay →
   agent ghi PDF ra thu muc `dry-run-output\` thay vi in.

## Tu khoi dong cung Windows

Mo cua so cau hinh (icon khay -> "Cau hinh...") va tick **"Khoi dong cung Windows"**.
Bam la ap dung ngay, khong can bam Luu. App ghi vao
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (khong can quyen admin).

VIEC NAY QUAN TRONG: 15-18/09/2026 may in HCM im lang HON 3 NGAY chi vi khong ai
bat lai app sau khi tat may. Bot van nhan lenh in, van noi "da xep hang in", job
chet lang sau 5 phut. Nguoi phat hien dau tien la KHACH.

> **KHONG dung `nssm`/Windows service.** Huong dan cu (bo tu ban nay) bao cai app
> lam service — chi dung cho ban Rust CU chua co UI. Ban hien tai la app tray-only
> co UI Slint, phai chay TRONG PHIEN NGUOI DUNG moi co tray icon va moi in duoc
> qua driver Windows. Service chay o session 0 khong lam duoc hai viec do.

## Phan phai verify tren may Windows that
- `bin=2` (tray-2) co dung khay A5 vat ly cua may in HP khong.
- SumatraPDF in ra dung kho A5, khong le/cat.
- Reconnect that khi mat mang / server restart.
- Service tu khoi dong sau reboot.

## Giao thuc (hop dong v4 `HOP-DONG-NHAT-KY-MAY-IN.md` §2)
- namespace `/print-agent`, auth `{token}`. **Transport: CHI websocket** (xem "Ket noi" duoi — polling da bi loai).
- server→agent event `job`: `{loai:"in", job:{id, name?, pdfBase64, paperSize, tray, copies}}`.
- server→agent event `cau-hinh`: `{hoTro:[...]}` — nho theo TUNG ket noi, quen khi noi lai.
- agent→server event `ket-qua`: `{jobId, trangThai:"da_in"|"loi"|"khong_ro", loiCuoi?, loai?, conTrongHangDoi?}`.
  `khong_ro` CHI gui khi `hoTro` co "khong_ro" (backend cu: im lang nhu truoc). `conTrongHangDoi` chi co
  voi `khong_ro`: `true` = job con trong hang doi Windows, app theo doi tiep va se bao `da_in` tre;
  `false` = job da roi hang doi (co the nam trong bo nho may in).
- `jobId` la chuoi MO (`<printJobId>-<ms>` hoac `<ms>-<n>`; printJobId la UUID — CO dau `-` ben trong — hoac
  cuid); app chi tach no tu ten file khi nhan lai job luc khoi dong (xem "Theo doi tiep").
- agent→server `su-co` (ngay khi thay su co luc in, moi (job, loai) mot lan), `trang-thai-may-in`
  (khi doi + ngay sau `cau-hinh`, doc may in moi 20 s luc ranh) — chi khi `hoTro` co; `thong-tin-app` (luon).
- Moi event len backend di qua ket noi HIEN TAI luc gui (`src/hop_thu_di.rs`): chua co ket noi / emit
  loi → cat vao hop thu di (toi da 200, bo cu nhat), gui lai khi ket noi moi gui `cau-hinh` (loc lai
  theo `hoTro` moi). Nhat ky ghi dung ket qua: `gui_server=co|xep_hang|bo`.

## Trien khai: BACKEND TRUOC, app sau
App nay can backend gui `cau-hinh`. Noi 10 s ma khong nhan `cau-hinh` → app ghi nhat ky `server_ban_cu`
va hien dong nho "Server ZaloCRM bản cũ — cần cập nhật server trước khi dùng app này"; van in nhu cu
(khong bao gio gui `khong_ro`/`su-co`/`trang-thai-may-in` cho backend cu, va KHONG tu choi in truoc khi goi
Sumatra — backend cu khong co cau dao, moi `loi` tieu mot luot thu).

Hop dong chung ba phan (app / backend / giao dien): `ZaloCRM/docs/may-in/HOP-DONG-NHAT-KY-MAY-IN.md`.

## Chong in doi — khi nao app bao `loi` (backend GUI LAI)
`loi` = backend gui lai. App CHI tra `loi` khi CHAC CHAN khong mot byte nao cua job da roi may tinh:
1. job chua tung PRINTING, 0 trang da in, co job chi trong {SPOOLING, PAUSED, BLOCKED_DEVQ}
   (danh sach TRANG — co la cung coi la khong an toan);
2. suot 15 s theo doi ma may in (cap may) van bao su co chan in **MOI** (khong co trong anh chup truoc Sumatra —
   kiem cuoi 25/09), hoac job bi BLOCKED_DEVQ. Chi co su co NEN → KHONG BAO GIO xoa: het cua so ra
   `khong_ro(<ma nen>)`, job nam lai + theo doi tiep (may WSD ngu + ERROR nen tung bi xoa → gui thu → xoa → lap mai);
3. tam dung → doc lai, kiem LAI dung dieu kien 1 → xoa → kiem lai da het.

Hoac TRUOC khi goi Sumatra (chua byte nao cua hoa don roi may — chi voi backend moi, xem T2): may in khong ton
tai trong Windows (T5) / hang doi dang ket (R-A/R-J) → `loi` ngay. copies > 1: ban sau bi go ma ban truoc da ra
→ KHONG `loi` (T9).

Moi ca khac → `khong_ro` (backend KHONG gui lai), job nam yen trong hang doi Windows va tu in khi het loi.

Them sau giam sat vong 2 (sua tiep o vong 3):
- **Co nen (R-B)**: co loi CAP MAY da co TRUOC KHI GOI SUMATRA (vd HP 4003 qua WSD bat ERROR suot ma van in)
  khong tinh chong lai job: job PRINTING roi roi hang doi sach → `da_in`; khong gui `su-co` cho co nen. Chi co
  cap may MOI xuat hien sau anh chup (ke ca trong ~2 s doc them) moi lam job thanh `khong_ro`. Luat xoa R2
  CHI tinh su co MOI (kiem cuoi 25/09 — truoc do tinh ca co nen, sinh vong lap vo han). `khong_tim_thay_may_in` khong
  bao gio la nen. **[vong 3, T3]** Anh chup lay o BUOC KIEM TRUOC KHI IN (cung mot vong doc spooler voi kiem hang
  doi), truyen xuong vong theo doi — ban truoc chup o lan doc dau cua vong theo doi, tuc SAU khi Sumatra chay
  1–60 s: su co bat dau trong khoang do bi coi la nen → `da_in` sai. Khong doc duoc may in luc kiem → nen RONG
  (than trong: moi ma deu tinh). Nut "In thu" khong qua buoc kiem → `printing::in_pdf` tu chup truoc Sumatra.
- **Khong don hoa don sau job ket (R-A/R-J)**: TRUOC khi in, app doc hang doi; co job (cua app hay chuong trinh
  khac) DANG KET mang PAPEROUT/ERROR/OFFLINE/USER_INTERVENTION → KHONG goi Sumatra, tra ngay `loi` ma cua
  job ket, `loiCuoi` "Máy in đang kẹt hoá đơn <số> — chưa gửi hoá đơn này xuống máy in" (job chuong trinh khac:
  "Hàng đợi máy in đang kẹt job khác…"). Chi BLOCKED_DEVQ thi van in. Khong doc duoc hang doi → dua vao lan doc
  gan nhat cua luong theo doi tiep. Job cua app da xep sau job ket tu truoc → `khong_ro` mang ma cua job ket.
  **[vong 3, T2] — khong tu choi in MAI:**
  - job ket ma mang them PAUSED (NV tam dung job ket — spooler bo qua no, in tiep job sau), PRINTED/COMPLETE/
    RETAINED ("Keep printed documents" — co loi con sot) hoac DELETING/DELETED → KHONG tinh la job ket
    (`spooler::CO_JOB_KHONG_CHAN`);
  - lan doc gan nhat cua theo doi tiep chi dung khi no chua qua 60 s (`theo_doi_tiep::KET_CU_NHAT`);
  - server BAN CU (ket noi hien tai chua nhan `cau-hinh`, hoac da ket luan `server_ban_cu`) → KHONG tu choi,
    in nhu truoc: backend cu khong co cau dao, moi `loi` tieu mot luot thu → vai phut la `that_bai`
    (`DuongGui::backend_moi`). Luat nay ap cho MOI lan tu choi truoc khi in (ca T5 duoi).
- **[vong 3, T5] May in khong ton tai** (doi ten/go trong Windows — OpenPrinter tra `ERROR_INVALID_PRINTER_NAME`)
  → tra ngay `loi(khong_tim_thay_may_in)`, KHONG goi Sumatra (chua byte nao roi may), nhat ky
  `khong_in_khong_tim_thay_may_in`. Backend coi ma nay la khong tieu luot → hoa don cho toi khi NV chon lai may
  in. Ban truoc: Sumatra → 15 s → `khong_ro(conTrongHangDoi:false)` "co the nam trong bo nho may in" — sai.
- **[vong 3, T4] Loi chung chung CAP MAY tren job sach → `can_xu_ly`**: job sach bi xoa o cuoi 15 s vi may (cap
  may) chi bat ERROR chung chung → `loi(can_xu_ly)` (khong tieu luot, hoa don cho may het loi). Ban truoc gui
  `loi(loi_may_in)` → backend tieu luot, ~15 phut sau `that_bai` trong khi dai bao "hệ thống TỰ in lại". Chi con
  loi rieng MOT job (BLOCKED_DEVQ) la `loi(loi_may_in)` — va dai cua no noi "thử gửi in lại vài lần; nếu vẫn lỗi
  sẽ báo thất bại" (xem bang duoi).
- **[vong 3, T9] copies > 1**: ban 1 da in, ban sau bi app go sach khoi hang doi (hoac Sumatra khong chay duoc) →
  KHONG `loi` (gui lai la thua to cua ban da ra) ma `khong_ro` + `conTrongHangDoi:false`, `loiCuoi` "Đã in k/n bản
  — bản còn lại CHƯA in (đã gỡ khỏi hàng đợi)"; dai + "In gần đây" noi dung cau do (khong "có thể đang nằm trong
  máy in").
- **Trang thai luc ranh GOP (R-A(2))**: `trang-thai-may-in` = co cap may gop co cua job dang ket trong hang doi
  (lay ma uu tien cao nhat §1); chi bao `binh_thuong` khi ca hai sach. chiTiet khong ghi ten tai lieu cua
  chuong trinh khac.
- **Kiem sau lenh xoa (R-K)**: cho 500 ms sau tam dung roi moi doc lai; trong luc cho xoa ma thay job da
  bat dau in → `khong_ro`, khong `loi`.
Dac biet: co loi TREN JOB (ERROR/PAPEROUT/USER_INTERVENTION/OFFLINE) do port monitor bat TRONG LUC dang
gui byte — may in mang co the da dem mot phan → KHONG BAO GIO xoa. Job roi hang doi "sach" ma may in bao
su co ngay sau do (~2 s) → `khong_ro` (co the dang nam trong bo nho may in). Thay DELETING/DELETED/RESTART
roi job bien mat → `khong_ro` (khong tinh la da in).

## Su co may in — canh bao tren may shop
Co su co thi cua so tu mo (toi da 1 lan / 10 phut), icon khay nhay cam, dai canh bao 2 dong. KHONG cau nao
bao NV "in lai" (NV in tay + job tu in = 2 to):

| Truong hop | Dong 1 | Dong 2 |
|---|---|---|
| `loi`, ma KHONG tieu luot (het_giay, ket_giay, offline, mo_nap, can_xu_ly) | `⚠ <Nhãn> — hoá đơn <số> chưa in` | `<Việc cần làm>. Hệ thống sẽ TỰ gửi in lại khi máy in hết lỗi — KHÔNG in tay.` |
| `loi(khong_tim_thay_may_in)` | `⚠ Không tìm thấy máy in trong Windows — hoá đơn <số> chưa in` | `Chọn lại máy in trong app. Hệ thống sẽ TỰ gửi in lại sau khi chọn đúng máy in — KHÔNG in tay.` |
| `loi`, ma TIEU luot (loi_may_in cua rieng job, loi_pdf, loi_sumatra, khong ma) | `⚠ <Nhãn> — hoá đơn <số> chưa in` | `<Việc cần làm>. Hệ thống sẽ thử gửi in lại vài lần; nếu vẫn lỗi sẽ báo thất bại — KHÔNG in tay.` |
| `khong_ro` in thieu ban (copies > 1) | `⚠ Hoá đơn <số>: Đã in k/n bản — bản còn lại CHƯA in` | `<Nhãn>: <Việc cần làm>. Bản còn lại đã gỡ khỏi hàng đợi — hệ thống KHÔNG tự in bù; cần đủ bản thì báo quản lý.` |
| `khong_ro` co ma su co may in, job con trong hang doi | `⚠ <Nhãn> — hoá đơn <số> đang chờ trong máy in` | `<Việc cần làm>. Hoá đơn sẽ TỰ in ra sau khi khắc phục — KHÔNG in lại.` |
| `khong_ro` co ma su co may in, job KHONG con trong hang doi | `⚠ <Nhãn> — hoá đơn <số> có thể đang nằm trong máy in` | `<Việc cần làm>. Khắc phục xong đợi vài phút — chỉ in lại nếu vẫn không thấy ra.` |
| `khong_ro` khong xac nhan | `Chưa xác nhận được hoá đơn <số> đã in` | `Xem khay giấy. Nếu 5 phút không thấy ra, báo quản lý kiểm trên ZaloCRM (Cài đặt › Máy in) — KHÔNG tự in lại.` |
| May in su co luc ranh | `⚠ <Nhãn> (máy in "<tên>")` | `<Việc cần làm>. Hoá đơn gửi tới sẽ chờ và tự in khi máy in hết lỗi.` |

**Nhieu dai (vong 3, T6)**: app giu toi da 5 dai hoa don CHUA xu ly (moi hoa don mot dai, vuot thi bo dai cu
nhat); cua so hien dai MOI NHAT + "(+N cảnh báo khác)" o dong 2. Ban truoc chi co mot o: dai "báo quản lý kiểm"
cua A bi su co/ket qua cua B de roi MAT khi B in xong. Moi dai tu tat theo DUNG luat cua chinh no: hoa don do
duoc xac nhan da in (theo doi tiep), mot job sau in xong (CHI khi dai la su co may in hoac loai `loi` — dai
"Chưa xác nhận được…" va dai "Đã in k/n bản" cua hoa don khac giu toi khi NV bam Đã hiểu, R-M), may in luc ranh
ve binh thuong (dai `loi`), hoac NV bam **"Đã hiểu"** — nut nay tat DAI DANG HIEN (dai ke tiep hien len; dai
su co may in thi an toi khi ma doi). "In gần đây": `Đã in` / `Lỗi — sẽ tự in lại: <nhãn>` (ma khong tieu luot)
/ `Lỗi — hệ thống thử lại: <nhãn>` (ma tieu luot) / `Đang chờ trong máy in: <nhãn>` / `Không rõ: <nhãn>` /
`Đã in k/n bản — bản còn lại CHƯA in` / `Đã in (sau khi khắc phục)`.

Ma su co doc tu co Win32 (`src/su_co.rs`); them: hang doi Pause → `can_xu_ly`, "Use Printer Offline" →
`offline`; co chung chung (chi ERROR) thi doc them cau trang thai driver ("Paper out", "Kẹt giấy"…).
Sau mot job co su co, lan doc may in luc ranh ke tiep luon gui `trang-thai-may-in` (de backend xoa chip
"Hết giấy" ket khi may in khong bat co cap may).

## Theo doi tiep job `khong_ro`
Job `khong_ro` con nam trong hang doi Windows duoc MOT luong theo doi tiep (`src/theo_doi_tiep.rs`):
doc hang doi moi 500 ms (PRINTING co the chi ~150 ms), giu toi da 12 gio/job, toi da 200 job (vuot → bo job cu
nhat, nhat ky `theo_doi_tiep_bo`). Thay PRINTING (hoac so trang > 0) roi job roi hang doi sach → gui `ket-qua
da_in` MUON (chi khi backend ho tro `khong_ro`) + "In gần đây" thanh "Đã in (sau khi khắc phục)" + tat dai.
Luong nay CHI DOC, khong bao gio dung/xoa job.

- **Mat dau / qua 12 gio (R-C)**: bien mat ma khong du bang chung (bi xoa tay, huy, chua thay in, may in bao
  su co MOI luc roi di) hoac qua 12 gio → gui `su-co {loai:"khong_xac_nhan", chiTiet:"Hoá đơn <số> không còn
  trong hàng đợi Windows mà app không thấy in (bị xoá/huỷ?) — kiểm khay giấy, in lại nếu chưa có"}` (chi khi
  hoTro co `su_co`) + "In gần đây"/dai chuyen sang cau "Chưa xác nhận được…" (dai bat lai du NV da bam Đã hiểu).
- **Co nen**: moi job mang anh chup co cap may tu lan doc DAU cua no; job roi di luc chi co co nen → van `da_in`.
- **Song qua Luu (R-E(1))**: danh sach nam trong mot kho dung chung, bam Luu thi chuyen sang lan chay mang moi
  (luong moi nhan quyen, luong cu tu thoat — khong bao gio hai luong cung dem mot job). Moi job nho MAY IN cua no:
  doi may in roi Luu thi job cu van duoc doc o may in cu.
- **Nhan lai khi khoi dong (R-E(2))**: sau buoc resume R11b, job cua app (`AI-…`/`print-agent-…`) con trong hang
  doi, khong Paused, khong dang xoa, nop chua qua 12 gio → theo doi tiep; jobId tach tu ten file
  (`AI-<so>-<khach>-<jobId>.pdf`: moi thu sau dau `-` thu ba; `print-agent-<jobId>-<hex>.pdf`). Khong tach duoc →
  nhat ky `theo_doi_tiep_bo_qua`. In xong → `da_in` tre (backend tra job theo printJobId trong jobId).
- **[vong 3, T8] Chi job cua CHINH may nay, chi id dung dang**: hang doi CHIA SE (`\\PC\may`) co job cua may
  khac — resume R11b va nhan lai R-E(2) chi dung job co `pMachineName` = ten may nay (`COMPUTERNAME`, khong phan
  biet hoa thuong, bo tien to `\\`, bo phan mien; rong = KHONG phai cua ta). Resume nham job cua may kia dung luc
  app may kia "tam dung → xoa" la in doi. Job may khac: mot dong `theo_doi_tiep_bo_qua … MAY KHAC`. Phan tach ra
  tu ten file chi nhan khi dung dang id backend (`spooler::la_id_backend`): `<uuid>-<13 so>`, `<cuid [a-z0-9]{8,40}>-<13 so>`,
  `<13 so>-<so>`, hoac id cu `<token>-<13 so>-<so>`; `AI-Report-Q3-x.pdf` cua chuong trinh khac → bo qua. Job
  dang in/theo doi tiep khop theo jobId DAY DU cua backend (duy nhat moi job) nen khong can loc may.

## Mot may mot ban app, mot ket noi
Mo ban thu hai (vd tu khoi dong + bam dup, hoac phien Windows/RDP thu hai) → hop thoai "đang chạy rồi" roi
thoat (mutex `Global\print-agent-lednelia`, TOAN MAY; `ERROR_ACCESS_DENIED` = mutex do phien khac tao = da co
ban dang chay — R-L). **[vong 3, T10]** Moi ban giu them mutex THEO PHIEN `Local\print-agent-lednelia`: Global co
ma Local chua co (hoac ACCESS_DENIED) = ban kia o PHIEN KHAC → hop thoai noi ro "đang chạy ở phiên đăng nhập
Windows khác (người dùng khác hoặc Remote Desktop)… thoát app ở phiên kia trước" (khong bao NV tim bieu tuong
khay — o phien nay no khong hien).

Bam Luu cau hinh → dung HAN ket noi + cac luong cu roi moi noi lai. **[vong 3, T7]** Trang thai giao dien (In gần
đây, cac dai) GIU NGUYEN qua lan Luu (cung mot `Arc`): ket qua cua job worker cu dang in do, `da_in` tre / mat
dau cua theo doi tiep deu hien dung cho; Luu chi quen ket noi/may in cu (`TrangThaiChung::doi_cau_hinh`). Co
dung cua ban cu bat NGAY khi bam Luu; tu do moi luong/callback cua ban cu thoi ghi trang thai ket noi. Client cu
noi xong SAU khi da bi thay (`connect()` treo >10 s) → callback "open" cua no KHONG dang ky gi (khong gianh cong
gui cua ket noi moi, khong nhan job), ket thuc luong poll, vong ngoai `disconnect()` no.

**Ket noi: CHI websocket (vong 3, T1).** Transport mac dinh cua rust_socketio (`Any`) thu websocket, hong thi LANG
LE roi ve polling (do: 1/23 lan noi lai). Tren POLLING, khi engine dong (server gui goi close, hoac chinh ta
`disconnect()`), `RawClient::poll` tra `Ok(None)` KHONG goi callback nao → vong lap thu vien quay rong 100% mot
nhan (`StoppedEngineIoSocket`), `da_noi` ket `true`, cong gui chet, watchdog khong no. App ep
`.transport_type(TransportType::Websocket)`: dong/dut ket noi hien ra thanh LOI → callback → noi lai. **Danh doi:
mang/proxy/firewall CHAN websocket thi app KHONG noi duoc** (truoc day co the roi ve polling) — nhat ky
`noi_that_bai … app chi dung websocket (khong dung polling)…` (moi loai loi mot dong). Thu vien chi bao
"EngineIO Error" — dong nhat ky do la manh moi. Can proxy/nginx cho qua `Upgrade: websocket` o `/socket.io/`.

Khong them "luoi" watchdog cho ca `da_noi=true` ma khong nhan duoc gi: lop socket.io cua rust_socketio 0.6 nuot
goi ping/pong o `rust_engineio` (khong callback nao thay) nen khong co tin hieu song RE de do; va transport
websocket DA tu phat hien ket noi chet lang — moi goi cho toi da pingInterval + pingTimeout roi tra
`PingTimeout` → callback "error" → noi lai. Watchdog 60 s (`NGUONG_CHET_HAN`) chi con cho ca `da_noi=false` lien
tuc ma khong callback nao bao (vd `connect()` Ok nhung khong bao gio co "open").

Tu noi lai cua rust_socketio TAT (`.reconnect(false)`, R-H): backoff cua thu vien het sau ~15 phut roi quay rong
100% CPU, va client da nghi van tu noi lai thanh ket noi ma. Loi/dong ket noi → callback ket thuc luong poll
cua client do (`resume_unwind`, khong `park()` mai — R-I) → vong ngoai noi lai NGAY voi backoff 1→30 s + jitter
(dat lai sau moi lan "open"); watchdog 60 s giu lam luoi. Server tu choi (token sai/thu hoi) → dong nho
"Server từ chối token máy in…" + nhat ky `tu_choi_ket_noi` MOT lan. **Bat buoc `panic = "unwind"`** (mac dinh
cua Cargo): dat `panic = "abort"` trong Cargo.toml thi `resume_unwind` giet CA tien trinh moi lan mat mang —
`net.rs` co `compile_error!` chan ngay luc bien dich.

## Nhat ky tren may
`%LOCALAPPDATA%\print-agent\logs\print-agent-YYYY-MM-DD.log` — moi dong mot su kien
(`<gio UTC>\t<su_kien>\t<noi dung>`), giu 14 ngay, khong ghi token/PDF. Tim nhanh:
`Select-String -Path "$env:LOCALAPPDATA\print-agent\logs\*.log" -Pattern "INV_2026_030045"`.

Su kien hay tra: `nhan_job`, `su_co`, `ket_qua` (co `con_trong_hang_doi=` va `gui_server=co|xep_hang|bo`),
`khong_in_hang_doi_ket`, `khong_in_khong_tim_thay_may_in`, `noi_that_bai`, `gui_lai`, `hop_thu_tran`,
`trang_thai_may_in`, `gui_trang_thai_may_in`,
`server_ban_cu`, `tu_choi_ket_noi`, `theo_doi_tiep_them`, `theo_doi_tiep_nhan_lai`, `theo_doi_tiep_bo_qua`,
`theo_doi_tiep_da_in`, `theo_doi_tiep_mat`, `theo_doi_tiep_het_han`, `theo_doi_tiep_bo`, `tiep_tuc_loi`,
`tiep_tuc_job_khi_khoi_dong`, `sumatra_qua_han`, `ngat_client_cham`, `noi_lai_tu_dau`, `net_dung`, `in_thu`.

## Thu tay tren Windows (sau khi backend moi da len)
1. In mot hoa don binh thuong → "Đã in", nhat ky `ket_qua ... gui_server=co` (hang doi rong phai doc
   ra rong — khong phai loi doc).
2. Rut het giay khay A5, in → job nam lai hang doi Windows, dai "đang chờ trong máy in", KHONG co lenh
   xoa; nap giay → giay ra DUNG MOT to, nhat ky `theo_doi_tiep_da_in`, "Đã in (sau khi khắc phục)".
3. Dang in thi tat may in (hoac bat "Use Printer Offline") → sau ~15 s job bi xoa khoi hang doi, dai "chưa in",
   backend giu job; bat may in lai → backend tu gui lai, ra DUNG MOT to. Neu may DA offline TU TRUOC khi in (co nen)
   → job KHONG bi xoa, nam lai hang doi, "đang chờ trong máy in"; bat may lai → ra DUNG MOT to, `da_in` tre.
4. Ket giay giua chung (in 2 ban) → khong co to nao bi in lai.
5. Trong luc job cho, xoa tay job trong hang doi Windows → nhat ky `theo_doi_tiep_mat`, khong gui `da_in`;
   gui `su-co khong_xac_nhan` (xem buoc 15).
6. Pause hang doi may in → dai "Máy in cần người xử lý", chiTiet "Hàng đợi máy in đang tạm dừng (Pause)".
7. Rut mang may tinh 2 phut khi dang in → cam lai: `gui_lai ... gui_server=co`, backend nhan ket qua.
8. Bam Luu cau hinh 3 lan lien → backend chi thay MOT ket noi cua may nay; Task Manager khong co CPU cao.
9. Mo exe lan hai → hop thoai "đang chạy rồi".
10. Bam "In thử" → giao dien khong do, ra mot trang A5 "In thu - Incokit Print Agent".
11. Tat app giua luc job dang "Paused" (kho tai hien — co the Pause tay mot job ten AI-…) → mo lai app:
    job duoc Resume, nhat ky `tiep_tuc_job_khi_khoi_dong`.
12. (R-A) May in MANG, rut het giay giua chung hoa don A → A `khong_ro`, nam lai hang doi. Gui hoa don B → B
    KHONG vao hang doi Windows, "In gần đây" B = "Lỗi — sẽ tự in lại: Hết giấy", nhat ky `khong_in_hang_doi_ket`,
    ZaloCRM giu B. Trang thai may in luc ranh (ZaloCRM) = het giay, khong phai binh thuong. Nap giay → A ra
    DUNG MOT to (`theo_doi_tiep_da_in`), roi backend tu gui B → B ra DUNG MOT to. Tong: 2 to cho 2 hoa don.
13. (R-J) Dung Word in mot tai lieu toi may in dang tat (job Word mang loi) → gui hoa don → hoa don `loi`
    "Hàng đợi máy in đang kẹt job khác…", khong vao hang doi. Xoa job Word → hoa don tu gui lai, ra mot to.
14. (R-B) May HP 4003 qua WSD o HN (co ERROR cap may bat suot): in 5 hoa don binh thuong → ca 5 "Đã in",
    nhat ky `ket_qua … trang_thai=da_in`, KHONG co `su_co … loai=loi_may_in`; ZaloCRM khong ngat cau dao.
15. (R-C) Rut giay, in → hoa don `khong_ro`, dai "đang chờ trong máy in". Xoa tay job trong hang doi Windows
    → ~1 s sau: nhat ky `theo_doi_tiep_mat … gui_server=co`, ZaloCRM co su co "khong_xac_nhan" voi cau "không
    còn trong hàng đợi Windows…", dai app thanh "Chưa xác nhận được hoá đơn … đã in".
16. (R-E) Rut giay, in → `khong_ro`. (a) Bam Luu cau hinh (khong doi may in) → nap giay → van co
    `theo_doi_tiep_da_in` + ZaloCRM nhan `da_in`. (b) Lam lai, lan nay TAT app truoc khi nap giay; mo lai app →
    nhat ky `theo_doi_tiep_nhan_lai`; nap giay → `theo_doi_tiep_da_in`, ZaloCRM nhan `da_in` tre.
17. (R-H/R-I) Dat token sai, Luu → dong nho "Server từ chối token máy in…", nhat ky `tu_choi_ket_noi` MOT dong;
    de 30 phut: Task Manager so luong (Threads) cua app KHONG tang dan, CPU ~0. Rut mang 3 phut roi cam lai →
    noi lai trong ≤ 30 s, backend chi thay MOT ket noi.
18. (T1) Khoi dong lai backend 5 lan lien (hoac `pm2 restart`) trong khi app dang noi: moi lan app noi lai
    trong ≤ 30 s, Task Manager CPU cua app ~0 SUOT (khong co nhan nao 100%), backend chi thay MOT ket noi. Tren
    backend/nginx: log ket noi cua may nay chi co transport `websocket`, khong co `polling`. Chan websocket o
    proxy (hoac tro `server_url` qua proxy khong ho tro Upgrade) → app "Mất kết nối", nhat ky `noi_that_bai … app
    chi dung websocket…` mot dong (khong spam), CPU ~0.
19. (T2) Rut giay, in hoa don A (`khong_ro`, nam lai hang doi). Mo hang doi Windows, Pause RIENG job A → gui hoa
    don B → B IN BINH THUONG (khong con `khong_in_hang_doi_ket`), nap giay → B ra. Resume A → A ra. Lap lai voi
    backend CU (chua co `cau-hinh`, dong nho "Server ZaloCRM bản cũ…"): job ket trong hang doi → hoa don moi van
    duoc gui xuong (khong `loi`), ZaloCRM khong co hoa don nao `that_bai` vi bi tu choi.
20. (T5) Doi ten may in trong Windows (Printers & scanners → Printer properties → ten moi) KHONG sua cau hinh app,
    gui hoa don → "In gần đây" "Lỗi — sẽ tự in lại: Không tìm thấy máy in trong Windows", dai "Chọn lại máy in
    trong app…", nhat ky `khong_in_khong_tim_thay_may_in`, Sumatra KHONG chay, ZaloCRM giu hoa don (khong tieu
    luot). Chon lai may in trong app + Luu → hoa don tu in, ra DUNG MOT to.
21. (T8) Hai may A, B cung in vao may in CHIA SE `\\A\HP` (moi may mot ban app, token rieng). Tren B: tam dung
    tay mot job `AI-…` cua B roi tat app B. Tren A: tat/mo lai app A → nhat ky A KHONG co
    `tiep_tuc_job_khi_khoi_dong` cho job cua B, co dong `theo_doi_tiep_bo_qua … MAY KHAC`; job cua B van Paused.
    In thu mot file ten `AI-Report-Q3-x.pdf` tu Word vao hang doi, tat/mo app → khong resume/nhan lai no
    (`theo_doi_tiep_bo_qua … khong tach duoc jobId dung dang backend`).
22. (T3) May HP ERROR suot (HN) van in → `da_in` nhu buoc 14. Rut giay NGAY SAU khi bam in (trong luc Sumatra con
    chay) → hoa don KHONG bao `da_in` sai: `khong_ro(het_giay)`/cho trong may in; nap giay → ra MOT to.
23. (T6/T9) Hoa don A "Chưa xác nhận được…", roi hoa don B in xong → dai A VAN CON; co them su co thi dong 2 co
    "(+N cảnh báo khác)"; bam "Đã hiểu" → dai ke tiep hien. In hoa don 2 ban, rut giay ngay khi ban 1 ra → dai
    "Đã in 1/2 bản — bản còn lại CHƯA in", ZaloCRM `khong_ro` voi loiCuoi cung cau do.
