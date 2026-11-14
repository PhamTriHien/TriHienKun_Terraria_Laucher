# Terraria Launcher (`app`)

Rust workspace cho ứng dụng desktop quản lý Terraria & tModLoader. Hướng dẫn sử dụng dành cho người dùng cuối nằm tại [../README.md](../README.md).

## Kiến trúc Workspace

```text
app/
├── Cargo.toml
├── Cargo.lock
└── crates/
    ├── tl-core/       # Kiểu dữ liệu, đường dẫn, serverconfig.txt, parser .tmod / .wld, ModPack & backup
    ├── tl-runtime/    # Chạy game/server async (Tokio), HTTP/GitHub/SteamCMD mod manager, Firewall/UPnP/DirectHost & IPv6 Bridge
    └── tl-app/        # Giao diện desktop (eframe 0.37 / egui 0.37 / wgpu) & resource build (winresource)
```

### Chi tiết các crate

- **`tl-core`**:
  - `paths`: tự động định vị `Terraria\`, `tModLoader\`, `data\`, kiểm tra `.NET 8.0`, XNA Framework 4.0 (GAC), `steamcmd.exe`.
  - `config`: đọc/ghi nguyên tử `config.json`, mô hình `ServerProfile` và sinh/parse `serverconfig.txt` cho cả Terraria 1.4.5.8 và tModLoader.
  - `binary_parsers`: giải mã header & Deflate payload của `.tmod`, đọc metadata `.wld`, quản lý `enabled.json`, ModPack và nén/giải nén backup `.zip`.
- **`tl-runtime`**:
  - `game_runner`: khởi chạy `Terraria.exe` hoặc `dotnet tModLoader.dll -nosteam`, cài đặt XNA 4.0 qua `ShellExecuteW` (`runas`), theo dõi tài nguyên bằng `sysinfo`.
  - `server_manager`: quản lý tiến trình server qua `tokio::process`, pipe stdin/stdout/stderr, parse danh sách người chơi online và `banlist.txt`.
  - `mods_manager`: quét/bật/tắt/xóa mod, nhập `.tmod`/`.zip`, truy vấn GitHub Releases API, tải stream `.tmod`, gọi `winget` cài `Valve.SteamCMD` và tải Workshop ẩn danh.
  - `network_host`: kiểm tra LAN/VPN/Global IPv6/Public IP, mở/xóa Windows Firewall (`netsh`), tìm IGD & gọi SOAP UPnP (`AddPortMapping`/`DeletePortMapping`), chạy DirectPortHost (LAN, VPN `Radmin VPN`/`Tailscale`/`ZeroTier`, UPnP/Port Forwarding, Global IPv6), cầu nối IPv4→IPv6 (`[IPv6]:port` qua Quick Join) và kiểm tra cổng TCP thực tế.
- **`tl-app`**:
  - Binary `TerrariaLauncher` nhúng icon GOG, dùng font Windows Unicode (`Segoe UI`, `Cascadia Mono` / `Consolas`) và chia 4 màn hình (`Play`, `Server`, `Mods`, `Settings`; trong đó `Server` tích hợp mở Port mạng DirectHost/UPnP/IPv6 cùng quản trị người chơi & `banlist.txt`).

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

Sau khi build release xong, file thực thi nằm tại `target\release\TerrariaLauncher.exe` (copy lên `C:\TerrariaLauncher\TerrariaLauncher.exe` để sử dụng cạnh thư mục `Terraria\` và `tModLoader\`).
