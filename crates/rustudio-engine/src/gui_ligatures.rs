//! Font ligatures for icon fonts drawn by name. Roblox's text engine applies
//! a font's GSUB ligatures, and the CoreScripts' Builder Icons font
//! (`rbxasset://LuaPackages/Packages/_Index/BuilderIcons/…`, its package
//! README: `Text = Icon.House` with the icon font as `FontFace`) turns a name
//! such as "speech-bubble-align-center" into one glyph that way. egui lays
//! text out a character at a time, so a face's ligatures whose glyph also has
//! a codepoint (Builder Icons maps each to the private use area) are read
//! once from its GSUB table, and the names are replaced by those codepoints
//! before layout.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::FontFamily;

type Map = Arc<HashMap<String, char>>;

static FACES: Mutex<Option<HashMap<String, Map>>> = Mutex::new(None);

/// Reads a face's ligatures when it is added as egui family `name`.
pub(super) fn note_face(name: &str, bytes: &[u8]) {
    let map = ligatures(bytes);
    if map.is_empty() {
        return;
    }
    if let Ok(mut g) = FACES.lock() {
        g.get_or_insert_with(HashMap::new).insert(name.to_string(), Arc::new(map));
    }
}

/// `text` with every ligature name of `family`'s face replaced by its glyph
/// (longest name first at each position), or `text` itself.
pub(super) fn apply(family: &FontFamily, text: String) -> String {
    let FontFamily::Name(name) = family else { return text };
    let map = match FACES.lock().ok().and_then(|g| g.as_ref()?.get(&**name).cloned()) {
        Some(m) => m,
        None => return text,
    };
    substitute(&map, &text).unwrap_or(text)
}

fn substitute(map: &HashMap<String, char>, text: &str) -> Option<String> {
    let longest = map.keys().map(|k| k.len()).max().unwrap_or(0);
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut i = 0;
    while i < text.len() {
        let mut hit = None;
        let mut end = (i + longest).min(text.len());
        while end > i {
            if text.is_char_boundary(end) {
                if let Some(c) = map.get(&text[i..end]) {
                    hit = Some((*c, end));
                    break;
                }
            }
            end -= 1;
        }
        match hit {
            Some((c, e)) if e - i > 1 => {
                out.push(c);
                i = e;
                changed = true;
            }
            _ => {
                let c = text[i..].chars().next()?;
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    changed.then_some(out)
}

pub(super) struct R<'a>(pub(super) &'a [u8]);

impl R<'_> {
    pub(super) fn u16(&self, at: usize) -> Option<u16> {
        Some(u16::from_be_bytes(self.0.get(at..at + 2)?.try_into().ok()?))
    }
    pub(super) fn u32(&self, at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(self.0.get(at..at + 4)?.try_into().ok()?))
    }
}

/// The face's GSUB ligature substitutions (lookup type 4, also through
/// extension lookups) as text → the ligature glyph's codepoint, for those
/// whose components and result all have codepoints in its cmap.
pub(super) fn ligatures(bytes: &[u8]) -> HashMap<String, char> {
    parse(bytes).unwrap_or_default()
}

/// A table's (offset, length) in an sfnt.
pub(super) fn table(bytes: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let f = R(bytes);
    for i in 0..f.u16(4)? as usize {
        let rec = 12 + 16 * i;
        if bytes.get(rec..rec + 4)? == tag {
            return Some((f.u32(rec + 8)? as usize, f.u32(rec + 12)? as usize));
        }
    }
    None
}

/// Every codepoint → glyph mapping of a face's cmap (format 4 and 12
/// subtables, each mapping as often as the subtables repeat it), glyph 0
/// left out.
pub(super) fn cmap(bytes: &[u8]) -> Option<Vec<(char, u16)>> {
    let f = R(bytes);
    let (cmap_at, _) = table(bytes, b"cmap")?;
    let mut out = Vec::new();
    let mut put = |c: u32, g: u32| {
        if let (Some(ch), Ok(g)) = (char::from_u32(c), u16::try_from(g)) {
            if g != 0 {
                out.push((ch, g));
            }
        }
    };
    for i in 0..f.u16(cmap_at + 2)? as usize {
        let sub = cmap_at + f.u32(cmap_at + 4 + 8 * i + 4)? as usize;
        match f.u16(sub)? {
            4 => {
                let seg = f.u16(sub + 6)? as usize / 2;
                let (ends, starts, deltas, ranges) = (sub + 14, sub + 16 + 2 * seg, sub + 16 + 4 * seg, sub + 16 + 6 * seg);
                for s in 0..seg {
                    let (start, end) = (f.u16(starts + 2 * s)? as u32, f.u16(ends + 2 * s)? as u32);
                    let delta = f.u16(deltas + 2 * s)? as u32;
                    let ro = f.u16(ranges + 2 * s)? as usize;
                    for c in start..=end.min(0xFFFE) {
                        let g = if ro == 0 {
                            (c + delta) & 0xFFFF
                        } else {
                            let at = ranges + 2 * s + ro + 2 * (c - start) as usize;
                            match f.u16(at)? as u32 {
                                0 => 0,
                                g => (g + delta) & 0xFFFF,
                            }
                        };
                        put(c, g);
                    }
                }
            }
            12 => {
                for gi in 0..f.u32(sub + 12)? as usize {
                    let at = sub + 16 + 12 * gi;
                    let (a, b, g) = (f.u32(at)?, f.u32(at + 4)?, f.u32(at + 8)?);
                    for c in a..=b.min(a.saturating_add(0xFFFF)) {
                        put(c, g + (c - a));
                    }
                }
            }
            _ => {}
        }
    }
    Some(out)
}

