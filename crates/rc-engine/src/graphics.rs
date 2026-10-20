//! **Port-only: the graphics options** (docs/plan/graphics_options.md). Every option controls one thing, and its first
//! value is always `Original`: what the PS2 showed on a TV. The others are modern alternatives. A preset sets them all
//! at once; it is not stored, it is read back from the options (Custom when they match no preset).
//!
//! | Option | Values | Consumer |
//! |---|---|---|
//! | [`AntiAliasing`] | Original (the game's own softening passes: the AA blit and the display copy), Off, 2x / 4x / 8x multisampling | crate::aa_blit, crate::render_settings (the samples) |
//! | [`Hud`] | Original (the 512×416 HUD scaled up smoothly, as the TV showed it), Sharp pixels | crate::hud_render composite |
//! | [`Textures`] | Original (the GS mip rule, one level per pixel), Smooth (blended levels), Sharp (the GPU's own choice, anisotropic) | the world shaders, through crate::game_camera's shared fog buffer |
//! | [`Detail`] | Original (the game's LOD distances), Far (×2), Farther (×4), Maximum (the full model whenever it is drawn) | crate::tfrag_lod, crate::tie_lod, crate::moby_render, crate::shrub_render (drawing only: the game logic keeps its own decisions) |
//! | [`FrameRate`] | 60 (the game's own rate, one drawn frame per logic tick), 120, 180, 240, Uncapped | crate::frame_pace (limiter, present mode, ticks from the clock), crate::interp (the drawn frames between two ticks) |
//!
//! [`FrameRate`] is not part of a preset (a preset keeps it): it is a property of the machine, not of the look. The
//! logic always runs at its own 60 Hz tick whatever the frame rate, so physics, animation and timers keep their speed.
//!
//! Persisted in the port settings file (`crate::render_settings::save_key`); `RC_HUD`-style switches are not used here
//! (`RC_HUD=0` already hides the HUD): `RC_GFX_ANTI_ALIASING`, `RC_GFX_HUD`, `RC_GFX_TEXTURES`, `RC_GFX_DETAIL` (the
//! keys below), `RC_MSAA=0|2|4|8` (Anti-aliasing Off or that multisampling), `RC_FPS=60|120|180|240|uncapped` (the frame rate;
//! `0` is uncapped) and `RC_GFX_PRESET=original|enhanced` override the file at start.

use bevy::prelude::*;

/// One option: its values in the Port Options row's order (the first is `Original`) and their settings-file keys.
pub trait GfxOption: Copy + PartialEq + Sized + 'static {
    const ALL: &'static [Self];
    /// The settings-file key of the option.
    const KEY: &'static str;
    fn key(self) -> &'static str;
    fn index(self) -> u8 { Self::ALL.iter().position(|&v| v == self).unwrap_or(0) as u8 }
    fn from_index(i: u8) -> Self { Self::ALL.get(i as usize).copied().unwrap_or(Self::ALL[0]) }
    fn parse(v: &str) -> Option<Self> { Self::ALL.iter().copied().find(|a| a.key() == v.trim()) }
}

/// How the world's edges are smoothed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AntiAliasing {
    /// The game's two full-screen bilinear copies, scaled to the frame (crate::aa_blit).
    #[default]
    Original,
    /// Neither: one sample per pixel, unfiltered.
    Off,
    X2,
    X4,
    X8,
}

impl AntiAliasing {
    /// The world cameras' samples per pixel.
    pub fn samples(self) -> u32 {
        match self {
            AntiAliasing::Original | AntiAliasing::Off => 1,
            AntiAliasing::X2 => 2,
            AntiAliasing::X4 => 4,
            AntiAliasing::X8 => 8,
        }
    }

    /// Multisampling at `n` samples (1 or less: Off).
    pub fn from_samples(n: u32) -> Self {
        match n {
            2 => AntiAliasing::X2,
            4 => AntiAliasing::X4,
            8 => AntiAliasing::X8,
            _ => AntiAliasing::Off,
        }
    }
}

