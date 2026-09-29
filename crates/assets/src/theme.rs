//! Design tokens from `assets/theme/` (see its README): three colour themes with the
//! same 31 roles, and the shared type, spacing, size, radius, stroke and opacity tokens.
//! All files are embedded at build time. `ui::theme` turns them into the egui style.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

pub const DARK_JSON: &str = include_str!("../../../assets/theme/dark.json");
pub const LIGHT_JSON: &str = include_str!("../../../assets/theme/light.json");
pub const HIGH_CONTRAST_JSON: &str = include_str!("../../../assets/theme/high-contrast.json");
pub const TOKENS_JSON: &str = include_str!("../../../assets/theme/tokens.json");

/// The bundled themes. Dark is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
    HighContrast,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 3] = [ThemeKind::Dark, ThemeKind::Light, ThemeKind::HighContrast];

    /// The `name` field of the theme file.
    pub fn name(self) -> &'static str {
        match self {
            ThemeKind::Dark => "dark",
            ThemeKind::Light => "light",
            ThemeKind::HighContrast => "high-contrast",
        }
    }

    pub fn json(self) -> &'static str {
        match self {
            ThemeKind::Dark => DARK_JSON,
            ThemeKind::Light => LIGHT_JSON,
            ThemeKind::HighContrast => HIGH_CONTRAST_JSON,
        }
    }
}

/// Which base egui visuals a theme starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dark,
    Light,
}

macro_rules! roles {
    ($($variant:ident => $key:literal,)+) => {
        /// A colour role. Every theme has a value for every role.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Role {
            $($variant,)+
        }

        impl Role {
            pub const ALL: &'static [Role] = &[$(Role::$variant,)+];
            pub const COUNT: usize = Self::ALL.len();

            /// The token name used in the theme files, such as `clip/video`.
            pub fn key(self) -> &'static str {
                match self {
                    $(Role::$variant => $key,)+
                }
            }

            pub fn from_key(key: &str) -> Option<Role> {
                match key {
                    $($key => Some(Role::$variant),)+
                    _ => None,
                }
            }

            pub fn index(self) -> usize {
                self as usize
            }
        }
    };
}

roles! {
    Bg0 => "bg/0",
    Bg1 => "bg/1",
    Bg2 => "bg/2",
    Bg3 => "bg/3",
    BorderSubtle => "border/subtle",
    BorderStrong => "border/strong",
    TextPrimary => "text/primary",
    TextSecondary => "text/secondary",
    TextMuted => "text/muted",
    TextInverse => "text/inverse",
    Accent => "accent",
    AccentSoft => "accent/soft",
    StateSuccess => "state/success",
    StateWarning => "state/warning",
    StateError => "state/error",
    StateInfo => "state/info",
    ClipVideo => "clip/video",
    ClipAudio => "clip/audio",
    ClipTitle => "clip/title",
    ClipAdjustment => "clip/adjustment",
    ClipSubtitle => "clip/subtitle",
    ClipOffline => "clip/offline",
    ScopeRed => "scope/red",
    ScopeGreen => "scope/green",
    ScopeBlue => "scope/blue",
    ScopeLuma => "scope/luma",
    GlassTop => "glass/top",
    GlassBottom => "glass/bottom",
    GlassEdge => "glass/edge",
    GlassHighlight => "glass/highlight",
    GlassShadow => "glass/shadow",
}

/// An sRGB colour with straight (unpremultiplied) alpha, as the token files store it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    /// Parses `#RRGGBB` or `#RRGGBBAA`.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let digits = hex.strip_prefix('#')?;
        if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Self {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: if digits.len() == 8 { byte(6)? } else { 255 },
        })
    }
}

/// A theme file that could not be used.
#[derive(Debug)]
pub enum ThemeError {
    Json(serde_json::Error),
    UnknownRole(String),
    MissingRole(Role),
    BadColor { role: String, value: String },
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "theme file is not valid: {error}"),
            Self::UnknownRole(role) => write!(f, "unknown colour role {role:?}"),
            Self::MissingRole(role) => write!(f, "colour role {:?} is missing", role.key()),
            Self::BadColor { role, value } => {
                write!(
                    f,
                    "colour role {role:?} has {value:?}, not #RRGGBB or #RRGGBBAA"
                )
            }
        }
    }
}

