//! egui with Windows' own fonts, loaded at run time, so no font ships with the app (egui's
//! `default_fonts` are off: their licences aren't MIT-style).
//!
//! Generic, for other apps to copy too: nothing here is Replica's. Text is Segoe UI Variable
//! (Windows 11), then Segoe UI, with Segoe UI Symbol after them for the symbols they lack.
//! [`SEMIBOLD`] is the same at weight 600 (the variable font's `wght` axis, or Segoe UI
//! Semibold). Monospace is Cascadia Mono, then Consolas. Each file is optional: one that isn't
//! there is left out, and [`install`] fails only when no text font is found.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::epaint::text::VariationCoords;
use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontTweak};

/// One font file in a family, at a weight if it's a variable font.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    pub file: &'static str,
    /// The `wght` axis, for a variable font; `None` keeps the file's default.
    pub weight: Option<f32>,
}

const fn face(file: &'static str) -> Face {
    Face { file, weight: None }
}

/// The text fonts, in the order egui tries them for a glyph.
pub const PROPORTIONAL: &[Face] = &[
    face("SegUIVar.ttf"),
    face("segoeui.ttf"),
    face("seguisym.ttf"),
];
/// Semibold text, for titles.
pub const SEMIBOLD_FACES: &[Face] = &[
    Face {
        file: "SegUIVar.ttf",
        weight: Some(600.0),
    },
    face("seguisb.ttf"),
    face("seguisym.ttf"),
];
/// The monospace fonts. Other families fall back to the text fonts after their own.
pub const MONOSPACE: &[Face] = &[face("CascadiaMono.ttf"), face("consola.ttf")];

/// The semibold family's name in egui.
pub static SEMIBOLD: std::sync::LazyLock<FontFamily> =
    std::sync::LazyLock::new(|| FontFamily::Name("semibold".into()));

/// The families [`install`] sets up.
pub fn families() -> Vec<(FontFamily, &'static [Face])> {
    vec![
        (FontFamily::Proportional, PROPORTIONAL),
        (SEMIBOLD.clone(), SEMIBOLD_FACES),
        (FontFamily::Monospace, MONOSPACE),
    ]
}

/// Windows' fonts folder (`%WINDIR%\Fonts`).
pub fn fonts_dir() -> PathBuf {
    let windows = std::env::var_os("WINDIR")
        .or_else(|| std::env::var_os("SystemRoot"))
        .unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&windows).join("Fonts")
}

/// The font definitions for `families` from the files in `dir`, and the files that weren't
/// there. Every family but the proportional one falls back to the proportional fonts after its
/// own. `None` if no proportional font was found.
pub fn definitions(
    dir: &Path,
    families: &[(FontFamily, &[Face])],
) -> (Option<FontDefinitions>, Vec<String>) {
    let mut fonts = FontDefinitions::empty();
    let mut missing = Vec::new();
    let mut files: HashMap<&str, Option<FontData>> = HashMap::new();
    let mut lists: Vec<(FontFamily, Vec<String>)> = Vec::new();
    for (family, faces) in families {
        let mut names = Vec::new();
        for f in *faces {
            let data =
                files
                    .entry(f.file)
                    .or_insert_with(|| match std::fs::read(dir.join(f.file)) {
                        Ok(bytes) => Some(FontData::from_owned(bytes)),
                        Err(_) => {
                            missing.push(f.file.to_owned());
                            None
                        }
                    });
            let Some(data) = data else { continue };
            let name = match f.weight {
                None => f.file.to_owned(),
                Some(w) => format!("{}@{w}", f.file),
            };
            if !fonts.font_data.contains_key(&name) {
                // Cloning shares the file's bytes.
                let mut data = data.clone();
                if let Some(w) = f.weight {
                    data = data.tweak(FontTweak {
                        coords: VariationCoords::new([(b"wght", w)]),
                        ..FontTweak::default()
                    });
                }
                fonts.font_data.insert(name.clone(), Arc::new(data));
            }
            names.push(name);
        }
        lists.push((family.clone(), names));
    }
    let text = lists
        .iter()
        .find(|(f, _)| *f == FontFamily::Proportional)
        .map(|(_, names)| names.clone())
        .unwrap_or_default();
    if text.is_empty() {
        return (None, missing);
    }
    for (family, mut names) in lists {
        if family != FontFamily::Proportional {
            names.extend(
                text.iter()
                    .filter(|t| !names.contains(t))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        fonts.families.insert(family, names);
    }
    (Some(fonts), missing)
}

/// No text font was found: where it looked and the files that weren't there. The app words
/// it for the user in its own strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoTextFont {
    pub dir: PathBuf,
    pub missing: Vec<String>,
}

/// Gives `ctx` Windows' fonts, or says what was missing when no text font would do.
pub fn install(ctx: &egui::Context) -> Result<(), NoTextFont> {
    let dir = fonts_dir();
    let (fonts, missing) = definitions(&dir, &families());
    if !missing.is_empty() {
        tracing::warn!(?missing, dir = %dir.display(), "some system fonts weren't found");
    }
    match fonts {
        Some(fonts) => {
            ctx.set_fonts(fonts);
            Ok(())
        }
        None => Err(NoTextFont { dir, missing }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_pc_has_the_text_font() {
        let (fonts, _) = definitions(&fonts_dir(), &families());
        let fonts = fonts.expect("Segoe UI");
        let text = &fonts.families[&FontFamily::Proportional];
        assert!(text.iter().any(|f| f == "segoeui.ttf"));
        // Monospace and semibold fall back to the text fonts.
        for family in [FontFamily::Monospace, SEMIBOLD.clone()] {
            assert!(fonts.families[&family].iter().any(|f| f == "segoeui.ttf"));
        }
    }

    #[test]
    fn a_variable_face_gets_its_weight_and_shares_the_file() {
        let (fonts, _) = definitions(&fonts_dir(), &families());
        let fonts = fonts.unwrap();
        if let Some(semibold) = fonts.font_data.get("SegUIVar.ttf@600") {
            assert_eq!(
                semibold.tweak.coords,
                VariationCoords::new([(b"wght", 600.0)])
            );
            let regular = &fonts.font_data["SegUIVar.ttf"];
            assert!(Arc::ptr_eq(&semibold.font, &regular.font));
            assert_eq!(fonts.families[&SEMIBOLD][0], "SegUIVar.ttf@600");
        }
    }

    #[test]
    fn missing_fonts_are_left_out_and_none_is_an_error() {
        const A: &[Face] = &[face("a.ttf")];
        const B: &[Face] = &[face("b.ttf")];
        let empty = tempfile::tempdir().unwrap();
        let (fonts, missing) = definitions(
            empty.path(),
            &[(FontFamily::Proportional, A), (FontFamily::Monospace, B)],
        );
        assert!(fonts.is_none());
        assert_eq!(missing, ["a.ttf", "b.ttf"]);

        const TEXT: &[Face] = &[face("segoeui.ttf")];
        const NOPE: &[Face] = &[face("nope.ttf")];
        let (fonts, missing) = definitions(
            &fonts_dir(),
            &[
                (FontFamily::Proportional, TEXT),
                (FontFamily::Monospace, NOPE),
            ],
        );
        assert_eq!(
            fonts.unwrap().families[&FontFamily::Monospace],
            ["segoeui.ttf"]
        );
        assert_eq!(missing, ["nope.ttf"]);
    }
}