fn parse(bytes: &[u8]) -> Option<HashMap<String, char>> {
    let f = R(bytes);
    let (gsub_at, _) = table(bytes, b"GSUB")?;
    // glyph → its (first) codepoint
    let mut chars: HashMap<u16, char> = HashMap::new();
    for (c, g) in cmap(bytes)? {
        chars.entry(g).or_insert(c);
    }
    let coverage = |at: usize| -> Option<Vec<u16>> {
        let n = f.u16(at + 2)? as usize;
        Some(match f.u16(at)? {
            1 => (0..n).map(|i| f.u16(at + 4 + 2 * i)).collect::<Option<_>>()?,
            _ => {
                let mut v = Vec::new();
                for i in 0..n {
                    let (s, e) = (f.u16(at + 4 + 6 * i)?, f.u16(at + 6 + 6 * i)?);
                    v.extend(s..=e);
                }
                v
            }
        })
    };
    let mut out = HashMap::new();
    let lookups = gsub_at + f.u16(gsub_at + 8)? as usize;
    for i in 0..f.u16(lookups)? as usize {
        let lookup = lookups + f.u16(lookups + 2 + 2 * i)? as usize;
        let kind = f.u16(lookup)?;
        for j in 0..f.u16(lookup + 4)? as usize {
            let mut sub = lookup + f.u16(lookup + 6 + 2 * j)? as usize;
            let mut t = kind;
            if t == 7 {
                t = f.u16(sub + 2)?;
                sub += f.u32(sub + 4)? as usize;
            }
            if t != 4 {
                continue;
            }
            let firsts = coverage(sub + f.u16(sub + 2)? as usize)?;
            for (k, first) in firsts.iter().enumerate().take(f.u16(sub + 4)? as usize) {
                let set = sub + f.u16(sub + 6 + 2 * k)? as usize;
                for q in 0..f.u16(set)? as usize {
                    let lig = set + f.u16(set + 2 + 2 * q)? as usize;
                    let glyph = f.u16(lig)?;
                    let n = f.u16(lig + 2)? as usize;
                    let mut name = String::new();
                    let mut ok = chars.get(first).map(|c| name.push(*c)).is_some();
                    for c in 1..n {
                        match chars.get(&f.u16(lig + 4 + 2 * (c - 1))?) {
                            Some(ch) if ok => name.push(*ch),
                            _ => ok = false,
                        }
                    }
                    if let (true, Some(ch)) = (ok && n > 1, chars.get(&glyph)) {
                        out.entry(name).or_insert(*ch);
                    }
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_their_glyph_longest_first() {
        let map = HashMap::from([("house".to_string(), '\u{f174}'), ("house-door".to_string(), '\u{f175}')]);
        assert_eq!(substitute(&map, "house").as_deref(), Some("\u{f174}"));
        assert_eq!(substitute(&map, "a house-door!").as_deref(), Some("a \u{f175}!"));
        assert_eq!(substitute(&map, "no icon here"), None);
    }

    #[test]
    fn builder_icons_ligatures_from_the_studio_install() {
        let Some(path) = rustudio_core::assets::resolve(
            "rbxasset://LuaPackages/Packages/_Index/BuilderIcons/BuilderIcons/Font/BuilderIcons-Regular.ttf",
            None,
        )
        .filter(|p| p.is_file()) else {
            return; // no Studio install on this machine
        };
        let map = ligatures(&std::fs::read(path).unwrap());
        assert!(map.len() > 500, "{}", map.len());
        // Every icon maps into the private use area.
        assert!(map.values().all(|c| ('\u{e000}'..='\u{f8ff}').contains(c)));
        assert!(map.contains_key("speech-bubble-align-center"));
        assert!(map.contains_key("three-dots-horizontal"));
    }
}
