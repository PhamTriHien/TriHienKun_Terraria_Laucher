use crate::state::NavTab;
use egui::{ColorImage, IconData, TextureHandle, TextureOptions};
use tl_core::{GameEdition, ThemeMode};

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

const BTN_EMERALD_BYTES: &[u8] = include_bytes!("../assets/btn_emerald.png");
const BTN_CYAN_BYTES: &[u8] = include_bytes!("../assets/btn_cyan.png");
const BTN_AMBER_BYTES: &[u8] = include_bytes!("../assets/btn_amber.png");
const BTN_RED_BYTES: &[u8] = include_bytes!("../assets/btn_red.png");
const BTN_SLATE_BYTES: &[u8] = include_bytes!("../assets/btn_slate.png");

const PANEL_CARD_DARK_BYTES: &[u8] = include_bytes!("../assets/panel_card_dark.png");
const PANEL_CARD_LIGHT_BYTES: &[u8] = include_bytes!("../assets/panel_card_light.png");
const PANEL_SUBCARD_DARK_BYTES: &[u8] = include_bytes!("../assets/panel_subcard_dark.png");
const PANEL_SUBCARD_LIGHT_BYTES: &[u8] = include_bytes!("../assets/panel_subcard_light.png");

const ICO_PLAY_BYTES: &[u8] = include_bytes!("../assets/ico_play.png");
const ICO_STOP_BYTES: &[u8] = include_bytes!("../assets/ico_stop.png");
const ICO_KEY_BYTES: &[u8] = include_bytes!("../assets/ico_key.png");
const ICO_DOWNLOAD_BYTES: &[u8] = include_bytes!("../assets/ico_download.png");
const ICO_REFRESH_BYTES: &[u8] = include_bytes!("../assets/ico_refresh.png");
const ICO_FOLDER_BYTES: &[u8] = include_bytes!("../assets/ico_folder.png");
const ICO_SUN_BYTES: &[u8] = include_bytes!("../assets/ico_sun.png");
const ICO_MOON_BYTES: &[u8] = include_bytes!("../assets/ico_moon.png");
const ICO_PACK_BYTES: &[u8] = include_bytes!("../assets/ico_pack.png");
const ICO_WORKSHOP_BYTES: &[u8] = include_bytes!("../assets/ico_workshop.png");
const ICO_COPY_BYTES: &[u8] = include_bytes!("../assets/ico_copy.png");
const ICO_TRASH_BYTES: &[u8] = include_bytes!("../assets/ico_trash.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiIcon {
    Play,
    Stop,
    Key,
    Download,
    Refresh,
    Folder,
    Sun,
    Moon,
    Pack,
    Workshop,
    Copy,
    Trash,
    Worlds,
    Settings,
    Server,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSkin {
    Emerald,
    Cyan,
    Amber,
    Red,
    Slate,
}

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
    pub btn_emerald: Option<TextureHandle>,
    pub btn_cyan: Option<TextureHandle>,
    pub btn_amber: Option<TextureHandle>,
    pub btn_red: Option<TextureHandle>,
    pub btn_slate: Option<TextureHandle>,
    pub panel_card_dark: Option<TextureHandle>,
    pub panel_card_light: Option<TextureHandle>,
    pub panel_subcard_dark: Option<TextureHandle>,
    pub panel_subcard_light: Option<TextureHandle>,
    pub ico_play: Option<TextureHandle>,
    pub ico_stop: Option<TextureHandle>,
    pub ico_key: Option<TextureHandle>,
    pub ico_download: Option<TextureHandle>,
    pub ico_refresh: Option<TextureHandle>,
    pub ico_folder: Option<TextureHandle>,
    pub ico_sun: Option<TextureHandle>,
    pub ico_moon: Option<TextureHandle>,
    pub ico_pack: Option<TextureHandle>,
    pub ico_workshop: Option<TextureHandle>,
    pub ico_copy: Option<TextureHandle>,
    pub ico_trash: Option<TextureHandle>,
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
            btn_emerald: decode_texture(ctx, "asset_btn_emerald", BTN_EMERALD_BYTES),
            btn_cyan: decode_texture(ctx, "asset_btn_cyan", BTN_CYAN_BYTES),
            btn_amber: decode_texture(ctx, "asset_btn_amber", BTN_AMBER_BYTES),
            btn_red: decode_texture(ctx, "asset_btn_red", BTN_RED_BYTES),
            btn_slate: decode_texture(ctx, "asset_btn_slate", BTN_SLATE_BYTES),
            panel_card_dark: decode_texture(ctx, "asset_panel_card_dark", PANEL_CARD_DARK_BYTES),
            panel_card_light: decode_texture(ctx, "asset_panel_card_light", PANEL_CARD_LIGHT_BYTES),
            panel_subcard_dark: decode_texture(
                ctx,
                "asset_panel_subcard_dark",
                PANEL_SUBCARD_DARK_BYTES,
            ),
            panel_subcard_light: decode_texture(
                ctx,
                "asset_panel_subcard_light",
                PANEL_SUBCARD_LIGHT_BYTES,
            ),
            ico_play: decode_texture(ctx, "asset_ico_play", ICO_PLAY_BYTES),
            ico_stop: decode_texture(ctx, "asset_ico_stop", ICO_STOP_BYTES),
            ico_key: decode_texture(ctx, "asset_ico_key", ICO_KEY_BYTES),
            ico_download: decode_texture(ctx, "asset_ico_download", ICO_DOWNLOAD_BYTES),
            ico_refresh: decode_texture(ctx, "asset_ico_refresh", ICO_REFRESH_BYTES),
            ico_folder: decode_texture(ctx, "asset_ico_folder", ICO_FOLDER_BYTES),
            ico_sun: decode_texture(ctx, "asset_ico_sun", ICO_SUN_BYTES),
            ico_moon: decode_texture(ctx, "asset_ico_moon", ICO_MOON_BYTES),
            ico_pack: decode_texture(ctx, "asset_ico_pack", ICO_PACK_BYTES),
            ico_workshop: decode_texture(ctx, "asset_ico_workshop", ICO_WORKSHOP_BYTES),
            ico_copy: decode_texture(ctx, "asset_ico_copy", ICO_COPY_BYTES),
            ico_trash: decode_texture(ctx, "asset_ico_trash", ICO_TRASH_BYTES),
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

    pub fn ui_icon(&self, icon: UiIcon) -> Option<&TextureHandle> {
        match icon {
            UiIcon::Play => self.ico_play.as_ref(),
            UiIcon::Stop => self.ico_stop.as_ref(),
            UiIcon::Key => self.ico_key.as_ref(),
            UiIcon::Download => self.ico_download.as_ref(),
            UiIcon::Refresh => self.ico_refresh.as_ref(),
            UiIcon::Folder => self.ico_folder.as_ref(),
            UiIcon::Sun => self.ico_sun.as_ref(),
            UiIcon::Moon => self.ico_moon.as_ref(),
            UiIcon::Pack => self.ico_pack.as_ref(),
            UiIcon::Workshop => self.ico_workshop.as_ref(),
            UiIcon::Copy => self.ico_copy.as_ref(),
            UiIcon::Trash => self.ico_trash.as_ref(),
            UiIcon::Worlds => self.nav_worlds.as_ref(),
            UiIcon::Settings => self.nav_settings.as_ref(),
            UiIcon::Server => self.nav_server.as_ref(),
            UiIcon::Host => self.nav_host.as_ref(),
        }
    }

    pub fn button_skin(&self, skin: ButtonSkin) -> Option<&TextureHandle> {
        match skin {
            ButtonSkin::Emerald => self.btn_emerald.as_ref(),
            ButtonSkin::Cyan => self.btn_cyan.as_ref(),
            ButtonSkin::Amber => self.btn_amber.as_ref(),
            ButtonSkin::Red => self.btn_red.as_ref(),
            ButtonSkin::Slate => self.btn_slate.as_ref(),
        }
    }

    pub fn card_panel(&self, mode: ThemeMode) -> Option<&TextureHandle> {
        match mode {
            ThemeMode::Dark => self.panel_card_dark.as_ref(),
            ThemeMode::Light => self.panel_card_light.as_ref(),
        }
    }

    pub fn subcard_panel(&self, mode: ThemeMode) -> Option<&TextureHandle> {
        match mode {
            ThemeMode::Dark => self.panel_subcard_dark.as_ref(),
            ThemeMode::Light => self.panel_subcard_light.as_ref(),
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
