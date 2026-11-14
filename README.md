# Terraria Launcher

Ứng dụng desktop quản lý game Terraria, server chuyên dụng, mod/ModPack và kết nối chơi online cho **Terraria 1.4.5.8 (Vanilla)** và **tModLoader 2026.9** (`1.4.4.9`), xây dựng bằng Rust (`eframe` / `egui` / `wgpu`).

## Cấu trúc thư mục (`C:\TerrariaLauncher`)

```text
C:\TerrariaLauncher\
├── TerrariaLauncher.exe    # Ứng dụng Launcher chính
├── README.md               # Tài liệu hướng dẫn sử dụng
├── Terraria\               # Terraria v1.4.5.8 GOG (Terraria.exe, TerrariaServer.exe, Content/)
│   └── __redist\XNA_40\xnafx40_redist.msi
├── tModLoader\             # tModLoader v2026.9.3.1 (tModLoader.dll, chạy trên .NET 8.0)
├── data\
│   ├── config.json         # Cấu hình launcher & hồ sơ server (tự tạo khi lưu)
│   ├── Saves\
│   │   ├── Vanilla\Worlds\ # Thế giới Terraria 1.4.5.8
│   │   └── tModLoader\     # Thế giới (.wld/.twld), Mods\ (enabled.json) & ModPacks\
│   ├── Servers\            # serverconfig.txt, server.log và banlist.txt theo từng hồ sơ
│   ├── Backups\            # Bản sao lưu thế giới (.zip)
│   └── SteamCMD\           # Thư mục SteamCMD & staging tải Steam Workshop
└── app\                    # Mã nguồn Rust workspace (tl-core, tl-runtime, tl-app)
```

## Bắt đầu nhanh

1. Mở `TerrariaLauncher.exe` (hoặc build từ `app/` bằng `cargo build --release`).
2. Nếu máy chưa có game hoặc thiếu thành phần nền, vào tab **Chơi Game** (thanh cảnh báo thiếu thành phần) hoặc tab **Cài đặt -> Cài đặt & Cập nhật Game**:
   - **tModLoader**: tự tải bản phát hành mới nhất (**Ổn định** / **Preview**) từ GitHub Releases và tự cài **.NET 8 Runtime portable** vào `tModLoader\dotnet\` khi cần.
   - **Terraria (Vanilla)**: mặc định tự tải và cập nhật từ nhánh [`terraria-game`](https://github.com/PhamTriHien/TriHienKun_Terraria_Laucher/tree/terraria-game) (hỗ trợ kiểm tra bản mới & xác thực SHA-256), hoặc cài từ **nhánh/Release GitHub** (`owner/repo#branch`, `owner/repo[@tag]`), **URL `.zip`**, hoặc chọn file **`.zip` có sẵn trên máy**.
   - **Microsoft XNA Framework 4.0**: nhấn **Cài XNA Framework 4.0** để tải bộ cài chính thức và khởi chạy trình cài đặt với quyền Administrator.
3. Chọn phiên bản muốn chơi tại tab **Chơi Game**:
   - **tModLoader 2026.9**: dùng `.NET 8` runtime (`tModLoader\dotnet\` hoặc hệ thống), hỗ trợ chọn **ModPack** và quản lý mod trực tiếp.
   - **Terraria 1.4.5.8 (Vanilla)**: cần **Microsoft XNA Framework 4.0**.

> Lưu ý: Nhánh `main` chỉ lưu mã nguồn (`app/`, `README.md`, `.gitignore`); gói cài đặt Terraria v1.4.5.8 được lưu tách biệt ở nhánh orphan `terraria-game` (chia mảnh `<= 90 MiB/phần` kèm `terraria-manifest.json`).

## Các tính năng chính

- **Chơi Game**:
  - Chạy **Terraria 1.4.5.8** hoặc **tModLoader 2026.9**.
  - Tham gia nhanh server theo `Host:Port` và lưu danh sách server thường chơi.
  - Tự động đọc thông tin file `.wld` (tên thế giới, kích thước, độ khó, special seeds, `.twld`), hỗ trợ sao lưu và khôi phục bằng `.zip`.
- **Server Game**:
  - Quản lý nhiều hồ sơ server độc lập, chỉnh sửa đầy đủ `serverconfig.txt` (bao gồm các tùy chọn `seed_*`, `worldrollbackstokeep`, `slowliquids` của 1.4.5).
  - Bật server nền không mở cửa sổ CMD rời, theo dõi RAM/CPU, xem console log theo thời gian thực và gửi lệnh trực tiếp (`playing`, `save`, `dawn`, `noon`, `dusk`, `midnight`, `settle`, `kick`, `ban`).
  - Hỗ trợ **Lưu & Tắt** an toàn (`save` + `exit`) và tự khởi động lại khi crash.
- **Cài Mod**:
  - **Đã cài**: hiển thị icon trích xuất từ `.tmod`, phiên bản, tác giả, `ModSide`, cảnh báo thiếu dependency, bật/tắt cập nhật `enabled.json` và hỗ trợ kéo thả file `.tmod` / `.zip` trực tiếp vào cửa sổ.
  - **GitHub & URL**: tra cứu nhanh release mới nhất từ repo phổ biến (`RecipeBrowser`, `MagicStorage`, `BossChecklist`, `AlchemistNPCLite`, `OreExcavator`, `WingSlot`), nhập repo `owner/repo` bất kỳ hoặc tải trực tiếp từ link `.tmod`.
  - **ModPack**: gom nhóm mod đang bật thành ModPack riêng và chuyển đổi nhanh.
  - **Workshop**: tự cài `SteamCMD` qua `winget` và tải ẩn danh mod từ Steam Workshop theo ID hoặc URL (hỗ trợ cấu hình Proxy HTTP/SOCKS5 ở tab **Cài đặt** nếu mạng bị chặn kết nối tới Steam).
- **Host Online**:
  - Hiển thị và sao chép nhanh địa chỉ **Mạng LAN**, **IP Công cộng** và **Tunnel Online**.
  - Tự động hoặc thủ công mở **Windows Firewall TCP** (`TerrariaLauncher_TCP_<port>`), mở **UPnP Router** kèm cảnh báo nếu mạng nằm sau **CGNAT**, hoặc bật **Public TCP Tunnel** qua `bore.pub`.
  - Kiểm tra kết nối TCP thực tế theo `host:port`, **Kick / Ban** người chơi đang online và quản lý `banlist.txt`.
