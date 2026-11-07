//! The desktop app's themes (its THEMES table), picked by the same
//! `settings.theme` id in store.json.

use slint::{Color, ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, Theme, ThemeSwatch};

/// Colors of a palette theme: main area, cards, text, borders, accent and
/// the text on it, selected surfaces and their text, switch tracks.
struct Palette {
    light: bool,
    bg: u32,
    card: u32,
    fg: u32,
    line: u32,
    accent: u32,
    on_accent: u32,
    tonal: u32,
    on_tonal: u32,
    track: u32,
    outline: Option<u32>,
}

const fn dark(c: [u32; 9]) -> Palette {
    let [bg, card, fg, line, accent, on_accent, tonal, on_tonal, track] = c;
    Palette { light: false, bg, card, fg, line, accent, on_accent, tonal, on_tonal, track, outline: None }
}

const fn light(c: [u32; 9]) -> Palette {
    let [bg, card, fg, line, accent, on_accent, tonal, on_tonal, track] = c;
    Palette { light: true, bg, card, fg, line, accent, on_accent, tonal, on_tonal, track, outline: None }
}

/// Material's colors in DUORAY purple, for "system", "dark" and "light".
const MATERIAL_DARK: Palette = dark([0x1c1b1f, 0x2a282d, 0xe6e1e5, 0x938f99, 0xd0bcff, 0x381e72, 0x4a4458, 0xe8def8, 0x36343b]);
const MATERIAL_LIGHT: Palette = light([0xf8f3f9, 0xece6f0, 0x1d1b20, 0x79747e, 0x6750a4, 0xffffff, 0xe8def8, 0x1e192b, 0xe7e0ec]);

/// id (stored), name (shown), palette (None: Material, following the
/// system for "system").
const THEMES: &[(&str, &str, Option<Palette>)] = &[
    ("system", "Как в системе", None),
    ("dark", "Тёмная", Some(MATERIAL_DARK)),
    ("light", "Светлая", Some(MATERIAL_LIGHT)),
    ("graphite", "Графитовая", Some(dark([0x2f3136, 0x25272b, 0xe4e5e8, 0x8f9197, 0xd0bcff, 0x381e72, 0x45474d, 0xe4e5e8, 0x3a3c41]))),
    (
        "oled",
        "OLED (чёрная)",
        Some(Palette { outline: Some(0x2c2c30), ..dark([0x000000, 0x0a0a0b, 0xe6e6e6, 0x5a5a5e, 0xd0bcff, 0x381e72, 0x2a2a2e, 0xe6e6e6, 0x26262a]) }),
    ),
    ("catppuccin-frappe", "Catppuccin Frappé", Some(dark([0x303446, 0x292c3c, 0xc6d0f5, 0x737994, 0xca9ee6, 0x303446, 0x51576d, 0xc6d0f5, 0x414559]))),
    ("catppuccin-macchiato", "Catppuccin Macchiato", Some(dark([0x24273a, 0x1e2030, 0xcad3f5, 0x6e738d, 0xc6a0f6, 0x24273a, 0x494d64, 0xcad3f5, 0x363a4f]))),
    ("catppuccin", "Catppuccin Mocha", Some(dark([0x1e1e2e, 0x181825, 0xcdd6f4, 0x6c7086, 0xcba6f7, 0x1e1e2e, 0x45475a, 0xcdd6f4, 0x313244]))),
    ("catppuccin-latte", "Catppuccin Latte", Some(light([0xeff1f5, 0xe6e9ef, 0x4c4f69, 0x9ca0b0, 0x8839ef, 0xffffff, 0xccd0da, 0x4c4f69, 0xdce0e8]))),
    ("coral", "Коралловая", Some(light([0xfffaf9, 0xfbeeeb, 0x3d3838, 0xc9a6ac, 0xc75545, 0xffffff, 0xf7d6d0, 0x3d3838, 0xecd8d6]))),
    ("coral-dark", "Тёмно-коралловая", Some(dark([0x2a2022, 0x221a1c, 0xf2e4e2, 0x8c6f72, 0xf08a76, 0x3a1510, 0x4a3336, 0xf2e4e2, 0x3b2c2f]))),
    ("tokyo-night", "Tokyo Night", Some(dark([0x1a1b26, 0x16161e, 0xc0caf5, 0x565f89, 0x7aa2f7, 0x1a1b26, 0x343a55, 0xc0caf5, 0x292e42]))),
    ("dracula", "Dracula", Some(dark([0x282a36, 0x21222c, 0xf8f8f2, 0x6272a4, 0xbd93f9, 0x282a36, 0x44475a, 0xf8f8f2, 0x343746]))),
    ("nord", "Nord", Some(dark([0x2e3440, 0x272c36, 0xeceff4, 0x616e88, 0x88c0d0, 0x2e3440, 0x434c5e, 0xeceff4, 0x3b4252]))),
    ("one-dark", "One Dark", Some(dark([0x282c34, 0x21252b, 0xd7dae0, 0x5c6370, 0x61afef, 0x282c34, 0x3e4451, 0xd7dae0, 0x2c313a]))),
    ("gruvbox", "Gruvbox", Some(dark([0x282828, 0x1d2021, 0xebdbb2, 0x928374, 0xfabd2f, 0x282828, 0x504945, 0xebdbb2, 0x3c3836]))),
    ("rose-pine", "Rosé Pine", Some(dark([0x1f1d2e, 0x191724, 0xe0def4, 0x6e6a86, 0xc4a7e7, 0x191724, 0x403d52, 0xe0def4, 0x26233a]))),
    (
        "matrix",
        "Матрица",
        Some(Palette { outline: Some(0x15402a), ..dark([0x06100a, 0x030805, 0xc4f5cf, 0x2a6b3a, 0x39e66b, 0x022110, 0x10301a, 0xd2fadb, 0x0d2414]) }),
    ),
];