impl std::error::Error for ThemeError {}

#[derive(Deserialize)]
struct ColorEntry {
    value: String,
}

#[derive(Deserialize)]
struct ThemeFile {
    name: String,
    mode: Mode,
    colors: BTreeMap<String, ColorEntry>,
}

/// One theme's colours.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeColors {
    pub name: String,
    pub mode: Mode,
    colors: [Rgba8; Role::COUNT],
}

impl ThemeColors {
    pub fn parse(json: &str) -> Result<Self, ThemeError> {
        let file: ThemeFile = serde_json::from_str(json).map_err(ThemeError::Json)?;
        let mut colors: [Option<Rgba8>; Role::COUNT] = [None; Role::COUNT];
        for (key, entry) in &file.colors {
            let role = Role::from_key(key).ok_or_else(|| ThemeError::UnknownRole(key.clone()))?;
            colors[role.index()] =
                Some(
                    Rgba8::from_hex(&entry.value).ok_or_else(|| ThemeError::BadColor {
                        role: key.clone(),
                        value: entry.value.clone(),
                    })?,
                );
        }
        let mut out = [Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        }; Role::COUNT];
        for &role in Role::ALL {
            out[role.index()] = colors[role.index()].ok_or(ThemeError::MissingRole(role))?;
        }
        Ok(Self {
            name: file.name,
            mode: file.mode,
            colors: out,
        })
    }

    pub fn load(kind: ThemeKind) -> Result<Self, ThemeError> {
        Self::parse(kind.json())
    }

    pub fn get(&self, role: Role) -> Rgba8 {
        self.colors[role.index()]
    }
}

fn value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    #[derive(Deserialize)]
    struct Valued {
        value: f32,
    }
    Valued::deserialize(d).map(|v| v.value)
}

/// Which family a type style uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FontRole {
    Ui,
    Mono,
}

/// One step of the type scale.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct TypeStyle {
    pub font: FontRole,
    pub weight: u16,
    pub size_px: f32,
    pub line_height_px: f32,
    pub letter_spacing_em: f32,
    #[serde(default)]
    pub caps_letter_spacing_em: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct TypeScale {
    pub display: TypeStyle,
    pub title: TypeStyle,
    pub body: TypeStyle,
    pub label: TypeStyle,
    pub caption: TypeStyle,
    pub mono: TypeStyle,
}

