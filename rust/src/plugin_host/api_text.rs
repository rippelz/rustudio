//! TextService:GetTextSize / GetTextBoundsAsync with real font metrics
//! (plugins measure their widget text with them; game scripts too). The
//! text is laid out with Roblox's own font files from Studio's content
//! folder (fonts/families/<Family>.json → the nearest face's file), the
//! same em mapping the GUI walk draws with (mech::GUI_FONT_EM_PER_TEXT_SIZE:
//! a line is TextSize tall), wrapped at the frame width, and rounded up to
//! whole pixels as Studio's results are. Studio 0.747's numbers for the
//! same calls are in checks/plugins/services/captures/history.txt
//! (`e.textsize`, `e.textbounds`); the unit test pins them.
//! Faces only on Roblox's servers (rbxassetid://) aren't fetched here: the
//! nearest local face of the family is used.

use std::collections::HashMap;
use std::sync::Arc;

use egui::epaint::text::{FontData, FontDefinitions, Fonts, TextOptions};
use egui::{FontFamily, FontId};
use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::{Ref, Variant};

use super::*;

/// Enum.Font name → (family json, weight, italic), as Font.fromEnum.
fn legacy(name: &str) -> (&'static str, u32, bool) {
    match name {
        "Legacy" => ("LegacyArial", 400, false),
        "Arial" => ("Arimo", 400, false),
        "ArialBold" => ("Arimo", 700, false),
        "SourceSans" => ("SourceSansPro", 400, false),
        "SourceSansBold" => ("SourceSansPro", 700, false),
        "SourceSansSemibold" => ("SourceSansPro", 600, false),
        "SourceSansLight" => ("SourceSansPro", 300, false),
        "SourceSansItalic" => ("SourceSansPro", 400, true),
        "Bodoni" => ("AccanthisADFStd", 400, false),
        "Garamond" => ("Guru", 400, false),
        "Cartoon" => ("ComicNeueAngular", 400, false),
        "Code" => ("Inconsolata", 400, false),
        "Highway" => ("HighwayGothic", 400, false),
        "SciFi" => ("Zekton", 400, false),
        "Arcade" => ("PressStart2P", 400, false),
        "Fantasy" => ("Balthazar", 400, false),
        "Antique" => ("RomanAntique", 400, false),
        "Gotham" => ("Montserrat", 400, false),
        "GothamMedium" | "GothamSemibold" => ("Montserrat", 500, false),
        "GothamBold" => ("Montserrat", 700, false),
        "GothamBlack" => ("Montserrat", 900, false),
        "BuilderSans" => ("BuilderSans", 400, false),
        "BuilderSansMedium" => ("BuilderSans", 500, false),
        "BuilderSansBold" => ("BuilderSans", 700, false),
        "BuilderSansExtraBold" => ("BuilderSans", 800, false),
        "RobotoMono" => ("RobotoMono", 400, false),
        _ => ("SourceSansPro", 400, false),
    }
}

/// The nearest local face file of a family (`rbxasset://fonts/families/X.json`).
fn face_path(family_json: &str, weight: u32, italic: bool) -> Option<std::path::PathBuf> {
    let p = rustudio_core::assets::resolve(family_json, None)?;
    let j: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
    let mut best: Option<(u32, std::path::PathBuf)> = None;
    for f in j.get("faces")?.as_array()? {
        let id = f.get("assetId")?.as_str()?;
        if !id.starts_with("rbxasset://") {
            continue;
        }
        let Some(file) = rustudio_core::assets::resolve(id, None).filter(|p| p.is_file()) else { continue };
        let w = f.get("weight")?.as_u64()? as u32;
        let it = f.get("style")?.as_str()? == "italic";
        let cost = w.abs_diff(weight) + if it != italic { 1000 } else { 0 };
        if best.as_ref().is_none_or(|(c, _)| cost < *c) {
            best = Some((cost, file));
        }
    }
    best.map(|(_, f)| f)
}