impl GfxOption for AntiAliasing {
    const ALL: &'static [Self] = &[AntiAliasing::Original, AntiAliasing::Off, AntiAliasing::X2, AntiAliasing::X4, AntiAliasing::X8];
    const KEY: &'static str = "anti_aliasing";
    fn key(self) -> &'static str {
        match self {
            AntiAliasing::Original => "original",
            AntiAliasing::Off => "off",
            AntiAliasing::X2 => "2x",
            AntiAliasing::X4 => "4x",
            AntiAliasing::X8 => "8x",
        }
    }
}

/// How the game's 2D screen (HUD, menus, text) reaches the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Hud {
    /// The 512×416 image scaled to the frame with bilinear filtering, as the TV (and an emulator's display) showed it.
    #[default]
    Original,
    /// Each game pixel a hard square (integer-looking pixel doubling).
    SharpPixels,
}

impl GfxOption for Hud {
    const ALL: &'static [Self] = &[Hud::Original, Hud::SharpPixels];
    const KEY: &'static str = "hud";
    fn key(self) -> &'static str {
        match self {
            Hud::Original => "original",
            Hud::SharpPixels => "sharp",
        }
    }
}

/// How world textures pick their detail level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Textures {
    /// The GS rule: one whole level from the depth (`round(log2(z / 32) + K)`), bilinear inside it.
    #[default]
    Original,
    /// The same rule without rounding: the two nearest levels blended (no visible switch line).
    Smooth,
    /// The GPU's own level from the screen footprint, with anisotropic filtering.
    Sharp,
}

impl GfxOption for Textures {
    const ALL: &'static [Self] = &[Textures::Original, Textures::Smooth, Textures::Sharp];
    const KEY: &'static str = "textures";
    fn key(self) -> &'static str {
        match self {
            Textures::Original => "original",
            Textures::Smooth => "smooth",
            Textures::Sharp => "sharp",
        }
    }
}

/// How far away models keep their full detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Detail {
    #[default]
    Original,
    Far,
    Farther,
    Maximum,
}

impl Detail {
    /// The factor on the game's LOD distances. Maximum: far past every draw distance (the largest is 720 units), finite
    /// so the morph ramps stay defined.
    pub fn lod_scale(self) -> f32 {
        match self {
            Detail::Original => 1.0,
            Detail::Far => 2.0,
            Detail::Farther => 4.0,
            Detail::Maximum => 1000.0,
        }
    }
}

impl GfxOption for Detail {
    const ALL: &'static [Self] = &[Detail::Original, Detail::Far, Detail::Farther, Detail::Maximum];
    const KEY: &'static str = "detail";
    fn key(self) -> &'static str {
        match self {
            Detail::Original => "original",
            Detail::Far => "far",
            Detail::Farther => "farther",
            Detail::Maximum => "maximum",
        }
    }
}

/// How many frames a second the port draws. The logic tick stays at the game's 60 Hz in every case
/// (crate::frame_pace): a higher rate draws more frames between two ticks, from poses blended between them
/// (crate::interp).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FrameRate {
    /// One frame per tick, as the PS2 drew (and, on a ~60 Hz display, paced by vsync).
    #[default]
    Fps60,
    Fps120,
    Fps180,
    Fps240,
    /// No limit: as many frames as the machine draws, without vsync.
    Uncapped,
}

impl FrameRate {
    /// The frame limit in Hz (`None`: no limit).
    pub fn cap_hz(self) -> Option<f64> {
        match self {
            FrameRate::Fps60 => Some(60.0),
            FrameRate::Fps120 => Some(120.0),
            FrameRate::Fps180 => Some(180.0),
            FrameRate::Fps240 => Some(240.0),
            FrameRate::Uncapped => None,
        }
    }

    /// Whether frames between two ticks are drawn from blended poses (everything above the game's own 60).
    pub fn interpolates(self) -> bool { self != FrameRate::Fps60 }

    /// The `RC_FPS` switch: a settings key, or `0` for uncapped.
    pub fn from_env(v: &str) -> Option<Self> {
        match v.trim() {
            "0" => Some(FrameRate::Uncapped),
            v => Self::parse(&v.to_ascii_lowercase()),
        }
    }
}

