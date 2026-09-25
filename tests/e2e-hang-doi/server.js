// Server socket.io GIẢ theo hợp đồng hàng đợi/huỷ v5.1 §8.7 — cho test đầu-cuối app 0.2.6.
const { Server } = require('socket.io');
const PORT = Number(process.env.PORT || 47812);
const io = new Server(PORT, { transports: ['websocket'] });
const muc = (id, trangThai, tamGiu) => ({
  id, soHoaDon: 'INV/2026/' + id, tenKhach: 'Anh Dev', mayInId: 'm1', mayInTen: 'HCM',
  trangThai, nhom: trangThai === 'khong_ro' ? 'chua_xac_nhan' : 'cho_in',
  lyDo: tamGiu ? 'Tạm giữ — máy in Hết giấy (từ 18:45)' : 'Đang gửi xuống máy in',
  tamGiu, lanThu: 0, tao: '2026-09-25T11:45:00.000Z', capNhat: '2026-09-25T11:45:00.000Z',
  huy: trangThai === 'cho_in' ? 'chac_chan' : 'khong',
});
let jobs = [muc('a1', 'cho_in', true), muc('a2', 'cho_in', true), muc('d1', 'dang_gui', false), muc('k1', 'khong_ro', false)];
const dem = { lay: 0, huy: 0, bo: 0, noi: 0 };
const anh = () => ({
  choIn: jobs.filter((j) => j.nhom === 'cho_in'),
  chuaXacNhan: jobs.filter((j) => j.nhom === 'chua_xac_nhan'),
  capNhat: new Date().toISOString(),
});
const boc = (x) => (Array.isArray(x) ? x[0] : x) || {};
io.of('/print-agent').on('connection', (s) => {
  if (s.handshake.auth?.token !== 'tok-e2e') { s.disconnect(true); return; }
  dem.noi++;
  s.emit('cau-hinh', { hoTro: ['khong_ro', 'su_co', 'trang_thai_may_in', 'nhat_ky_app', 'hang_doi'] });
  s.emit('hang-doi', anh());
  s.on('lay-hang-doi', () => { dem.lay++; s.emit('hang-doi', anh()); });
  s.on('yeu-cau-huy', (p, ack) => {
    dem.huy++;
    const id = boc(p).printJobId;
    if (id === 'im_lang') return; // không ack — app phải nói "chưa rõ"
    const j = jobs.find((x) => x.id === id);
    let kq;
    if (!j) kq = { id, soHoaDon: null, ok: false, trangThaiMoi: null, loi: 'KHONG_TIM_THAY', noiDung: 'Không tìm thấy lệnh in này.' };
    else if (j.trangThai === 'cho_in') {
      jobs = jobs.filter((x) => x.id !== id);
      kq = { id, soHoaDon: j.soHoaDon, ok: true, trangThaiMoi: 'da_huy', cach: 'chua_gui', noiDung: 'Đã huỷ — hoá đơn chắc chắn không in' };
    } else kq = { id, soHoaDon: j.soHoaDon, ok: false, trangThaiMoi: j.trangThai, loi: j.trangThai === 'khong_ro' ? 'CHUA_XAC_NHAN' : 'DANG_IN', noiDung: 'Hoá đơn đang được gửi/in ở máy in — không huỷ được nữa.' };
    if (typeof ack === 'function') ack(kq);
    s.emit('hang-doi', anh());
  });
  s.on('yeu-cau-bo-theo-doi', (p, ack) => {
    dem.bo++;
    const id = boc(p).printJobId;
    const j = jobs.find((x) => x.id === id);
    const ok = !!j && j.trangThai === 'khong_ro';
    if (ok) jobs = jobs.filter((x) => x.id !== id);
    ack({ id, ok, noiDung: ok ? 'Đã bỏ khỏi hàng đợi' : 'Chỉ bỏ theo dõi được lệnh chưa xác nhận' });
    s.emit('hang-doi', anh());
  });
  s.on('dem', (_p, ack) => ack(dem));
});
console.log('mock-hang-doi nghe', PORT);
