use std::path::PathBuf;

fn main() {
    #[cfg(windows)]
    {
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let icon_path = PathBuf::from(&manifest_dir)
                .join("..")
                .join("..")
                .join("..")
                .join("Terraria")
                .join("app")
                .join("goggame-1207665503.ico");
            if icon_path.exists() {
                println!("cargo:rerun-if-changed={}", icon_path.display());
                let mut res = winresource::WindowsResource::new();
                res.set_icon(&icon_path.to_string_lossy());
                res.set("ProductName", "Terraria Launcher");
                res.set(
                    "FileDescription",
                    "Terraria & tModLoader Server, Mod and Host Launcher",
                );
                if let Err(err) = res.compile() {
                    println!("cargo:warning=Failed to compile Windows resource: {err}");
                }
            }
        }
    }
}