impl GfxOption for FrameRate {
    const ALL: &'static [Self] = &[FrameRate::Fps60, FrameRate::Fps120, FrameRate::Fps180, FrameRate::Fps240, FrameRate::Uncapped];
    const KEY: &'static str = "frame_rate";
    fn key(self) -> &'static str {
        match self {
            FrameRate::Fps60 => "60",
            FrameRate::Fps120 => "120",
            FrameRate::Fps180 => "180",
            FrameRate::Fps240 => "240",
            FrameRate::Uncapped => "uncapped",
        }
    }
}

/// A preset: a value for every option (but the frame rate, which a preset keeps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Original,
    Enhanced,
    /// The options match no preset (shown, never chosen).
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Original, Preset::Enhanced, Preset::Custom];
    pub fn index(self) -> u8 { Self::ALL.iter().position(|&p| p == self).unwrap_or(0) as u8 }
    pub fn from_index(i: u8) -> Self { Self::ALL.get(i as usize).copied().unwrap_or(Preset::Custom) }
}

/// The graphics options (module docs).
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GraphicsSettings {
    pub aa: AntiAliasing,
    pub hud: Hud,
    pub textures: Textures,
    pub detail: Detail,
    pub frame_rate: FrameRate,
}

impl GraphicsSettings {
    /// The options of a preset (`Custom` keeps `self`).
    pub fn with_preset(self, p: Preset) -> Self {
        let frame_rate = self.frame_rate;
        match p {
            Preset::Original => GraphicsSettings { frame_rate, ..GraphicsSettings::default() },
            // 4x: the best count every GPU offers (crate::render_settings clamps a count a device lacks).
            Preset::Enhanced => GraphicsSettings { aa: AntiAliasing::X4, hud: Hud::Original, textures: Textures::Smooth, detail: Detail::Far, frame_rate },
            Preset::Custom => self,
        }
    }

    /// The preset these options are.
    pub fn preset(&self) -> Preset {
        [Preset::Original, Preset::Enhanced].into_iter().find(|&p| self.with_preset(p) == *self).unwrap_or(Preset::Custom)
    }

    /// The settings file's values, then the `RC_GFX_*` switches over them.
    pub fn startup() -> Self {
        let mut s = GraphicsSettings::default();
        if let Ok(p) = std::env::var("RC_GFX_PRESET") {
            match p.trim() {
                "enhanced" => s = s.with_preset(Preset::Enhanced),
                _ => s = s.with_preset(Preset::Original),
            }
        } else {
            s.aa = file_value(legacy_msaa());
            s.hud = file_value(s.hud);
            s.textures = file_value(s.textures);
            s.detail = file_value(s.detail);
        }
        // The frame rate is not part of a preset: the file's value applies under `RC_GFX_PRESET` too.
        s.frame_rate = file_value(s.frame_rate);
        s.aa = env_value(s.aa);
        // The older switch: `RC_MSAA=0|2|4|8` (0 = Off).
        if let Ok(v) = std::env::var("RC_MSAA") { s.aa = AntiAliasing::from_samples(v.trim().parse().unwrap_or(0)); }
        s.hud = env_value(s.hud);
        s.textures = env_value(s.textures);
        s.detail = env_value(s.detail);
        s.frame_rate = env_value(s.frame_rate);
        if let Some(r) = std::env::var("RC_FPS").ok().as_deref().and_then(FrameRate::from_env) { s.frame_rate = r; }
        s
    }

    /// Writes every option into the port settings file.
    pub fn save(&self) {
        save(self.aa);
        save(self.hud);
        save(self.textures);
        save(self.detail);
        save(self.frame_rate);
    }
}

/// Before the Anti-aliasing row had Original, the file kept the sample count (`msaa = 0|2|4|8`): a count above 1 stays
/// that multisampling; off (the old default) becomes Original.
fn legacy_msaa() -> AntiAliasing {
    match crate::render_settings::load_key("msaa").and_then(|v| v.trim().parse::<u32>().ok()) {
        Some(n) if n > 1 => AntiAliasing::from_samples(n),
        _ => AntiAliasing::Original,
    }
}

fn file_value<T: GfxOption>(d: T) -> T { crate::render_settings::load_key(T::KEY).as_deref().and_then(T::parse).unwrap_or(d) }

