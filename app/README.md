# TriHienKun Terraria Launcher (`app`)

Rust workspace cho ứng dụng desktop quản lý Terraria & tModLoader. Hướng dẫn sử dụng dành cho người dùng cuối nằm tại [../README.md](../README.md).

## Kiến trúc Workspace

```text
app/
├── Cargo.toml
├── Cargo.lock
└── crates/
    ├── tl-core/       # Kiểu dữ liệu, lỗi dùng chung, đường dẫn, serverconfig.txt, parser .tmod/.wld, ModPack & TLPACK1
    ├── tl-runtime/    # Cài đặt game/runtime, chạy game/server async (Tokio), mod/ModPack/SteamCMD, Firewall/UPnP/DirectHost & IPv6 Bridge
    └── tl-app/        # Giao diện desktop (eframe 0.37 / egui 0.37 / wgpu) & resource build (winresource)
```

### Chi tiết các crate

- **`tl-core`**:
  - `paths`: tự động định vị `Terraria\`, `tModLoader\`, `data\`, kiểm tra `.NET 8.0`, XNA Framework 4.0 (GAC), `steamcmd.exe`.
  - `config`: đọc/ghi nguyên tử `config.json`, mô hình `ServerProfile`, cấu hình đồ họa/hiệu năng (`GraphicsBackend`, `ProcessPriority`, `ThemeMode`) và sinh/parse `serverconfig.txt` cho cả Terraria 1.4.5.8 và tModLoader.
  - `error`: định nghĩa kiểu lỗi dùng chung `LauncherError` và `LauncherResult`.
  - `modpack_share`: bộ mã hóa/giải mã mã đồng bộ ModPack `TLPACK1:` (JSON + Deflate + checksum SHA-256 4-byte + Base64URL), lưu danh sách mod, nguồn tải và trạng thái bật/tắt.
  - `binary_parsers`: giải mã header & Deflate payload của `.tmod`, đọc metadata `.wld`, quản lý `enabled.json`, ModPack và nén/giải nén backup `.zip`.
- **`tl-runtime`**:
  - `http_client`: khởi tạo `rustls` crypto provider và dựng client HTTP/HTTPS hỗ trợ chuẩn hóa proxy HTTP/SOCKS5 (có fallback `no_proxy`).
  - `game_installer`: tải và cài đặt Terraria 1.4.5.8 (nhánh `terraria-game` kèm xác thực manifest SHA-256 theo từng mảnh, repo/release GitHub, URL hoặc `.zip` nội bộ), tModLoader (GitHub Releases + `.NET 8 Runtime` portable), XNA 4.0 và `SteamCMD` (tải trực tiếp gói `steamcmd.zip` chính thức từ Valve với fallback `winget`).
  - `game_runner`: khởi chạy `Terraria.exe` hoặc `dotnet tModLoader.dll -nosteam` (áp dụng cấu hình FNA3D driver, `DOTNET_TieredPGO`, `DOTNET_gcServer` và độ ưu tiên tiến trình Windows), cài đặt XNA 4.0 qua `ShellExecuteW` (`runas`) và theo dõi tài nguyên bằng `sysinfo`.
  - `server_manager`: quản lý tiến trình server qua `tokio::process`, pipe stdin/stdout/stderr, parse danh sách người chơi online và `banlist.txt`.
  - `mods_manager`: quét/bật/tắt/xóa mod, nhập `.tmod`/`.zip`, truy vấn GitHub Releases API, tải stream `.tmod` kèm tiến trình theo byte, quản lý `workshop_manifest.json`, duyệt Steam Workshop (tìm kiếm, sắp xếp, phân trang, chi tiết, phụ thuộc, kiểm tra cập nhật) và tải Workshop ẩn danh qua `SteamCMD` với telemetry thời gian thực (`content_log.txt` & disk write I/O qua `sysinfo`).
  - `modpack_sync`: quản lý `mod_sources.json`, tạo/xem trước/đối chiếu mã đồng bộ `TLPACK1:`, đồng bộ hàng loạt mod từ GitHub/URL/SteamCMD và xuất `install.txt`.
  - `network_host`: kiểm tra LAN/VPN/Global IPv6/Public IP, mở/xóa Windows Firewall (`netsh`), tìm IGD & gọi SOAP UPnP (`AddPortMapping`/`DeletePortMapping`), chạy DirectPortHost (LAN, VPN `Radmin VPN`/`Tailscale`/`ZeroTier`, UPnP/Port Forwarding, Global IPv6), cầu nối IPv4→IPv6 (`[IPv6]:port` qua Quick Join) và kiểm tra cổng TCP thực tế.
- **`tl-app`**:
  - Binary `TriHienKun_Terraria_Launcher` nhúng `assets/app_icon.ico` vào file `.exe` (qua `build.rs` / `winresource`) và nạp `assets/brand_emblem.png` (qua `assets.rs`) làm icon cửa sổ.
  - Tổ chức các module `anim3d`, `assets`, `theme`, `state` và `views/{mod, play_view, server_view, host_view, mods_view, settings_view}`, dùng font Windows Unicode (`Segoe UI`, `Cascadia Mono` / `Consolas`) và chia 4 màn hình chính (`Play`, `Server`, `Mods`, `Settings`; trong đó `server_view` tích hợp `host_view` để mở Port mạng DirectHost/UPnP/IPv6 cùng quản trị người chơi & `banlist.txt`).

## Lệnh kiểm tra & Build

Chạy từ `C:\TerrariaLauncher\app` (hoặc truyền `--manifest-path C:\TerrariaLauncher\app\Cargo.toml`):

```powershell
# Kiểm tra Clippy toàn workspace (coi warning là lỗi)
cargo clippy --offline --workspace --all-targets -- -D warnings

# Chạy unit tests
cargo test --offline --workspace

# Build bản Release tối ưu (LTO thin, strip symbols)
cargo build --offline --release
```

Sau khi build release xong, file thực thi nằm tại `target\release\TriHienKun_Terraria_Launcher.exe` (copy lên `C:\TerrariaLauncher\TriHienKun Terraria Launcher.exe` để sử dụng cạnh thư mục `Terraria\` và `tModLoader\`).
