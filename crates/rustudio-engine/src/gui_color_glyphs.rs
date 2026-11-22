//! Colour glyphs (COLR v0 layers in CPAL colours) for the fonts Roblox falls
//! back to: content/fonts/RobloxEmoji.ttf draws U+E000, the verified badge
//! the CoreScripts append to names (VerifiedBadges' VERIFIED_EMOJI,
//! ExpChat's Config.VERIFIED_EMOJI), as a blue badge with a white check.
//! egui draws a glyph in one colour, so the face is rebuilt with every
//! layer glyph at a codepoint of its own (U+F0000 + glyph id) and no advance
//! but the last layer's, and a colour glyph is laid out as its layers, each
//! in its palette colour, one over the other.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontFamily};

use super::ligatures::{cmap, table, R};

/// Where a layer glyph's codepoint starts (plane 15, private use).
const LAYER_BASE: u32 = 0xF0000;

pub(super) struct ColorFace {
    /// The rebuilt face, a fallback font of every family.
    pub bytes: Vec<u8>,
    /// Colour glyph → its layers bottom first: (layer codepoint, its
    /// palette colour or None for the text's own).
    layers: HashMap<char, Vec<(char, Option<Color32>)>>,
}

/// RobloxEmoji rebuilt, or None without the content folder's file.
static FACE: LazyLock<Option<ColorFace>> = LazyLock::new(|| {
    let path = rustudio_core::assets::content_dir().join("fonts/RobloxEmoji.ttf");
    rebuild(&std::fs::read(path).ok()?)
});

pub(super) fn face() -> Option<&'static ColorFace> {
    FACE.as_ref()
}

/// Families whose own face has a glyph at a colour glyph's codepoint (an
/// icon font's private use area): theirs is drawn, not the colour glyph.
static SHADOWED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Notes a face added as egui family `name`.
pub(super) fn note_face(name: &str, bytes: &[u8]) {
    let Some(face) = face() else { return };
    let covers = cmap(bytes).is_some_and(|m| m.iter().any(|(c, _)| face.layers.contains_key(c)));
    if covers {
        if let Ok(mut g) = SHADOWED.lock() {
            g.get_or_insert_with(HashSet::new).insert(name.to_string());
        }
    }
}

/// Appends `text` in `format`, each colour glyph as its layers.
pub(super) fn append(job: &mut LayoutJob, text: &str, format: TextFormat) {
    let face = match face() {
        Some(f) if text.chars().any(|c| f.layers.contains_key(&c)) => f,
        _ => return job.append(text, 0.0, format),
    };
    if let FontFamily::Name(name) = &format.font_id.family {
        if SHADOWED.lock().ok().is_some_and(|g| g.as_ref().is_some_and(|s| s.contains(&**name))) {
            return job.append(text, 0.0, format);
        }
    }
    let alpha = format.color.a() as f32 / 255.0;
    let mut start = 0;
    let mut buf = [0u8; 4];
    for (i, c) in text.char_indices() {
        let Some(layers) = face.layers.get(&c) else { continue };
        if start < i {
            job.append(&text[start..i], 0.0, format.clone());
        }
        for (ch, color) in layers {
            let mut f = format.clone();
            if let Some(col) = color {
                f.color = col.gamma_multiply(alpha);
            }
            job.append(ch.encode_utf8(&mut buf), 0.0, f);
        }
        start = i + c.len_utf8();
    }
    if start < text.len() {
        job.append(&text[start..], 0.0, format);
    }
}