fn env_value<T: GfxOption>(d: T) -> T {
    std::env::var(format!("RC_GFX_{}", T::KEY.to_ascii_uppercase())).ok().as_deref().and_then(T::parse).unwrap_or(d)
}

fn save<T: GfxOption>(v: T) { crate::render_settings::save_key(T::KEY, v.key()); }

pub struct GraphicsPlugin;

impl Plugin for GraphicsPlugin {
    fn build(&self, app: &mut App) {
        let s = GraphicsSettings::startup();
        if s != GraphicsSettings::default() { println!("graphics: {s:?} ({:?})", s.preset()); }
        app.insert_resource(s).add_systems(PreUpdate, (apply_detail, apply_samples));
    }
}

/// The Anti-aliasing option's sample count into crate::render_settings (which puts it on the world cameras), clamped to
/// what the GPU offers.
fn apply_samples(g: Res<GraphicsSettings>, render: Option<ResMut<crate::render_settings::RenderSettings>>, supported: Option<Res<crate::render_settings::SupportedMsaa>>) {
    let Some(mut render) = render else { return };
    if !g.is_changed() { return; }
    let want = crate::render_settings::msaa_from_samples(g.aa.samples());
    let want = supported.map_or(want, |s| s.clamp(want));
    if render.msaa != want { render.msaa = want; }
}

/// The Detail distance factor into the moby LOD pick (crate::moby_lod reads it from a static; tfrag, tie and shrub
/// systems and the shaders read the resource or the shared fog buffer).
fn apply_detail(g: Res<GraphicsSettings>) {
    if g.is_changed() { crate::moby_lod::set_lod_scale(g.detail.lod_scale()); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_round_trip() {
        let o = GraphicsSettings::default();
        assert_eq!(o.preset(), Preset::Original);
        let e = o.with_preset(Preset::Enhanced);
        assert_eq!(e.preset(), Preset::Enhanced);
        let c = GraphicsSettings { hud: Hud::SharpPixels, ..o };
        assert_eq!(c.preset(), Preset::Custom);
        assert_eq!(c.with_preset(Preset::Custom), c);
    }

    #[test]
    fn frame_rate_is_not_part_of_a_preset() {
        let fast = GraphicsSettings { frame_rate: FrameRate::Fps240, ..GraphicsSettings::default() };
        assert_eq!(fast.preset(), Preset::Original);
        assert_eq!(fast.with_preset(Preset::Enhanced).frame_rate, FrameRate::Fps240);
        assert_eq!(fast.with_preset(Preset::Enhanced).preset(), Preset::Enhanced);
        assert_eq!(fast.with_preset(Preset::Original), fast);
    }

    #[test]
    fn frame_rate_switch_and_limits() {
        assert_eq!(FrameRate::from_env("0"), Some(FrameRate::Uncapped));
        assert_eq!(FrameRate::from_env(" Uncapped "), Some(FrameRate::Uncapped));
        assert_eq!(FrameRate::from_env("144"), None);
        assert_eq!(FrameRate::Fps180.cap_hz(), Some(180.0));
        assert_eq!(FrameRate::Uncapped.cap_hz(), None);
        assert!(!FrameRate::default().interpolates() && FrameRate::Fps120.interpolates());
    }

    #[test]
    fn keys_parse_back() {
        for &v in AntiAliasing::ALL { assert_eq!(AntiAliasing::parse(v.key()), Some(v)); }
        for &v in Hud::ALL { assert_eq!(Hud::parse(v.key()), Some(v)); }
        for &v in Textures::ALL { assert_eq!(Textures::parse(v.key()), Some(v)); }
        for &v in Detail::ALL { assert_eq!(Detail::parse(v.key()), Some(v)); }
        for &v in FrameRate::ALL { assert_eq!(FrameRate::parse(v.key()), Some(v)); }
        assert_eq!(Detail::from_index(9), Detail::Original);
        assert_eq!(AntiAliasing::from_samples(0), AntiAliasing::Off);
        assert_eq!(AntiAliasing::from_samples(4).samples(), 4);
    }
}
