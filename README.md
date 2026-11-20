# TriHienKun Terraria Launcher

Ứng dụng desktop quản lý game Terraria, server chuyên dụng, mod/ModPack và kết nối chơi online cho **Terraria 1.4.5.8 (Vanilla)** và **tModLoader 2026.9** (`1.4.4.9`), xây dựng bằng Rust (`eframe` / `egui` / `wgpu`).

## Cấu trúc thư mục (`C:\TerrariaLauncher`)

```text
C:\TerrariaLauncher\
├── TriHienKun Terraria Launcher.exe  # Ứng dụng Launcher chính
├── README.md               # Tài liệu hướng dẫn sử dụng
├── Terraria\               # Terraria v1.4.5.8 GOG (Terraria.exe, TerrariaServer.exe, Content/)
│   └── __redist\XNA_40\xnafx40_redist.msi
├── tModLoader\             # tModLoader v2026.9.3.1 (tModLoader.dll, chạy trên .NET 8.0)
├── data\
│   ├── config.json         # Cấu hình launcher & hồ sơ server (tự tạo khi lưu)
│   ├── Saves\
│   │   ├── Vanilla\Worlds\ # Thế giới Terraria 1.4.5.8
│   │   └── tModLoader\     # Thế giới & mod tModLoader
│   │       ├── Worlds\     # Thế giới tModLoader (.wld/.twld)
│   │       ├── ModConfigs\ # Cấu hình mod dùng chung cho game và server
│   │       └── Mods\       # File .tmod, enabled.json, mod_sources.json, workshop_manifest.json, ModPacks\
│   ├── Servers\            # serverconfig.txt, server.log và banlist.txt theo từng hồ sơ
│   ├── Backups\            # Bản sao lưu thế giới (.zip)
│   └── SteamCMD\           # Thư mục SteamCMD & staging tải Steam Workshop
└── app\                    # Mã nguồn Rust workspace (tl-core, tl-runtime, tl-app)
```

## Bắt đầu nhanh