/// The face with its COLR layer glyphs mapped and spaced, COLR / CPAL
/// dropped (egui draws outlines only).
fn rebuild(bytes: &[u8]) -> Option<ColorFace> {
    let f = R(bytes);
    let (colr, _) = table(bytes, b"COLR")?;
    let (cpal, _) = table(bytes, b"CPAL")?;
    let (maxp, _) = table(bytes, b"maxp")?;
    let (hhea, hhea_len) = table(bytes, b"hhea")?;
    let (hmtx, _) = table(bytes, b"hmtx")?;
    if f.u16(colr)? != 0 {
        return None;
    }
    let glyphs = f.u16(maxp + 4)? as usize;
    let long = f.u16(hhea + 34)? as usize;
    // (advance, left side bearing) per glyph
    let mut metrics: Vec<(u16, u16)> = (0..glyphs)
        .map(|g| {
            if g < long {
                Some((f.u16(hmtx + 4 * g)?, f.u16(hmtx + 4 * g + 2)?))
            } else {
                Some((f.u16(hmtx + 4 * (long - 1))?, f.u16(hmtx + 4 * long + 2 * (g - long))?))
            }
        })
        .collect::<Option<_>>()?;
    // CPAL palette 0, stored BGRA.
    let entries = f.u16(cpal + 2)? as usize;
    let records = cpal + f.u32(cpal + 8)? as usize;
    let palette_start = f.u16(cpal + 12)? as usize;
    let palette: Vec<Color32> = (0..entries)
        .map(|i| {
            let at = records + 4 * (palette_start + i);
            let b = bytes.get(at..at + 4)?;
            Some(Color32::from_rgba_unmultiplied(b[2], b[1], b[0], b[3]))
        })
        .collect::<Option<_>>()?;
    let bases = f.u16(colr + 2)? as usize;
    let base_at = colr + f.u32(colr + 4)? as usize;
    let layer_at = colr + f.u32(colr + 8)? as usize;
    let mut by_glyph: HashMap<u16, Vec<(char, Option<Color32>)>> = HashMap::new();
    let mut mapped: Vec<u16> = Vec::new();
    for b in 0..bases {
        let rec = base_at + 6 * b;
        let (base, first, n) = (f.u16(rec)?, f.u16(rec + 2)? as usize, f.u16(rec + 4)? as usize);
        let mut list = Vec::with_capacity(n);
        for l in first..first + n {
            let (g, p) = (f.u16(layer_at + 4 * l)?, f.u16(layer_at + 4 * l + 2)?);
            if g as usize >= glyphs {
                return None;
            }
            // The layers stack: only the top one moves the pen on.
            let last = l + 1 == first + n;
            metrics[g as usize].0 = if last { metrics.get(base as usize)?.0 } else { 0 };
            mapped.push(g);
            list.push((char::from_u32(LAYER_BASE + g as u32)?, if p == 0xFFFF { None } else { palette.get(p as usize).copied() }));
        }
        by_glyph.insert(base, list);
    }
    let mut map = cmap(bytes)?;
    map.sort_unstable();
    map.dedup();
    let layers: HashMap<char, Vec<(char, Option<Color32>)>> =
        map.iter().filter_map(|(c, g)| Some((*c, by_glyph.get(g)?.clone()))).collect();
    mapped.sort_unstable();
    mapped.dedup();
    map.extend(mapped.iter().filter_map(|g| Some((char::from_u32(LAYER_BASE + *g as u32)?, *g))));
    // cmap: one format 12 subtable (Windows, full Unicode).
    let mut groups: Vec<(u32, u32, u32)> = Vec::new();
    for (c, g) in &map {
        let (c, g) = (*c as u32, *g as u32);
        match groups.last_mut() {
            Some((start, end, sg)) if *end + 1 == c && *sg + (c - *start) == g => *end = c,
            _ => groups.push((c, c, g)),
        }
    }
    let mut new_cmap = Vec::new();
    new_cmap.extend(0u16.to_be_bytes());
    new_cmap.extend(1u16.to_be_bytes());
    new_cmap.extend(3u16.to_be_bytes());
    new_cmap.extend(10u16.to_be_bytes());
    new_cmap.extend(12u32.to_be_bytes());
    new_cmap.extend(12u16.to_be_bytes());
    new_cmap.extend(0u16.to_be_bytes());
    new_cmap.extend((16 + 12 * groups.len() as u32).to_be_bytes());
    new_cmap.extend(0u32.to_be_bytes());
    new_cmap.extend((groups.len() as u32).to_be_bytes());
    for (a, b, g) in &groups {
        new_cmap.extend(a.to_be_bytes());
        new_cmap.extend(b.to_be_bytes());
        new_cmap.extend(g.to_be_bytes());
    }
    let new_hmtx: Vec<u8> = metrics.iter().flat_map(|(a, l)| [a.to_be_bytes(), l.to_be_bytes()]).flatten().collect();
    let mut new_hhea = bytes.get(hhea..hhea + hhea_len)?.to_vec();
    new_hhea.get_mut(34..36)?.copy_from_slice(&(glyphs as u16).to_be_bytes());
    // Every other table as it was, these three replaced, COLR / CPAL out.
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for i in 0..f.u16(4)? as usize {
        let rec = 12 + 16 * i;
        let tag: [u8; 4] = bytes.get(rec..rec + 4)?.try_into().ok()?;
        let data = match &tag {
            b"COLR" | b"CPAL" => continue,
            b"cmap" => new_cmap.clone(),
            b"hmtx" => new_hmtx.clone(),
            b"hhea" => new_hhea.clone(),
            _ => {
                let (at, len) = (f.u32(rec + 8)? as usize, f.u32(rec + 12)? as usize);
                bytes.get(at..at + len)?.to_vec()
            }
        };
        tables.push((tag, data));
    }
    tables.sort_by(|a, b| a.0.cmp(&b.0));
    let n = tables.len() as u16;
    let pow = 1u16 << (15 - n.leading_zeros().min(15));
    let mut out = Vec::new();
    out.extend(bytes.get(0..4)?);
    out.extend(n.to_be_bytes());
    out.extend((pow * 16).to_be_bytes());
    out.extend((pow.trailing_zeros() as u16).to_be_bytes());
    out.extend((n * 16 - pow * 16).to_be_bytes());
    let mut at = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        let sum = data
            .chunks(4)
            .map(|c| {
                let mut w = [0u8; 4];
                w[..c.len()].copy_from_slice(c);
                u32::from_be_bytes(w)
            })
            .fold(0u32, u32::wrapping_add);
        out.extend(tag);
        out.extend(sum.to_be_bytes());
        out.extend((at as u32).to_be_bytes());
        out.extend((data.len() as u32).to_be_bytes());
        body.extend(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        at = 12 + 16 * tables.len() + body.len();
    }
    out.extend(body);
    Some(ColorFace { bytes: out, layers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_badge_is_blue_under_white() {
        let Some(face) = face() else {
            return; // no Studio content folder on this machine
        };
        let layers = &face.layers[&'\u{E000}'];
        assert_eq!(layers.len(), 2);
        // RobloxEmoji's CPAL: the badge 0,102,255, the check white.
        assert_eq!(layers[0].1, Some(Color32::from_rgb(0, 102, 255)));
        assert_eq!(layers[1].1, Some(Color32::WHITE));
        // The rebuilt face maps each layer and only the top one advances.
        let rebuilt = cmap(&face.bytes).unwrap();
        let glyph_of = |c: char| rebuilt.iter().find(|(k, _)| *k == c).map(|(_, g)| *g).unwrap();
        let (hmtx, _) = table(&face.bytes, b"hmtx").unwrap();
        let advance = |g: u16| R(&face.bytes).u16(hmtx + 4 * g as usize).unwrap();
        assert_eq!(advance(glyph_of(layers[0].0)), 0);
        assert_eq!(advance(glyph_of(layers[1].0)), advance(glyph_of('\u{E000}')));
        assert!(advance(glyph_of('\u{E000}')) > 0);
    }

    #[test]
    fn colour_glyphs_become_layer_sections() {
        let Some(_) = face() else { return };
        let mut job = LayoutJob::default();
        let format = TextFormat::simple(egui::FontId::proportional(14.0), Color32::from_rgb(10, 200, 30));
        append(&mut job, "Player1\u{E000}:", format);
        let texts: Vec<&str> =
            job.sections.iter().map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0]).collect();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(texts[0], "Player1");
        assert_eq!(texts[3], ":");
        assert_eq!(job.sections[1].format.color, Color32::from_rgb(0, 102, 255));
        assert_eq!(job.sections[3].format.color, Color32::from_rgb(10, 200, 30));
    }
}