/// App spacing steps in px. The board-only steps (48, 64) are not here on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Spacing {
    #[serde(rename = "2", deserialize_with = "value")]
    pub s2: f32,
    #[serde(rename = "4", deserialize_with = "value")]
    pub s4: f32,
    #[serde(rename = "8", deserialize_with = "value")]
    pub s8: f32,
    #[serde(rename = "12", deserialize_with = "value")]
    pub s12: f32,
    #[serde(rename = "16", deserialize_with = "value")]
    pub s16: f32,
    #[serde(rename = "24", deserialize_with = "value")]
    pub s24: f32,
    #[serde(rename = "32", deserialize_with = "value")]
    pub s32: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Sizes {
    #[serde(deserialize_with = "value")]
    pub control_pro: f32,
    #[serde(deserialize_with = "value")]
    pub control_beginner: f32,
    #[serde(deserialize_with = "value")]
    pub row_pro: f32,
    #[serde(deserialize_with = "value")]
    pub row_beginner: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Radius {
    #[serde(deserialize_with = "value")]
    pub panel: f32,
    #[serde(deserialize_with = "value")]
    pub control: f32,
    #[serde(deserialize_with = "value")]
    pub floating: f32,
    #[serde(deserialize_with = "value")]
    pub pill: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct IconSizes {
    #[serde(deserialize_with = "value")]
    pub sm: f32,
    #[serde(deserialize_with = "value")]
    pub md: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Strokes {
    #[serde(deserialize_with = "value")]
    pub icon: f32,
    #[serde(deserialize_with = "value")]
    pub selected_clip_outline: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Opacity {
    #[serde(deserialize_with = "value")]
    pub disabled: f32,
    #[serde(deserialize_with = "value")]
    pub glass: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct FloatingShadow {
    pub offset_y_px: f32,
    pub blur_px: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct FocusRing {
    pub width_px: f32,
    pub gap_px: f32,
}

/// Values written on the boards rather than stored as tokens.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Rules {
    pub floating_shadow: FloatingShadow,
    pub focus_ring: FocusRing,
    #[serde(deserialize_with = "value")]
    pub backdrop_blur_px: f32,
    #[serde(deserialize_with = "value")]
    pub danger_fill_alpha: f32,
}

/// Tokens shared by every theme (`tokens.json`).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Tokens {
    #[serde(rename = "type")]
    pub type_scale: TypeScale,
    #[serde(rename = "spacing_px")]
    pub spacing: Spacing,
    #[serde(rename = "sizes_px")]
    pub sizes: Sizes,
    #[serde(rename = "radius_px")]
    pub radius: Radius,
    #[serde(rename = "icon_px")]
    pub icon: IconSizes,
    #[serde(rename = "stroke_px")]
    pub stroke: Strokes,
    pub opacity: Opacity,
    pub rules: Rules,
}

impl Tokens {
    pub fn parse(json: &str) -> Result<Self, ThemeError> {
        serde_json::from_str(json).map_err(ThemeError::Json)
    }

    pub fn load() -> Result<Self, ThemeError> {
        Self::parse(TOKENS_JSON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_has_every_role() {
        assert_eq!(Role::COUNT, 31);
        for kind in ThemeKind::ALL {
            let theme = ThemeColors::load(kind).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert_eq!(theme.name, kind.name());
        }
    }

    #[test]
    fn values_match_the_files() {
        let dark = ThemeColors::load(ThemeKind::Dark).expect("dark");
        assert_eq!(dark.mode, Mode::Dark);
        assert_eq!(
            dark.get(Role::Bg0),
            Rgba8 {
                r: 0x0C,
                g: 0x0C,
                b: 0x0C,
                a: 255
            }
        );
        assert_eq!(
            dark.get(Role::AccentSoft),
            Rgba8 {
                r: 0xF2,
                g: 0xF2,
                b: 0xF2,
                a: 0x1A
            }
        );
        let light = ThemeColors::load(ThemeKind::Light).expect("light");
        assert_eq!(light.mode, Mode::Light);
        let hc = ThemeColors::load(ThemeKind::HighContrast).expect("hc");
        assert_eq!(hc.get(Role::TextMuted), hc.get(Role::TextPrimary));
        assert_eq!(hc.get(Role::GlassTop).a, 0);
    }

    #[test]
    fn shared_tokens_load() {
        let tokens = Tokens::load().expect("tokens.json");
        assert_eq!(tokens.type_scale.body.size_px, 14.0);
        assert_eq!(tokens.type_scale.mono.font, FontRole::Mono);
        assert_eq!(tokens.spacing.s8, 8.0);
        assert_eq!(tokens.radius.floating, 8.0);
        assert_eq!(tokens.sizes.control_pro, 24.0);
        assert_eq!(tokens.opacity.disabled, 0.4);
        assert_eq!(tokens.rules.floating_shadow.blur_px, 48.0);
    }

    #[test]
    fn bad_theme_files_are_rejected() {
        assert_eq!(Rgba8::from_hex("#12345"), None);
        assert_eq!(Rgba8::from_hex("123456"), None);
        let missing = r##"{"name":"x","mode":"dark","colors":{"bg/0":{"value":"#000000"}}}"##;
        assert!(matches!(
            ThemeColors::parse(missing),
            Err(ThemeError::MissingRole(Role::Bg1))
        ));
        let unknown = r##"{"name":"x","mode":"dark","colors":{"bg/9":{"value":"#000000"}}}"##;
        assert!(matches!(
            ThemeColors::parse(unknown),
            Err(ThemeError::UnknownRole(_))
        ));
    }
}
