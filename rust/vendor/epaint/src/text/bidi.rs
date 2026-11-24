//! Lightweight Unicode Bidirectional Algorithm (UAX #9) for mixed LTR/RTL text.
//!
//! Fast-paths pure-LTR / ASCII text with zero allocations. When a paragraph
//! contains RTL characters (Arabic, Hebrew, etc.), resolves per-character
//! embedding levels (P2/P3, W1-W7, N1-N2, I1-I2) and reorders shaped runs
//! per visual line (L2) while preserving HarfBuzz's intra-run visual order and
//! GPOS glyph offsets.

use super::Glyph;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BidiClass {
    L,   // Left-to-Right
    R,   // Right-to-Left (Hebrew, etc.)
    AL,  // Right-to-Left Arabic
    EN,  // European Number
    AN,  // Arabic-Indic Number
    ES,  // European Separator (+, -)
    ET,  // European Terminator ($, %, etc.)
    CS,  // Common Separator (., ,, :)
    NSM, // Nonspacing Mark
    BN,  // Boundary Neutral
    WS,  // Whitespace
    ON,  // Other Neutral
}

#[inline]
pub(super) fn is_rtl_char(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0x0590..=0x08FF // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic, Arabic Extended
            | 0xFB1D..=0xFDFF // Hebrew & Arabic Presentation Forms-A
            | 0xFE70..=0xFEFF // Arabic Presentation Forms-B
            | 0x200F // RLM
            | 0x202B // RLE
            | 0x202E // RLO
            | 0x2067 // RLI
    )
}

#[inline]
pub(super) fn has_rtl(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    text.chars().any(is_rtl_char)
}

fn bidi_class(c: char) -> BidiClass {
    use unicode_general_category::{GeneralCategory, get_general_category};

    let u = c as u32;
    if u < 0x80 {
        return match c {
            'A'..='Z' | 'a'..='z' => BidiClass::L,
            '0'..='9' => BidiClass::EN,
            '+' | '-' => BidiClass::ES,
            '#' | '$' | '%' => BidiClass::ET,
            ',' | '.' | '/' | ':' => BidiClass::CS,
            ' ' | '\t' | '\x0C' => BidiClass::WS,
            _ => BidiClass::ON,
        };
    }

    match u {
        // Explicit marks
        0x200E => return BidiClass::L,
        0x200F => return BidiClass::R,
        0x200B..=0x200D | 0x2060..=0x2064 | 0xFEFF => return BidiClass::BN,
        // Hebrew
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => {
            return if matches!(
                get_general_category(c),
                GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
            ) {
                BidiClass::NSM
            } else {
                BidiClass::R
            };
        }
        // Arabic & Arabic Extended
        0x0600..=0x07BF | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFC => {
            if matches!(u, 0x0660..=0x0669 | 0x066B..=0x066C) {
                return BidiClass::AN;
            }
            if matches!(u, 0x06F0..=0x06F9) {
                return BidiClass::EN;
            }
            if matches!(
                get_general_category(c),
                GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
            ) {
                return BidiClass::NSM;
            }
            return BidiClass::AL;
        }
        // NKo, Samaritan, Mandaic, etc.
        0x07C0..=0x089F => {
            return if matches!(
                get_general_category(c),
                GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
            ) {
                BidiClass::NSM
            } else {
                BidiClass::R
            };
        }
        // Emoji / symbols are Other Neutral
        0x2190..=0x2BFF | 0x1F000..=0x1FAFF | 0xE000..=0xF8FF => return BidiClass::ON,
        _ => {}
    }

    match get_general_category(c) {
        GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark => BidiClass::NSM,
        GeneralCategory::SpaceSeparator => BidiClass::WS,
        GeneralCategory::Format => BidiClass::BN,
        GeneralCategory::UppercaseLetter
        | GeneralCategory::LowercaseLetter
        | GeneralCategory::TitlecaseLetter
        | GeneralCategory::ModifierLetter
        | GeneralCategory::OtherLetter
        | GeneralCategory::SpacingMark => BidiClass::L,
        GeneralCategory::DecimalNumber => BidiClass::EN,
        GeneralCategory::CurrencySymbol => BidiClass::ET,
        _ => BidiClass::ON,
    }
}

