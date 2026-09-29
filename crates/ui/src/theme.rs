//! The Glass theme: the tokens from `assets/theme/` turned into the egui style.
//!
//! This is the only UI module allowed to build colours from numbers; everything else
//! asks a [`Theme`] for a [`Role`] (`crates/app/tests/ui_source.rs` enforces it).
//!
//! ```ignore
//! ui::theme::set(ctx, ThemeKind::Light);             // switch at run time
//! let clip = ui::theme::current(ctx).color(Role::ClipVideo); // custom painting
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use assets::theme::{Rgba8, ThemeColors, ThemeError, Tokens, TypeStyle};
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, vec2};

pub use assets::theme::{FontRole, Mode, Role, ThemeKind};

/// The vertex tint that shows a textured mesh (thumbnails, patterns) unchanged. It
/// multiplies the texture; it is not a colour the design uses.
pub const TEXTURE_TINT: Color32 = Color32::WHITE;

/// Named text styles for the type-scale steps egui has no built-in style for.
pub const DISPLAY: &str = "display";
pub const TITLE: &str = "title";
pub const LABEL: &str = "label";
pub const CAPTION: &str = "caption";

/// A loaded theme: colours for every [`Role`] plus the shared tokens.
#[derive(Clone, Debug)]
pub struct Theme {
    kind: ThemeKind,
    mode: Mode,
    colors: [Color32; Role::COUNT],
    tokens: Tokens,
}

