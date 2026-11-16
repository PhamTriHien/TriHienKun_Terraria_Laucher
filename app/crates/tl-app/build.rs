use std::path::PathBuf;

fn main() {
    #[cfg(windows)]
    {
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let embedded_icon = PathBuf::from(&manifest_dir)
                .join("assets")
                .join("app_icon.ico");
            let fallback_icon = PathBuf::from(&manifest_dir)
                .join("..")
                .join("..")
                .join("..")
                .join("Terraria")
                .join("app")
                .join("goggame-1207665503.ico");
            let icon_path = if embedded_icon.exists() {
                embedded_icon
            } else {
                fallback_icon
            };

            if icon_path.exists() {
                println!("cargo:rerun-if-changed={}", icon_path.display());
                let mut res = winresource::WindowsResource::new();
                res.set_icon(&icon_path.to_string_lossy());
                res.set("ProductName", "TriHienKun Terraria Launcher");
                res.set("FileDescription", "TriHienKun Terraria Launcher");
                res.set("OriginalFilename", "TriHienKun Terraria Launcher.exe");
                if let Err(err) = res.compile() {
                    println!("cargo:warning=Failed to compile Windows resource: {err}");
                }
            }
        }
    }
}