/// Resolve UAX #9 embedding levels for each `char` in `text`.
/// Returns `(base_is_rtl, levels)`.
pub(super) fn resolve_levels(text: &str) -> (bool, Vec<u8>) {
    let mut classes: Vec<BidiClass> = text.chars().map(bidi_class).collect();
    if classes.is_empty() {
        return (false, Vec::new());
    }

    // P2/P3: Paragraph embedding level from first strong character.
    let base_rtl = classes
        .iter()
        .find_map(|&c| match c {
            BidiClass::L => Some(false),
            BidiClass::R | BidiClass::AL => Some(true),
            _ => None,
        })
        .unwrap_or(false);
    let base_level: u8 = if base_rtl { 1 } else { 0 };
    let sos = if base_rtl { BidiClass::R } else { BidiClass::L };

    // W1: NSM takes the type of the previous character (or sos).
    let mut prev = sos;
    for cls in &mut classes {
        if *cls == BidiClass::NSM {
            *cls = prev;
        } else if *cls != BidiClass::BN {
            prev = *cls;
        }
    }

    // W2: EN -> AN when last strong type was AL.
    let mut last_strong = sos;
    for cls in &mut classes {
        match *cls {
            BidiClass::L | BidiClass::R | BidiClass::AL => last_strong = *cls,
            BidiClass::EN if last_strong == BidiClass::AL => *cls = BidiClass::AN,
            _ => {}
        }
    }

    // W3: AL -> R.
    for cls in &mut classes {
        if *cls == BidiClass::AL {
            *cls = BidiClass::R;
        }
    }

    // W4: Single ES between ENs -> EN; single CS between two ENs or two ANs.
    let n = classes.len();
    for i in 1..n.saturating_sub(1) {
        if classes[i] == BidiClass::ES
            && classes[i - 1] == BidiClass::EN
            && classes[i + 1] == BidiClass::EN
        {
            classes[i] = BidiClass::EN;
        } else if classes[i] == BidiClass::CS {
            if classes[i - 1] == BidiClass::EN && classes[i + 1] == BidiClass::EN {
                classes[i] = BidiClass::EN;
            } else if classes[i - 1] == BidiClass::AN && classes[i + 1] == BidiClass::AN {
                classes[i] = BidiClass::AN;
            }
        }
    }

    // W5: ET sequences adjacent to EN -> EN.
    let mut i = 0;
    while i < n {
        if classes[i] == BidiClass::ET {
            let start = i;
            while i < n && classes[i] == BidiClass::ET {
                i += 1;
            }
            let left_en = start > 0 && classes[start - 1] == BidiClass::EN;
            let right_en = i < n && classes[i] == BidiClass::EN;
            if left_en || right_en {
                for c in &mut classes[start..i] {
                    *c = BidiClass::EN;
                }
            }
        } else {
            i += 1;
        }
    }

    // W6: Remaining separators/terminators -> ON.
    for cls in &mut classes {
        if matches!(*cls, BidiClass::ES | BidiClass::ET | BidiClass::CS) {
            *cls = BidiClass::ON;
        }
    }

    // W7: EN -> L when last strong type was L.
    last_strong = sos;
    for cls in &mut classes {
        match *cls {
            BidiClass::L | BidiClass::R => last_strong = *cls,
            BidiClass::EN if last_strong == BidiClass::L => *cls = BidiClass::L,
            _ => {}
        }
    }

    // N1 / N2: Resolve neutral sequences (WS, ON, BN).
    let strong_dir = |c: BidiClass| -> Option<BidiClass> {
        match c {
            BidiClass::L => Some(BidiClass::L),
            BidiClass::R | BidiClass::EN | BidiClass::AN => Some(BidiClass::R),
            _ => None,
        }
    };
    let mut i = 0;
    while i < n {
        if strong_dir(classes[i]).is_none() {
            let start = i;
            while i < n && strong_dir(classes[i]).is_none() {
                i += 1;
            }
            let left = if start > 0 {
                strong_dir(classes[start - 1]).unwrap_or(sos)
            } else {
                sos
            };
            let right = if i < n {
                strong_dir(classes[i]).unwrap_or(sos)
            } else {
                sos
            };
            let resolved = if left == right { left } else { sos };
            for c in &mut classes[start..i] {
                *c = resolved;
            }
        } else {
            i += 1;
        }
    }

    // I1 / I2: Compute final embedding levels.
    let levels: Vec<u8> = classes
        .into_iter()
        .map(|cls| {
            if base_level == 0 {
                match cls {
                    BidiClass::R => 1,
                    BidiClass::AN | BidiClass::EN => 2,
                    _ => 0,
                }
            } else {
                match cls {
                    BidiClass::L | BidiClass::EN | BidiClass::AN => 2,
                    _ => 1,
                }
            }
        })
        .collect();

    (base_rtl, levels)
}