fn color32(c: Rgba8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn radius(px: f32) -> CornerRadius {
    CornerRadius::same(px.round().clamp(0.0, 255.0) as u8)
}

fn margin(px: f32) -> Margin {
    Margin::same(px.round().clamp(0.0, 127.0) as i8)
}

impl Theme {
    /// Loads a bundled theme from the embedded token files.
    pub fn load(kind: ThemeKind) -> Result<Self, ThemeError> {
        let colors = ThemeColors::load(kind)?;
        let tokens = Tokens::load()?;
        Ok(Self {
            kind,
            mode: colors.mode,
            colors: std::array::from_fn(|i| color32(colors.get(Role::ALL[i]))),
            tokens,
        })
    }

    pub fn kind(&self) -> ThemeKind {
        self.kind
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The colour for a role, for custom painting.
    pub fn color(&self, role: Role) -> Color32 {
        self.colors[role.index()]
    }

    /// Type, spacing, size, radius, stroke and opacity tokens.
    pub fn tokens(&self) -> &Tokens {
        &self.tokens
    }

    /// The font for a type-scale step (400 or 500 picks the regular or medium family).
    pub fn font(&self, style: &TypeStyle) -> FontId {
        let family = match (style.font, style.weight >= 500) {
            (FontRole::Ui, false) => FontFamily::Proportional,
            (FontRole::Ui, true) => crate::fonts::medium(),
            (FontRole::Mono, false) => FontFamily::Monospace,
            (FontRole::Mono, true) => crate::fonts::mono_medium(),
        };
        FontId::new(style.size_px, family)
    }

    /// The egui style for this theme.
    pub fn style(&self) -> egui::Style {
        let c = |role| self.color(role);
        let t = &self.tokens;
        let hairline = |role| Stroke::new(1.0, c(role));
        let control = radius(t.radius.control);
        let floating = radius(t.radius.floating);

        let mut style = egui::Style::default();
        let v = &mut style.visuals;
        *v = match self.mode {
            Mode::Dark => egui::Visuals::dark(),
            Mode::Light => egui::Visuals::light(),
        };

        v.override_text_color = None;
        v.weak_text_color = Some(c(Role::TextMuted));
        v.hyperlink_color = c(Role::Accent);
        v.faint_bg_color = c(Role::Bg2);
        v.extreme_bg_color = c(Role::Bg0);
        v.text_edit_bg_color = Some(c(Role::Bg0));
        v.code_bg_color = c(Role::Bg2);
        v.warn_fg_color = c(Role::StateWarning);
        v.error_fg_color = c(Role::StateError);
        v.panel_fill = c(Role::Bg1);
        v.window_fill = c(Role::Bg1);
        v.window_stroke = hairline(Role::BorderSubtle);
        v.window_corner_radius = floating;
        v.menu_corner_radius = floating;
        let shadow = Shadow {
            offset: [
                0,
                t.rules
                    .floating_shadow
                    .offset_y_px
                    .round()
                    .clamp(-128.0, 127.0) as i8,
            ],
            blur: t.rules.floating_shadow.blur_px.round().clamp(0.0, 255.0) as u8,
            spread: 0,
            color: c(Role::GlassShadow),
        };
        v.window_shadow = shadow;
        v.popup_shadow = shadow;
        v.selection.bg_fill = c(Role::AccentSoft);
        v.selection.stroke = Stroke::new(1.0, c(Role::Accent));
        v.text_cursor.stroke = Stroke::new(t.rules.focus_ring.width_px, c(Role::Accent));
        v.disabled_alpha = t.opacity.disabled;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = c(Role::Bg1);
        w.noninteractive.weak_bg_fill = c(Role::Bg1);
        w.noninteractive.bg_stroke = hairline(Role::BorderSubtle);
        w.noninteractive.fg_stroke = Stroke::new(1.0, c(Role::TextPrimary));
        w.noninteractive.corner_radius = control;
        // Secondary controls at rest: bg/2 with a border/subtle hairline.
        w.inactive.bg_fill = c(Role::Bg2);
        w.inactive.weak_bg_fill = c(Role::Bg2);
        w.inactive.bg_stroke = hairline(Role::BorderSubtle);
        w.inactive.fg_stroke = Stroke::new(1.0, c(Role::TextPrimary));
        w.inactive.corner_radius = control;
        // Hover lifts to bg/3; controls reach 3:1 with border/strong.
        w.hovered.bg_fill = c(Role::Bg3);
        w.hovered.weak_bg_fill = c(Role::Bg3);
        w.hovered.bg_stroke = hairline(Role::BorderStrong);
        w.hovered.fg_stroke = Stroke::new(1.5, c(Role::TextPrimary));
        w.hovered.corner_radius = control;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = c(Role::Bg3);
        w.active.weak_bg_fill = c(Role::Bg3);
        w.active.bg_stroke = Stroke::new(1.0, c(Role::Accent));
        w.active.fg_stroke = Stroke::new(2.0, c(Role::TextPrimary));
        w.active.corner_radius = control;
        w.active.expansion = 0.0;
        w.open.bg_fill = c(Role::Bg3);
        w.open.weak_bg_fill = c(Role::Bg3);
        w.open.bg_stroke = hairline(Role::BorderStrong);
        w.open.fg_stroke = Stroke::new(1.0, c(Role::TextPrimary));
        w.open.corner_radius = control;

        // Pro density: 8 px gutters, 24 px controls, 12 px labels on buttons.
        let s = &mut style.spacing;
        s.item_spacing = vec2(t.spacing.s8, t.spacing.s4);
        s.button_padding = vec2(
            t.spacing.s8,
            ((t.sizes.control_pro - t.type_scale.label.line_height_px) / 2.0).max(0.0),
        );
        s.interact_size = vec2(t.sizes.control_pro * 2.0, t.sizes.control_pro);
        s.window_margin = margin(t.spacing.s12);
        s.menu_margin = margin(t.spacing.s4);
        s.indent = t.spacing.s16;
        s.icon_width = t.icon.sm;
        s.icon_width_inner = t.icon.sm / 2.0;
        s.icon_spacing = t.spacing.s4;

        let ts = &t.type_scale;
        style.text_styles = BTreeMap::from([
            (TextStyle::Small, self.font(&ts.caption)),
            (TextStyle::Body, self.font(&ts.body)),
            (TextStyle::Button, self.font(&ts.label)),
            (TextStyle::Heading, self.font(&ts.title)),
            (TextStyle::Monospace, self.font(&ts.mono)),
            (TextStyle::Name(DISPLAY.into()), self.font(&ts.display)),
            (TextStyle::Name(TITLE.into()), self.font(&ts.title)),
            (TextStyle::Name(LABEL.into()), self.font(&ts.label)),
            (TextStyle::Name(CAPTION.into()), self.font(&ts.caption)),
        ]);
        style
    }

    /// Installs the fonts (once per context) and this theme's style, and makes it the
    /// context's current theme. Cheap enough to call on a theme switch, not per frame.
    pub fn apply(self: &Arc<Self>, ctx: &egui::Context) {
        crate::fonts::install(ctx);
        let egui_theme = match self.mode {
            Mode::Dark => egui::Theme::Dark,
            Mode::Light => egui::Theme::Light,
        };
        ctx.set_style_of(egui_theme, self.style());
        ctx.set_theme(egui_theme);
        ctx.data_mut(|d| d.insert_temp(current_id(), self.clone()));
    }
}

fn current_id() -> egui::Id {
    egui::Id::new("glass.theme.current")
}

/// A bundled theme, loaded once per process. The token files are embedded and checked
/// by tests; if one were ever unreadable the error is logged and egui's own visuals are
/// used for every role rather than panicking.
pub fn builtin(kind: ThemeKind) -> Arc<Theme> {
    static THEMES: LazyLock<[Arc<Theme>; 3]> =
        LazyLock::new(|| ThemeKind::ALL.map(|kind| Arc::new(load_or_fallback(kind))));
    let index = ThemeKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
    THEMES[index].clone()
}

fn load_or_fallback(kind: ThemeKind) -> Theme {
    Theme::load(kind).unwrap_or_else(|error| {
        tracing::error!("theme {}: {error}; using egui's visuals", kind.name());
        let (mode, visuals) = match kind {
            ThemeKind::Light => (Mode::Light, egui::Visuals::light()),
            _ => (Mode::Dark, egui::Visuals::dark()),
        };
        Theme {
            kind,
            mode,
            colors: [visuals.text_color(); Role::COUNT],
            tokens: Tokens::load().unwrap_or(FALLBACK_TOKENS),
        }
    })
}

/// `tokens.json` as Rust, used only if the embedded file could not be read; a test keeps
/// the two equal.
const FALLBACK_TOKENS: Tokens = {
    use assets::theme::*;
    const fn ty(font: FontRole, weight: u16, size_px: f32, line_height_px: f32) -> TypeStyle {
        TypeStyle {
            font,
            weight,
            size_px,
            line_height_px,
            letter_spacing_em: 0.0,
            caps_letter_spacing_em: None,
        }
    }
    const fn tracked(style: TypeStyle, letter_spacing_em: f32) -> TypeStyle {
        TypeStyle {
            letter_spacing_em,
            ..style
        }
    }
    Tokens {
        type_scale: TypeScale {
            display: tracked(ty(FontRole::Ui, 500, 24.0, 32.0), -0.02),
            title: tracked(ty(FontRole::Ui, 500, 18.0, 24.0), -0.01),
            body: ty(FontRole::Ui, 400, 14.0, 20.0),
            label: ty(FontRole::Ui, 400, 12.0, 16.0),
            caption: TypeStyle {
                caps_letter_spacing_em: Some(0.06),
                ..ty(FontRole::Ui, 400, 11.0, 14.0)
            },
            mono: ty(FontRole::Mono, 400, 12.0, 16.0),
        },
        spacing: Spacing {
            s2: 2.0,
            s4: 4.0,
            s8: 8.0,
            s12: 12.0,
            s16: 16.0,
            s24: 24.0,
            s32: 32.0,
        },
        sizes: Sizes {
            control_pro: 24.0,
            control_beginner: 32.0,
            row_pro: 28.0,
            row_beginner: 40.0,
        },
        radius: Radius {
            panel: 0.0,
            control: 4.0,
            floating: 8.0,
            pill: 999.0,
        },
        icon: IconSizes { sm: 16.0, md: 20.0 },
        stroke: Strokes {
            icon: 1.5,
            selected_clip_outline: 1.5,
        },
        opacity: Opacity {
            disabled: 0.4,
            glass: 0.72,
        },
        rules: Rules {
            floating_shadow: FloatingShadow {
                offset_y_px: 16.0,
                blur_px: 48.0,
            },
            focus_ring: FocusRing {
                width_px: 2.0,
                gap_px: 2.0,
            },
            backdrop_blur_px: 24.0,
            danger_fill_alpha: 0.14,
        },
    }
};

/// Applies a bundled theme to `ctx`: the run-time theme switch.
pub fn set(ctx: &egui::Context, kind: ThemeKind) {
    builtin(kind).apply(ctx);
}

/// The theme's name for the theme picker, in the current locale.
pub fn label(kind: ThemeKind) -> String {
    match kind {
        ThemeKind::Dark => crate::t!("theme.dark"),
        ThemeKind::Light => crate::t!("theme.light"),
        ThemeKind::HighContrast => crate::t!("theme.high_contrast"),
    }
}

/// The theme last applied to `ctx`, or the default (dark) when none was.
pub fn current(ctx: &egui::Context) -> Arc<Theme> {
    ctx.data(|d| d.get_temp::<Arc<Theme>>(current_id()))
        .unwrap_or_else(|| builtin(ThemeKind::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_loads_with_every_role() {
        for kind in ThemeKind::ALL {
            let theme = Theme::load(kind).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert_eq!(theme.kind(), kind);
            for &role in Role::ALL {
                // Only the high-contrast glass roles are cleared to transparent.
                let cleared = kind == ThemeKind::HighContrast
                    && matches!(
                        role,
                        Role::GlassTop | Role::GlassBottom | Role::GlassHighlight
                    );
                assert_eq!(
                    theme.color(role).a() == 0,
                    cleared,
                    "{kind:?} {}",
                    role.key()
                );
            }
        }
    }

    #[test]
    fn style_uses_the_tokens() {
        let theme = Theme::load(ThemeKind::Dark).expect("dark");
        let style = theme.style();
        let v = &style.visuals;
        assert!(v.dark_mode);
        assert_eq!(v.panel_fill, theme.color(Role::Bg1));
        assert_eq!(v.selection.bg_fill, theme.color(Role::AccentSoft));
        assert_eq!(v.selection.stroke.color, theme.color(Role::Accent));
        assert_eq!(v.widgets.hovered.bg_fill, theme.color(Role::Bg3));
        assert_eq!(v.window_corner_radius, CornerRadius::same(8));
        assert_eq!(v.widgets.inactive.corner_radius, CornerRadius::same(4));
        assert_eq!(style.spacing.interact_size.y, 24.0);
        assert_eq!(style.text_styles[&TextStyle::Body].size, 14.0);
        assert_eq!(
            style.text_styles[&TextStyle::Heading].family,
            crate::fonts::medium()
        );
        assert!(
            !Theme::load(ThemeKind::Light)
                .expect("light")
                .style()
                .visuals
                .dark_mode
        );
    }

    #[test]
    fn fallback_tokens_match_the_file() {
        assert_eq!(Tokens::load().expect("tokens"), FALLBACK_TOKENS);
    }
}