fn color(rgb: u32) -> Color {
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Index of the theme with this id; an unknown one (from a newer desktop
/// version) is "dark".
pub fn index_of(id: &str) -> usize {
    THEMES.iter().position(|t| t.0 == id).or_else(|| THEMES.iter().position(|t| t.0 == "dark")).unwrap_or(0)
}

pub fn id_at(index: usize) -> &'static str {
    THEMES.get(index).map_or("dark", |t| t.0)
}

/// Paints the window with theme `id` at `text_scale` percent.
pub fn apply(ui: &AppWindow, id: &str, text_scale: u32) {
    let t = ui.global::<Theme>();
    let (_, _, palette) = &THEMES[index_of(id)];
    let p = palette.as_ref().unwrap_or(if t.get_system_light() { &MATERIAL_LIGHT } else { &MATERIAL_DARK });
    t.set_follow_system(palette.is_none());
    t.set_c_light(p.light);
    t.set_c_bg(color(p.bg));
    t.set_c_card(color(p.card));
    t.set_c_fg(color(p.fg));
    t.set_c_line(color(p.line));
    t.set_c_accent(color(p.accent));
    t.set_c_on_accent(color(p.on_accent));
    t.set_c_tonal(color(p.tonal));
    t.set_c_on_tonal(color(p.on_tonal));
    t.set_c_track(color(p.track));
    t.set_c_outline(p.outline.map_or(Color::from_argb_u8(0, 0, 0, 0), color));
    t.set_text_scale(text_scale.clamp(80, 150) as f32 / 100.0);
}

/// The tiles of the theme picker.
pub fn swatches(current: &str) -> ModelRc<ThemeSwatch> {
    let current = index_of(current);
    let tiles: Vec<ThemeSwatch> = THEMES
        .iter()
        .enumerate()
        .map(|(i, (_, name, p))| {
            let p = p.as_ref().unwrap_or(&MATERIAL_DARK);
            ThemeSwatch {
                name: (*name).into(),
                bg: color(p.bg),
                card: color(p.card),
                fg: color(p.fg),
                accent: color(p.accent),
                active: i == current,
            }
        })
        .collect();
    ModelRc::new(VecModel::from(tiles))
}