struct Measurer {
    defs: FontDefinitions,
    fonts: Fonts,
    /// face file → (family, em per TextSize)
    faces: HashMap<std::path::PathBuf, (FontFamily, f32)>,
}

thread_local! {
    static M: std::cell::RefCell<Option<Measurer>> = const { std::cell::RefCell::new(None) };
}

/// The text's size in pixels: Studio's GetTextSize (`width` the frame width
/// it wraps at). Lines are TextSize tall; widths round up.
pub fn text_size(text: &str, size: f32, family_json: &str, weight: u32, italic: bool, width: f32) -> [f32; 2] {
    let legacy_scale = if family_json.ends_with("/LegacyArial.json") {
        rustudio_core::mech::GUI_LEGACY_TEXT_SCALE
    } else {
        1.0
    };
    let eff_size = (size * legacy_scale).max(1.0);
    let lines = text.split('\n').count().max(1) as f32;
    if text.is_empty() {
        return [0.0, size];
    }
    let path = face_path(family_json, weight, italic)
        .or_else(|| face_path("rbxasset://fonts/families/SourceSansPro.json", 400, false));
    let Some(path) = path else {
        // No font files at all: Studio's average advance (half the size).
        let w = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0) as f32 * eff_size * 0.5;
        return [w.ceil(), size * lines];
    };
    M.with(|m| {
        let mut g = m.borrow_mut();
        let ms = g.get_or_insert_with(|| {
            let defs = FontDefinitions::empty();
            Measurer { fonts: Fonts::new(TextOptions::default(), defs.clone()), defs, faces: HashMap::new() }
        });
        if !ms.faces.contains_key(&path) {
            let name = path.display().to_string();
            if let Ok(bytes) = std::fs::read(&path) {
                ms.defs.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
                ms.defs.families.insert(FontFamily::Name(name.clone().into()), vec![name.clone()]);
                ms.fonts = Fonts::new(TextOptions::default(), ms.defs.clone());
                ms.fonts.set_measure_only(true);
                let fam = FontFamily::Name(name.into());
                let line = ms.fonts.with_pixels_per_point(1.0).row_height(&FontId::new(100.0, fam.clone())) / 100.0;
                let em = rustudio_core::mech::GUI_FONT_EM_PER_TEXT_SIZE / line.max(0.5);
                ms.faces.insert(path.clone(), (fam, em));
            }
        }
        let Some((fam, em)) = ms.faces.get(&path).cloned() else {
            return [0.0, size * lines];
        };
        let mut f = egui::TextFormat::simple(FontId::new(eff_size * em, fam), egui::Color32::WHITE);
        f.line_height = Some(size);
        f.studio_text_size = Some(eff_size);
        let mut job = egui::text::LayoutJob::default();
        job.append(text, 0.0, f);
        job.wrap.max_width = if width > 0.0 { width } else { f32::INFINITY };
        job.keep_trailing_whitespace = true;
        job.round_output_to_gui = false;
        let g = ms.fonts.with_pixels_per_point(1.0).layout_job(job);
        let rows = g.rows.len().max(1) as f32;
        [g.rect.width().ceil(), (rows * size).max(size)]
    })
}

