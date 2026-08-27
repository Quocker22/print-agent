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

## Chay nen (Windows service, tu khoi dong cung may)

Dung [nssm](https://nssm.cc/download):
```
nssm install ZaloCRMPrintAgent "C:\...\print-agent.exe" "C:\...\config.ini"
nssm set ZaloCRMPrintAgent AppDirectory "C:\..."
nssm set ZaloCRMPrintAgent AppExit Default Restart
nssm set ZaloCRMPrintAgent Start SERVICE_AUTO_START
nssm start ZaloCRMPrintAgent
```

## Phan phai verify tren may Windows that
- `bin=2` (tray-2) co dung khay A5 vat ly cua may in HP khong.
- SumatraPDF in ra dung kho A5, khong le/cat.
- Reconnect that khi mat mang / server restart.
- Service tu khoi dong sau reboot.

## Giao thuc (chot tu server ZaloCRM)
- namespace `/print-agent`, auth `{token, orgId}`.
- server→agent event `job`: `{loai:"in", job:{id, pdfBase64, paperSize, tray, copies}}`.
- agent→server event `ket-qua`: `{jobId, trangThai:"da_in"|"loi", loiCuoi?}`.