1. Mở `TriHienKun Terraria Launcher.exe` (hoặc build từ `app/` bằng `cargo build --release`).
2. Nếu máy chưa có game hoặc thiếu thành phần nền, vào tab **Chơi Game** (thanh cảnh báo thiếu thành phần) hoặc tab **Cài đặt -> Thành phần & Cài đặt Game**:
   - **tModLoader**: tự tải bản phát hành mới nhất (**Ổn định** / **Preview**) từ GitHub Releases và tự cài **.NET 8 Runtime portable** vào `tModLoader\dotnet\` khi cần.
   - **Terraria (Vanilla)**: mặc định tự tải và cập nhật từ nhánh [`terraria-game`](https://github.com/PhamTriHien/TriHienKun_Terraria_Laucher/tree/terraria-game) (hỗ trợ kiểm tra bản mới & xác thực SHA-256), hoặc cài từ **nhánh/Release GitHub** (`owner/repo#branch`, `owner/repo[@tag]`), **URL `.zip`**, hoặc chọn file **`.zip` có sẵn trên máy**.
   - **Microsoft XNA Framework 4.0**: nhấn **Cài XNA Framework 4.0** để tải bộ cài chính thức và khởi chạy trình cài đặt với quyền Administrator.
   - **SteamCMD** (nếu dùng Workshop): tải trực tiếp gói `steamcmd.zip` chính thức từ Valve vào `data\SteamCMD\` (tự động fallback sang `winget` nếu tải trực tiếp thất bại).
3. Chọn phiên bản muốn chơi tại tab **Chơi Game**:
   - **tModLoader 2026.9**: dùng `.NET 8` runtime (`tModLoader\dotnet\` hoặc hệ thống), hỗ trợ chọn **ModPack** và quản lý mod trực tiếp.
   - **Terraria 1.4.5.8 (Vanilla)**: cần **Microsoft XNA Framework 4.0**.

> Lưu ý: Nhánh `main` chỉ lưu mã nguồn (`app/`, `README.md`, `.gitignore`); gói cài đặt Terraria v1.4.5.8 được lưu tách biệt ở nhánh orphan `terraria-game` (chia mảnh `<= 90 MiB/phần` kèm `terraria-manifest.json`).

## Các tính năng chính

- **Chơi Game**:
  - Chạy **Terraria 1.4.5.8** hoặc **tModLoader 2026.9**.
  - **Tham gia nhanh** server bằng cách nhập/dán `Host:Port`, `[IPv6]:port` hoặc mã **Cổng Ảo P2P** (`TLPORT:...`) (Launcher tự động mở cầu nối cục bộ `127.0.0.1:<port>` cho kết nối IPv6 hoặc P2P + Relay) và lưu danh sách server thường chơi.
  - Tự động đọc thông tin file `.wld` (tên thế giới, kích thước, độ khó, special seeds, `.twld`), hỗ trợ sao lưu, khôi phục bằng `.zip` và kéo thả trực tiếp file thế giới (`.wld`, `.twld`) hoặc bản sao lưu thế giới (`.zip`) vào cửa sổ launcher.
- **Server Game**:
  - **Cấu hình**: quản lý nhiều hồ sơ server độc lập, chỉnh sửa đầy đủ `serverconfig.txt` (bao gồm các tùy chọn `seed_*`, `worldrollbackstokeep`, `slowliquids` của 1.4.5), bật server nền không mở cửa sổ CMD rời, theo dõi RAM/CPU, hỗ trợ **Lưu & Tắt** an toàn (`save` + `exit`) và tự khởi động lại khi crash.
  - **Mở Port & IP**:
    - **Cổng Ảo P2P + Relay (`TLPORT:...`)**: tích hợp sẵn trong Launcher, tạo đường hầm outbound mã hóa P2P/HTTPS-relay cố định theo từng hồ sơ server, hoạt động qua CGNAT, tường lửa router và mạng IPv4/IPv6 mà không cần tài khoản hay mở port modem; bạn bè chỉ cần dán mã `TLPORT:...` vào **Tham gia nhanh**.
    - **Mạng LAN, VPN & Mở cổng trực tiếp**: tự động nhận diện **Mạng LAN** và **VPN chơi game** (`Radmin VPN`, `Tailscale`, `ZeroTier`), cấp quyền **Windows Firewall**, mở cổng tự động **UPnP IPv4** (`WANIPConnection`) và **UPnP IPv6 Firewall Pinhole** (`WANIPv6FirewallControl`) trên Modem/Router, phát hiện chính xác **CGNAT** (đối chiếu WAN IP của Modem với Public IP) kèm hướng dẫn **Port Forwarding** thủ công, lắng nghe **Global IPv6** (`[IPv6]:port` cho **Tham gia nhanh**), kiểm tra khả năng kết nối từ Internet bên ngoài và kiểm tra kết nối TCP thực tế theo `host:port`.
  - **Console & Người chơi**: xem console log theo thời gian thực, gửi lệnh trực tiếp (`playing`, `save`, `dawn`, `noon`, `dusk`, `midnight`, `settle`, `kick`, `ban`), **Kick / Ban** người chơi đang online và quản lý `banlist.txt`.
- **Cài Mod**:
  - **Đã cài**:
    - Hiển thị icon trích xuất từ `.tmod`, phiên bản, tác giả, `ModSide`, cảnh báo thiếu dependency, bật/tắt cập nhật `enabled.json` và kéo thả trực tiếp file `.tmod` / `.zip` vào cửa sổ.
    - Gắn nhãn nguồn/ID từng mod (`Workshop <id>`, `GitHub: <repo>`, `URL .tmod`, `Thủ công`) lưu trong `mod_sources.json`; hỗ trợ gắn/chỉnh nguồn thủ công (`Lưu nguồn`) và tra cứu nhanh trên Workshop (`Tìm Workshop`).
    - **Xuất mã đồng bộ** / **Nhập mã đồng bộ** (`TLPACK1:...`) với phạm vi `Mod đang bật` hoặc `Tất cả đã cài` (giữ nguyên trạng thái bật/tắt của từng mod giữa các máy) và theo dõi tiến trình tải theo byte thực tế.
  - **Cấu hình**: xem, nhập và chỉnh file JSON trong `ModConfigs` (tối đa 512 KiB/file). Launcher sao lưu file cũ trước khi thay thế và chỉ cho lưu khi tModLoader và server tModLoader đã dừng. Cấu hình này không thay thế menu chọn preset tạo world của Advanced World Generation.
  - **GitHub & URL**: tra cứu nhanh release mới nhất từ repo phổ biến (`RecipeBrowser`, `MagicStorage`, `BossChecklist`, `AlchemistNPCLite`, `OreExcavator`, `WingSlot`), nhập repo `owner/repo` bất kỳ hoặc tải trực tiếp từ link `.tmod` kèm thanh tiến trình tải trực tiếp theo byte.
  - **ModPack**: gom nhóm mod thành ModPack riêng, chuyển đổi nhanh, đồng bộ tiến trình tải/cập nhật và hỗ trợ xuất `install.txt`.
  - **Workshop**:
    - Trình duyệt **Steam Workshop** tích hợp ngay trong app: tìm kiếm, sắp xếp (`Thịnh hành`, `Phổ biến nhất`, `Đánh giá cao`, `Mới nhất`), phân trang, xem chi tiết mod, hàng đợi dependency và phát hiện bản cập nhật.
    - Cài đặt `SteamCMD` (tải trực tiếp gói chính thức từ Valve với fallback `winget`) và tải ẩn danh mod từ Steam Workshop theo ID, URL hoặc từ trình duyệt; hiển thị thanh tiến trình tải thực tế từ `content_log.txt` & tốc độ ghi đĩa (disk I/O), đồng thời hỗ trợ cấu hình **Proxy HTTP/SOCKS5** ở tab **Cài đặt** nếu mạng bị chặn kết nối tới Steam.
- **Cài đặt**:
  - Chuyển đổi giao diện **Sáng** / **Tối** ở thanh bên (lưu trong `config.json`).
  - **Thành phần & Cài đặt Game**: kiểm tra và cài đặt/cập nhật Terraria, tModLoader, `.NET 8 Runtime`, XNA Framework 4.0 và `SteamCMD`.
  - **Hiệu năng & Đồ họa**: chọn backend đồ họa (`Tự động (Mặc định)`, `Direct3D 11` / DirectX 11, `Vulkan`, `OpenGL`), bật/tắt tối ưu **.NET 8** (`Tăng tốc biên dịch khi chơi lâu`, `Tối ưu bộ nhớ cho gói nhiều Mod (tốn thêm RAM)`) và chỉnh độ ưu tiên tiến trình (`Bình thường`, `Cao hơn`, `Cao`).
  - **Thư mục & Kết nối Proxy**: tùy chỉnh đường dẫn thư mục Terraria / tModLoader / SteamCMD, cấu hình **Proxy HTTP/SOCKS5** và nhập GitHub Token.
