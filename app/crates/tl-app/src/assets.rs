use crate::state::NavTab;
use egui::{ColorImage, IconData, TextureHandle, TextureOptions};
use tl_core::GameEdition;

const BRAND_EMBLEM_BYTES: &[u8] = include_bytes!("../assets/brand_emblem.png");
const EDITION_VANILLA_BYTES: &[u8] = include_bytes!("../assets/edition_vanilla.png");
const EDITION_TMOD_BYTES: &[u8] = include_bytes!("../assets/edition_tmodloader.png");
const HERO_VANILLA_BYTES: &[u8] = include_bytes!("../assets/hero_vanilla.jpg");
const HERO_TMOD_BYTES: &[u8] = include_bytes!("../assets/hero_tmodloader.jpg");
const NAV_PLAY_BYTES: &[u8] = include_bytes!("../assets/nav_play.png");
const NAV_SERVER_BYTES: &[u8] = include_bytes!("../assets/nav_server.png");
const NAV_MODS_BYTES: &[u8] = include_bytes!("../assets/nav_mods.png");
const NAV_HOST_BYTES: &[u8] = include_bytes!("../assets/nav_host.png");
const NAV_WORLDS_BYTES: &[u8] = include_bytes!("../assets/nav_worlds.png");
const NAV_SETTINGS_BYTES: &[u8] = include_bytes!("../assets/nav_settings.png");

pub struct LauncherAssets {
    pub brand_emblem: Option<TextureHandle>,
    pub edition_vanilla: Option<TextureHandle>,
    pub edition_tmodloader: Option<TextureHandle>,
    pub hero_vanilla: Option<TextureHandle>,
    pub hero_tmodloader: Option<TextureHandle>,
    pub nav_play: Option<TextureHandle>,
    pub nav_server: Option<TextureHandle>,
    pub nav_mods: Option<TextureHandle>,
    pub nav_host: Option<TextureHandle>,
    pub nav_worlds: Option<TextureHandle>,
    pub nav_settings: Option<TextureHandle>,
}

impl LauncherAssets {
    pub fn load(ctx: &egui::Context) -> Self {
        Self {
            brand_emblem: decode_texture(ctx, "asset_brand_emblem", BRAND_EMBLEM_BYTES),
            edition_vanilla: decode_texture(ctx, "asset_edition_vanilla", EDITION_VANILLA_BYTES),
            edition_tmodloader: decode_texture(ctx, "asset_edition_tmod", EDITION_TMOD_BYTES),
            hero_vanilla: decode_texture(ctx, "asset_hero_vanilla", HERO_VANILLA_BYTES),
            hero_tmodloader: decode_texture(ctx, "asset_hero_tmod", HERO_TMOD_BYTES),
            nav_play: decode_texture(ctx, "asset_nav_play", NAV_PLAY_BYTES),
            nav_server: decode_texture(ctx, "asset_nav_server", NAV_SERVER_BYTES),
            nav_mods: decode_texture(ctx, "asset_nav_mods", NAV_MODS_BYTES),
            nav_host: decode_texture(ctx, "asset_nav_host", NAV_HOST_BYTES),
            nav_worlds: decode_texture(ctx, "asset_nav_worlds", NAV_WORLDS_BYTES),
            nav_settings: decode_texture(ctx, "asset_nav_settings", NAV_SETTINGS_BYTES),
        }
    }

    pub fn nav_icon(&self, tab: NavTab) -> Option<&TextureHandle> {
        match tab {
            NavTab::Play => self.nav_play.as_ref(),
            NavTab::Server => self.nav_server.as_ref(),
            NavTab::Mods => self.nav_mods.as_ref(),
            NavTab::Host => self.nav_host.as_ref(),
            NavTab::Settings => self.nav_settings.as_ref(),
        }
    }

    pub fn edition_icon(&self, edition: GameEdition) -> Option<&TextureHandle> {
        match edition {
            GameEdition::Vanilla => self.edition_vanilla.as_ref(),
            GameEdition::TModLoader => self.edition_tmodloader.as_ref(),
        }
    }

    pub fn hero_banner(&self, edition: GameEdition) -> Option<&TextureHandle> {
        match edition {
            GameEdition::Vanilla => self.hero_vanilla.as_ref(),
            GameEdition::TModLoader => self.hero_tmodloader.as_ref(),
        }
    }
}

pub fn load_embedded_window_icon() -> Option<IconData> {
    let img = image::load_from_memory(BRAND_EMBLEM_BYTES).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

fn decode_texture(ctx: &egui::Context, id: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let color_img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    Some(ctx.load_texture(id, color_img, TextureOptions::LINEAR))
}