pub fn call(lua: &Lua, class: &str, name: &str, _this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if class != "TextService" {
        return None;
    }
    let r = match name {
        "GetTextSize" => (|| {
            require_args(args, 4)?;
            let text = arg_string(args, 1)?;
            let size = match argv(args, 2) {
                Value::Integer(n) => n as f32,
                Value::Number(n) => n as f32,
                v => return Err(rt_err(format!("Unable to cast {} to float", v.type_name()))),
            };
            let font = opt_ud::<rustudio_engine::ltypes::LEnumItem>(&argv(args, 3))
                .filter(|e| e.enum_type == "Font")
                .map(|e| e.name.to_string())
                .ok_or_else(|| rt_err("Unable to cast value to Font"))?;
            let frame = opt_ud::<rustudio_engine::ltypes::LVector2>(&argv(args, 4)).ok_or_else(|| rt_err("Unable to cast value to Vector2"))?;
            let (fam, w, it) = legacy(&font);
            let s = text_size(&text, size, &format!("rbxasset://fonts/families/{fam}.json"), w, it, frame.0.x);
            ret1(Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LVector2(glam::Vec2::new(s[0], s[1])))?))
        })(),
        "GetTextBoundsAsync" => (|| {
            let p = opt_inst(&argv(args, 1)).ok_or_else(|| rt_err("Argument 1 missing or nil"))?;
            let (text, size, width, font) = {
                let w = world(lua)?;
                let d = w.d();
                let text = match d.prop(p, "Text") {
                    Some(Variant::String(s)) => s,
                    _ => String::new(),
                };
                let size = match d.prop(p, "Size") {
                    Some(Variant::Float32(f)) => f,
                    Some(Variant::Float64(f)) => f as f32,
                    _ => 14.0,
                };
                let width = match d.prop(p, "Width") {
                    Some(Variant::Float32(f)) => f,
                    Some(Variant::Float64(f)) => f as f32,
                    _ => 0.0,
                };
                let font = match d.prop(p, "Font") {
                    Some(Variant::Font(f)) => Some(f),
                    _ => None,
                };
                (text, size, width, font)
            };
            let (fam, weight, italic) = match &font {
                Some(f) => {
                    let fam = if f.family.ends_with("/GothamSSm.json") {
                        "rbxasset://fonts/families/Montserrat.json".to_string()
                    } else {
                        f.family.clone()
                    };
                    let w = {
                        use rbx_dom_weak::types::FontWeight as W;
                        match f.weight {
                            W::Thin => 100,
                            W::ExtraLight => 200,
                            W::Light => 300,
                            W::Medium => 500,
                            W::SemiBold => 600,
                            W::Bold => 700,
                            W::ExtraBold => 800,
                            W::Heavy => 900,
                            _ => 400,
                        }
                    };
                    (fam, w, matches!(f.style, rbx_dom_weak::types::FontStyle::Italic))
                }
                None => ("rbxasset://fonts/families/SourceSansPro.json".to_string(), 400, false),
            };
            let s = text_size(&text, size, &fam, weight, italic, width);
            ret1(Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LVector2(glam::Vec2::new(s[0], s[1])))?))
        })(),
        _ => return None,
    };
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Studio 0.747's GetTextSize for the probe's cases
    /// (captures/history.txt e.textsize): exact match to the pixel.
    #[test]
    fn get_text_size_matches_studio() {
        if face_path("rbxasset://fonts/families/SourceSansPro.json", 400, false).is_none() {
            eprintln!("no Studio content folder: skipped");
            return;
        }
        let cases: &[(&str, f32, &str, f32, [f32; 2])] = &[
            ("Hello world", 14.0, "SourceSans", 1000.0, [57.0, 14.0]),
            ("Hello world", 18.0, "Gotham", 1000.0, [88.0, 18.0]),
            ("The quick brown fox jumps", 24.0, "Arial", 1000.0, [258.0, 24.0]),
            ("WWW iii 000", 12.0, "Code", 1000.0, [66.0, 12.0]),
            ("The quick brown fox jumps over the lazy dog", 16.0, "SourceSans", 100.0, [94.0, 48.0]),
            ("", 14.0, "SourceSans", 1000.0, [0.0, 14.0]),
            ("line1\nline2", 20.0, "SourceSansBold", 1000.0, [38.0, 40.0]),
            ("Builder", 15.0, "BuilderSans", 1000.0, [40.0, 15.0]),
            ("Roboto Mono", 13.0, "RobotoMono", 1000.0, [66.0, 13.0]),
        ];
        let mut bad = Vec::new();
        for (t, s, f, w, want) in cases {
            let (fam, wt, it) = legacy(f);
            let got = text_size(t, *s, &format!("rbxasset://fonts/families/{fam}.json"), wt, it, *w);
            println!("CAL {t:?} {s} {f}: {got:?} studio {want:?}");
            if got != *want {
                bad.push(format!("{t:?} {s} {f}: {got:?} vs Studio {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }
}