/// Reorder shaped runs on a single wrapped row into visual left-to-right order
/// (UAX #9 Rule L2), preserving HarfBuzz's intra-run visual glyph order and
/// GPOS x-offsets.
pub(super) fn reorder_row_glyphs(glyphs: &mut [Glyph]) {
    let mut max_level = 0u8;
    let mut min_odd = u8::MAX;
    for g in glyphs.iter() {
        if g.bidi_level > max_level {
            max_level = g.bidi_level;
        }
        if g.bidi_level % 2 == 1 && g.bidi_level < min_odd {
            min_odd = g.bidi_level;
        }
    }
    if max_level == 0 {
        return;
    }

    // Group contiguous glyphs belonging to the same shaped run.
    // Within each shaped run, HarfBuzz already emitted glyphs in visual L->R order.
    struct RunSpan {
        start: usize,
        end: usize,
        level: u8,
        origin_x: f32,
        advance_x: f32,
    }

    let mut runs: Vec<RunSpan> = Vec::new();
    let mut i = 0;
    while i < glyphs.len() {
        let start = i;
        let run_id = glyphs[i].bidi_run;
        let level = glyphs[i].bidi_level;
        let origin_x = glyphs[i].pos.x;
        let mut advance_x = 0.0f32;
        while i < glyphs.len() && glyphs[i].bidi_run == run_id && glyphs[i].bidi_level == level {
            advance_x += glyphs[i].advance_width;
            i += 1;
        }
        runs.push(RunSpan {
            start,
            end: i,
            level,
            origin_x,
            advance_x,
        });
    }

    // UAX #9 Rule L2: reverse contiguous run spans for each level from max_level down to lowest odd level.
    let lowest = if min_odd == u8::MAX { 1 } else { min_odd };
    let mut level = max_level;
    while level >= lowest {
        let mut idx = 0;
        while idx < runs.len() {
            if runs[idx].level >= level {
                let span_start = idx;
                while idx < runs.len() && runs[idx].level >= level {
                    idx += 1;
                }
                runs[span_start..idx].reverse();
            } else {
                idx += 1;
            }
        }
        if level == 0 {
            break;
        }
        level -= 1;
    }

    // Rebuild glyphs in visual order and shift each run's x positions cleanly.
    let mut reordered: Vec<Glyph> = Vec::with_capacity(glyphs.len());
    let mut cursor_x = 0.0f32;
    for run in &runs {
        let dx = cursor_x - run.origin_x;
        for g in &glyphs[run.start..run.end] {
            let mut moved = *g;
            moved.pos.x += dx;
            reordered.push(moved);
        }
        cursor_x += run.advance_x;
    }
    glyphs.copy_from_slice(&reordered);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bidi_levels_pure_arabic() {
        let (rtl, levels) = resolve_levels("مرحبا بالعالم");
        assert!(rtl);
        assert!(levels.iter().all(|&l| l == 1), "levels={levels:?}");
    }

    #[test]
    fn test_bidi_levels_mixed_ltr_base() {
        let (rtl, levels) = resolve_levels("Hello مرحبا World");
        assert!(!rtl);
        // "Hello " -> 0, "مرحبا" -> 1, " World" -> 0
        assert_eq!(&levels[0..6], &[0, 0, 0, 0, 0, 0]);
        assert_eq!(&levels[6..11], &[1, 1, 1, 1, 1]);
        assert_eq!(&levels[11..17], &[0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn test_bidi_levels_hebrew_base_with_latin() {
        let (rtl, levels) = resolve_levels("שלום עולם Hello 42");
        assert!(rtl);
        // "שלום עולם " (10 chars) -> 1, "Hello 42" (8 chars) -> 2
        assert_eq!(&levels[0..10], &[1; 10]);
        assert_eq!(&levels[10..18], &[2; 8]);
    }
}
