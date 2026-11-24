//! Pure TextBox editing state machine matching Roblox Studio's measured behaviour:
//! 1-based UTF-8 byte-offset `CursorPosition` (-1 when unfocused), `SelectionStart`,
//! word boundaries, MultiLine navigation, clipboard ops, and exact property-change
//! signal ordering.

/// A single edit/navigation action applied to a focused `TextBox`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditAction {
    InsertText(String),
    Paste(String),
    Backspace { word: bool },
    Delete { word: bool },
    Left { shift: bool, word: bool },
    Right { shift: bool, word: bool },
    Home { shift: bool },
    End { shift: bool },
    Up { shift: bool },
    Down { shift: bool },
    SelectAll,
    Copy,
    Cut,
    Enter,
    Escape,
    /// Click placed the caret at 1-based UTF-8 byte position `pos`.
    ClickAt { pos: usize, shift: bool },
    /// Double-click at 1-based UTF-8 byte position `pos` selects the word there.
    DoubleClickAt { pos: usize },
    /// Drag extended selection to 1-based UTF-8 byte position `pos`.
    DragTo { pos: usize },
}

/// Snapshot of a TextBox's editing state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBoxState {
    pub text: String,
    /// 1-based UTF-8 byte position (`1..=text.len()+1`), or `-1` when unfocused.
    pub cursor: i32,
    /// 1-based UTF-8 byte position of selection anchor, or `-1` when no selection.
    pub selection_start: i32,
    pub focused: bool,
    pub editable: bool,
    pub multi_line: bool,
    pub clear_text_on_focus: bool,
}

/// Result of applying an `EditAction`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EditOutcome {
    /// Signals to fire, in Roblox's exact order (`CursorPosition`, `SelectionStart`, `Text`).
    pub changed_cursor: bool,
    pub changed_selection: bool,
    pub changed_text: bool,
    /// Text to copy to the system clipboard (on Ctrl+C / Ctrl+X).
    pub copied: Option<String>,
    /// Focus lost: `Some(true)` on Enter (single-line), `Some(false)` on Escape.
    pub lost_focus: Option<bool>,
}

impl TextBoxState {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cursor: 1,
            selection_start: -1,
            focused: false,
            editable: true,
            multi_line: false,
            clear_text_on_focus: true,
        }
    }

    /// 0-based byte range `(lo, hi)` if a non-empty selection is active.
    pub fn selection_byte_range(&self) -> Option<(usize, usize)> {
        if !self.focused || self.selection_start < 1 || self.cursor < 1 {
            return None;
        }
        let a = clamp_byte_pos(&self.text, (self.selection_start - 1) as usize);
        let b = clamp_byte_pos(&self.text, (self.cursor - 1) as usize);
        if a == b {
            None
        } else if a < b {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    /// Current 0-based byte index of the caret (clamped to a char boundary).
    pub fn cursor_byte(&self) -> usize {
        if self.cursor < 1 {
            0
        } else {
            clamp_byte_pos(&self.text, (self.cursor - 1) as usize)
        }
    }

    /// Gains focus (via `CaptureFocus` or mouse click). When `click_pos` is `Some(p)`,
    /// places the caret at `p` (unless `ClearTextOnFocus` cleared the text); otherwise at end.
    pub fn focus(&mut self, click_pos: Option<usize>) -> EditOutcome {
        let mut out = EditOutcome::default();
        let old_text = self.text.clone();
        let old_cursor = self.cursor;
        let old_sel = self.selection_start;

        if self.clear_text_on_focus && self.editable && !self.text.is_empty() {
            self.text.clear();
        }
        self.focused = true;
        let target_0 = match click_pos {
            Some(p) if self.text == old_text => clamp_byte_pos(&self.text, p.saturating_sub(1)),
            _ => self.text.len(),
        };
        self.cursor = (target_0 + 1) as i32;
        self.selection_start = -1;

        out.changed_text = self.text != old_text;
        out.changed_cursor = self.cursor != old_cursor;
        out.changed_selection = self.selection_start != old_sel;
        out
    }

    /// Loses focus: sets `CursorPosition = -1`, `SelectionStart = -1`.
    pub fn blur(&mut self, enter_pressed: bool) -> EditOutcome {
        let mut out = EditOutcome::default();
        let old_cursor = self.cursor;
        let old_sel = self.selection_start;
        self.focused = false;
        self.cursor = -1;
        self.selection_start = -1;
        out.changed_cursor = self.cursor != old_cursor;
        out.changed_selection = self.selection_start != old_sel;
        out.lost_focus = Some(enter_pressed);
        out
    }

    /// Applies one editing or navigation action.
    pub fn apply(&mut self, action: EditAction) -> EditOutcome {
        if !self.focused {
            return EditOutcome::default();
        }
        let old_text = self.text.clone();
        let old_cursor = self.cursor;
        let old_sel = self.selection_start;
        let mut out = EditOutcome::default();

        // Ensure cursor is valid within current text before acting.
        let cur = self.cursor_byte();

        match action {
            EditAction::InsertText(s) => {
                if self.editable && !s.is_empty() {
                    self.insert_str(&s);
                }
            }
            EditAction::Paste(raw) => {
                if self.editable && !raw.is_empty() {
                    let normalized = normalize_paste(&raw);
                    if !normalized.is_empty() {
                        self.insert_str(&normalized);
                    }
                }
            }
            EditAction::Enter => {
                if self.multi_line {
                    if self.editable {
                        self.insert_str("\n");
                    }
                } else {
                    return self.blur(true);
                }
            }
            EditAction::Escape => {
                return self.blur(false);
            }
            EditAction::Backspace { word } => {
                if self.editable {
                    if let Some((lo, hi)) = self.selection_byte_range() {
                        self.text.replace_range(lo..hi, "");
                        self.cursor = (lo + 1) as i32;
                        self.selection_start = -1;
                    } else if cur > 0 {
                        let target = if word {
                            word_left(&self.text, cur)
                        } else {
                            prev_char_boundary(&self.text, cur)
                        };
                        self.text.replace_range(target..cur, "");
                        self.cursor = (target + 1) as i32;
                        self.selection_start = -1;
                    }
                }
            }
            EditAction::Delete { word } => {
                if self.editable {
                    if let Some((lo, hi)) = self.selection_byte_range() {
                        self.text.replace_range(lo..hi, "");
                        self.cursor = (lo + 1) as i32;
                        self.selection_start = -1;
                    } else if cur < self.text.len() {
                        let target = if word {
                            word_right_delete(&self.text, cur)
                        } else {
                            next_char_boundary(&self.text, cur)
                        };
                        self.text.replace_range(cur..target, "");
                        self.cursor = (cur + 1) as i32;
                        self.selection_start = -1;
                    }
                }
            }
            EditAction::Left { shift, word } => {
                if shift {
                    let target = if word {
                        word_left(&self.text, cur)
                    } else {
                        prev_char_boundary(&self.text, cur)
                    };
                    self.move_with_shift(cur, target);
                } else if let Some((lo, _)) = self.selection_byte_range() {
                    self.cursor = (lo + 1) as i32;
                    self.selection_start = -1;
                } else {
                    let target = if word {
                        word_left(&self.text, cur)
                    } else {
                        prev_char_boundary(&self.text, cur)
                    };
                    self.cursor = (target + 1) as i32;
                    self.selection_start = -1;
                }
            }
            EditAction::Right { shift, word } => {
                if shift {
                    let target = if word {
                        word_right_move(&self.text, cur)
                    } else {
                        next_char_boundary(&self.text, cur)
                    };
                    self.move_with_shift(cur, target);
                } else if let Some((_, hi)) = self.selection_byte_range() {
                    self.cursor = (hi + 1) as i32;
                    self.selection_start = -1;
                } else {
                    let target = if word {
                        word_right_move(&self.text, cur)
                    } else {
                        next_char_boundary(&self.text, cur)
                    };
                    self.cursor = (target + 1) as i32;
                    self.selection_start = -1;
                }
            }
            EditAction::Home { shift } => {
                let target = if self.multi_line {
                    line_start(&self.text, cur)
                } else {
                    0
                };
                if shift {
                    self.move_with_shift(cur, target);
                } else {
                    self.cursor = (target + 1) as i32;
                    self.selection_start = -1;
                }
            }
            EditAction::End { shift } => {
                let target = if self.multi_line {
                    line_end(&self.text, cur)
                } else {
                    self.text.len()
                };
                if shift {
                    self.move_with_shift(cur, target);
                } else {
                    self.cursor = (target + 1) as i32;
                    self.selection_start = -1;
                }
            }
            EditAction::Up { shift } => {
                if self.multi_line {
                    if let Some(target) = move_line_up(&self.text, cur) {
                        if shift {
                            self.move_with_shift(cur, target);
                        } else {
                            self.cursor = (target + 1) as i32;
                            self.selection_start = -1;
                        }
                    } else if !shift && self.selection_start != -1 {
                        self.selection_start = -1;
                    }
                }
            }
            EditAction::Down { shift } => {
                if self.multi_line {
                    if let Some(target) = move_line_down(&self.text, cur) {
                        if shift {
                            self.move_with_shift(cur, target);
                        } else {
                            self.cursor = (target + 1) as i32;
                            self.selection_start = -1;
                        }
                    } else if !shift && self.selection_start != -1 {
                        self.selection_start = -1;
                    }
                }
            }
            EditAction::SelectAll => {
                if self.text.is_empty() {
                    self.cursor = 1;
                    self.selection_start = -1;
                } else {
                    self.cursor = (self.text.len() + 1) as i32;
                    self.selection_start = 1;
                }
            }
            EditAction::Copy => {
                if let Some((lo, hi)) = self.selection_byte_range() {
                    out.copied = Some(self.text[lo..hi].to_string());
                }
            }
            EditAction::Cut => {
                if let Some((lo, hi)) = self.selection_byte_range() {
                    out.copied = Some(self.text[lo..hi].to_string());
                    if self.editable {
                        self.text.replace_range(lo..hi, "");
                        self.cursor = (lo + 1) as i32;
                        self.selection_start = -1;
                    }
                }
            }
            EditAction::ClickAt { pos, shift } => {
                let target = clamp_byte_pos(&self.text, pos.saturating_sub(1));
                if shift {
                    self.move_with_shift(cur, target);
                } else {
                    self.cursor = (target + 1) as i32;
                    self.selection_start = -1;
                }
            }
            EditAction::DoubleClickAt { pos } => {
                let at = clamp_byte_pos(&self.text, pos.saturating_sub(1));
                let (lo, hi) = word_bounds_at(&self.text, at);
                if lo < hi {
                    self.selection_start = (lo + 1) as i32;
                    self.cursor = (hi + 1) as i32;
                } else {
                    self.cursor = (lo + 1) as i32;
                    self.selection_start = -1;
                }
            }
            EditAction::DragTo { pos } => {
                let target = clamp_byte_pos(&self.text, pos.saturating_sub(1));
                self.move_with_shift(cur, target);
            }
        }

        out.changed_cursor = self.cursor != old_cursor;
        out.changed_selection = self.selection_start != old_sel;
        out.changed_text = self.text != old_text;
        out
    }

    fn insert_str(&mut self, s: &str) {
        if let Some((lo, hi)) = self.selection_byte_range() {
            self.text.replace_range(lo..hi, s);
            self.cursor = (lo + s.len() + 1) as i32;
            self.selection_start = -1;
        } else {
            let cur = self.cursor_byte();
            self.text.insert_str(cur, s);
            self.cursor = (cur + s.len() + 1) as i32;
            self.selection_start = -1;
        }
    }

    fn move_with_shift(&mut self, cur: usize, target: usize) {
        if self.selection_start < 1 {
            if target != cur {
                self.selection_start = (cur + 1) as i32;
            }
        }
        self.cursor = (target + 1) as i32;
        if self.selection_start == self.cursor {
            self.selection_start = -1;
        }
    }
}

/// Normalize CRLF / CR to LF on paste (Roblox keeps `\n` even in single-line boxes).
pub fn normalize_paste(raw: &str) -> String {
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

/// Clamp a 0-based byte index to `0..=s.len()` on a valid UTF-8 char boundary.
pub fn clamp_byte_pos(s: &str, pos: usize) -> usize {
    let mut p = pos.min(s.len());
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

pub fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = clamp_byte_pos(s, pos);
    if p == 0 {
        return 0;
    }
    p -= 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

pub fn next_char_boundary(s: &str, pos: usize) -> usize {
    let p = clamp_byte_pos(s, pos);
    if p >= s.len() {
        return s.len();
    }
    let ch = s[p..].chars().next().unwrap();
    p + ch.len_utf8()
}

/// Roblox word-left (Ctrl+Left / Ctrl+Backspace): skip whitespace going left,
/// then skip non-whitespace going left.
pub fn word_left(s: &str, pos: usize) -> usize {
    let mut p = clamp_byte_pos(s, pos);
    while p > 0 {
        let prev = prev_char_boundary(s, p);
        let ch = s[prev..p].chars().next().unwrap();
        if ch.is_whitespace() {
            p = prev;
        } else {
            break;
        }
    }
    while p > 0 {
        let prev = prev_char_boundary(s, p);
        let ch = s[prev..p].chars().next().unwrap();
        if !ch.is_whitespace() {
            p = prev;
        } else {
            break;
        }
    }
    p
}

/// Roblox Ctrl+Right: skip non-whitespace going right, then skip whitespace going right.
pub fn word_right_move(s: &str, pos: usize) -> usize {
    let mut p = clamp_byte_pos(s, pos);
    while p < s.len() {
        let ch = s[p..].chars().next().unwrap();
        if !ch.is_whitespace() {
            p += ch.len_utf8();
        } else {
            break;
        }
    }
    while p < s.len() {
        let ch = s[p..].chars().next().unwrap();
        if ch.is_whitespace() {
            p += ch.len_utf8();
        } else {
            break;
        }
    }
    p
}

/// Roblox Ctrl+Delete: skip whitespace going right, then skip non-whitespace going right.
pub fn word_right_delete(s: &str, pos: usize) -> usize {
    let mut p = clamp_byte_pos(s, pos);
    while p < s.len() {
        let ch = s[p..].chars().next().unwrap();
        if ch.is_whitespace() {
            p += ch.len_utf8();
        } else {
            break;
        }
    }
    while p < s.len() {
        let ch = s[p..].chars().next().unwrap();
        if !ch.is_whitespace() {
            p += ch.len_utf8();
        } else {
            break;
        }
    }
    p
}

/// Word bounds around byte position `pos` for double-click selection.
pub fn word_bounds_at(s: &str, pos: usize) -> (usize, usize) {
    if s.is_empty() {
        return (0, 0);
    }
    let mut p = clamp_byte_pos(s, pos);
    if p == s.len() {
        p = prev_char_boundary(s, p);
    }
    let ch = s[p..].chars().next().unwrap();
    let is_ws = ch.is_whitespace();
    let mut lo = p;
    while lo > 0 {
        let prev = prev_char_boundary(s, lo);
        let c = s[prev..lo].chars().next().unwrap();
        if c.is_whitespace() == is_ws {
            lo = prev;
        } else {
            break;
        }
    }
    let mut hi = p + ch.len_utf8();
    while hi < s.len() {
        let c = s[hi..].chars().next().unwrap();
        if c.is_whitespace() == is_ws {
            hi += c.len_utf8();
        } else {
            break;
        }
    }
    (lo, hi)
}

pub fn line_start(s: &str, pos: usize) -> usize {
    let p = clamp_byte_pos(s, pos);
    match s[..p].rfind('\n') {
        Some(nl) => nl + 1,
        None => 0,
    }
}

pub fn line_end(s: &str, pos: usize) -> usize {
    let p = clamp_byte_pos(s, pos);
    match s[p..].find('\n') {
        Some(rel) => p + rel,
        None => s.len(),
    }
}

pub fn move_line_up(s: &str, pos: usize) -> Option<usize> {
    let p = clamp_byte_pos(s, pos);
    let ls = line_start(s, p);
    if ls == 0 {
        return None;
    }
    let col_chars = s[ls..p].chars().count();
    let prev_end = ls - 1; // the '\n' ending the previous line
    let prev_start = line_start(s, prev_end);
    Some(advance_chars(s, prev_start, prev_end, col_chars))
}

pub fn move_line_down(s: &str, pos: usize) -> Option<usize> {
    let p = clamp_byte_pos(s, pos);
    let ls = line_start(s, p);
    let le = line_end(s, p);
    if le >= s.len() {
        return None;
    }
    let col_chars = s[ls..p].chars().count();
    let next_start = le + 1;
    let next_end = line_end(s, next_start);
    Some(advance_chars(s, next_start, next_end, col_chars))
}

fn advance_chars(s: &str, start: usize, end: usize, count: usize) -> usize {
    let mut p = start;
    for _ in 0..count {
        if p >= end {
            break;
        }
        let ch = s[p..end].chars().next().unwrap();
        p += ch.len_utf8();
    }
    p
}

/// Truncate text to at most `max_graphemes` visible units (when `>= 0`).
pub fn limit_visible_graphemes(s: &str, max: i32) -> &str {
    if max < 0 {
        return s;
    }
    if max == 0 {
        return "";
    }
    let mut count = 0i32;
    for (idx, _) in s.char_indices() {
        if count == max {
            return &s[..idx];
        }
        count += 1;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_clear_and_readonly() {
        let mut tb = TextBoxState::new("initial");
        let o = tb.focus(None);
        assert_eq!(tb.text, "");
        assert_eq!(tb.cursor, 1);
        assert_eq!(tb.selection_start, -1);
        assert!(o.changed_text);

        // ReadOnly ignores ClearTextOnFocus and all edits, allows navigation & Copy.
        let mut ro = TextBoxState::new("readonly text");
        ro.editable = false;
        ro.clear_text_on_focus = true;
        let o = ro.focus(None);
        assert_eq!(ro.text, "readonly text");
        assert_eq!(ro.cursor, 14);
        assert!(!o.changed_text);

        ro.apply(EditAction::InsertText("xyz".into()));
        ro.apply(EditAction::Backspace { word: false });
        assert_eq!(ro.text, "readonly text");

        ro.apply(EditAction::Left { shift: false, word: false });
        assert_eq!(ro.cursor, 13);
        ro.apply(EditAction::Left { shift: true, word: false });
        assert_eq!((ro.cursor, ro.selection_start), (12, 13));
        ro.apply(EditAction::SelectAll);
        assert_eq!((ro.cursor, ro.selection_start), (14, 1));
        let cp = ro.apply(EditAction::Copy);
        assert_eq!(cp.copied.as_deref(), Some("readonly text"));
        // Cut copies but does not mutate readonly text.
        let ct = ro.apply(EditAction::Cut);
        assert_eq!(ct.copied.as_deref(), Some("readonly text"));
        assert_eq!(ro.text, "readonly text");
    }

    #[test]
    fn word_jumps_and_deletes_match_studio() {
        let mut tb = TextBoxState::new("hello world foo_bar");
        tb.clear_text_on_focus = false;
        tb.focus(None);
        assert_eq!(tb.cursor, 20);

        for expected in [13, 7, 1, 1] {
            tb.apply(EditAction::Left { shift: false, word: true });
            assert_eq!(tb.cursor, expected);
        }
        for expected in [7, 13, 20, 20] {
            tb.apply(EditAction::Right { shift: false, word: true });
            assert_eq!(tb.cursor, expected);
        }

        // Ctrl+Backspace on 'foo   bar_baz!  end'
        tb.text = "foo   bar_baz!  end".into();
        tb.cursor = 20;
        tb.apply(EditAction::Backspace { word: true });
        assert_eq!((tb.text.as_str(), tb.cursor), ("foo   bar_baz!  ", 17));
        tb.apply(EditAction::Backspace { word: true });
        assert_eq!((tb.text.as_str(), tb.cursor), ("foo   ", 7));
        tb.apply(EditAction::Backspace { word: true });
        assert_eq!((tb.text.as_str(), tb.cursor), ("", 1));

        // Ctrl+Delete on 'foo   bar_baz!  end'
        tb.text = "foo   bar_baz!  end".into();
        tb.cursor = 1;
        tb.apply(EditAction::Delete { word: true });
        assert_eq!((tb.text.as_str(), tb.cursor), ("   bar_baz!  end", 1));
        tb.apply(EditAction::Delete { word: true });
        assert_eq!((tb.text.as_str(), tb.cursor), ("  end", 1));
        tb.apply(EditAction::Delete { word: true });
        assert_eq!((tb.text.as_str(), tb.cursor), ("", 1));
    }

    #[test]
    fn selection_collapse_replace_and_clipboard() {
        let mut tb = TextBoxState::new("hello world foo_bar");
        tb.clear_text_on_focus = false;
        tb.focus(None);
        tb.cursor = 6;
        tb.apply(EditAction::Left { shift: true, word: false });
        tb.apply(EditAction::Left { shift: true, word: false });
        assert_eq!((tb.cursor, tb.selection_start), (4, 6));
        // Left collapses to lo
        tb.apply(EditAction::Left { shift: false, word: false });
        assert_eq!((tb.cursor, tb.selection_start), (4, -1));
        // Shift+Right x3 then Right collapses to hi
        for _ in 0..3 {
            tb.apply(EditAction::Right { shift: true, word: false });
        }
        assert_eq!((tb.cursor, tb.selection_start), (7, 4));
        tb.apply(EditAction::Right { shift: false, word: false });
        assert_eq!((tb.cursor, tb.selection_start), (7, -1));

        // Ctrl+A + typing replaces selection
        tb.apply(EditAction::SelectAll);
        assert_eq!((tb.cursor, tb.selection_start), (20, 1));
        let o = tb.apply(EditAction::InsertText("a".into()));
        assert_eq!((tb.text.as_str(), tb.cursor, tb.selection_start), ("a", 2, -1));
        assert!(o.changed_cursor && o.changed_selection && o.changed_text);

        // Cut & Paste
        tb.text = "abcdef".into();
        tb.selection_start = 2;
        tb.cursor = 5;
        let cut = tb.apply(EditAction::Cut);
        assert_eq!(cut.copied.as_deref(), Some("bcd"));
        assert_eq!((tb.text.as_str(), tb.cursor, tb.selection_start), ("aef", 2, -1));
        tb.apply(EditAction::End { shift: false });
        tb.apply(EditAction::Paste(cut.copied.unwrap()));
        assert_eq!((tb.text.as_str(), tb.cursor), ("aefbcd", 7));
    }

    #[test]
    fn multiline_navigation_and_single_line_enter_escape() {
        let mut tb = TextBoxState::new("");
        tb.multi_line = true;
        tb.clear_text_on_focus = false;
        tb.focus(None);
        tb.apply(EditAction::InsertText("line1".into()));
        tb.apply(EditAction::Enter);
        tb.apply(EditAction::InsertText("line2long".into()));
        tb.apply(EditAction::Enter);
        tb.apply(EditAction::InsertText("l3".into()));
        assert_eq!((tb.text.as_str(), tb.cursor), ("line1\nline2long\nl3", 19));

        tb.apply(EditAction::Up { shift: false });
        assert_eq!(tb.cursor, 9); // col 2 of line2long
        tb.apply(EditAction::End { shift: false });
        assert_eq!(tb.cursor, 16);
        tb.apply(EditAction::Home { shift: false });
        assert_eq!(tb.cursor, 7);
        tb.apply(EditAction::Down { shift: false });
        assert_eq!(tb.cursor, 17);
        tb.apply(EditAction::Up { shift: true });
        assert_eq!((tb.cursor, tb.selection_start), (7, 17));

        let esc = tb.apply(EditAction::Escape);
        assert_eq!(esc.lost_focus, Some(false));
        assert_eq!((tb.cursor, tb.selection_start, tb.focused), (-1, -1, false));

        // Single-line Enter blurs with enterPressed=true; Up/Down are no-ops.
        let mut sl = TextBoxState::new("hello");
        sl.clear_text_on_focus = false;
        sl.focus(None);
        sl.cursor = 3;
        sl.apply(EditAction::Up { shift: false });
        sl.apply(EditAction::Down { shift: false });
        assert_eq!(sl.cursor, 3);
        let ent = sl.apply(EditAction::Enter);
        assert_eq!(ent.lost_focus, Some(true));
        assert_eq!((sl.cursor, sl.selection_start, sl.focused), (-1, -1, false));
    }

    #[test]
    fn double_click_and_utf8_boundaries() {
        let mut tb = TextBoxState::new("alpha beta_two gamma");
        tb.clear_text_on_focus = false;
        tb.focus(None);
        tb.apply(EditAction::DoubleClickAt { pos: 9 }); // inside "beta_two"
        assert_eq!((tb.selection_start, tb.cursor), (7, 15));
        assert_eq!(tb.selection_byte_range(), Some((6, 14)));

        // Multi-byte UTF-8
        let mut u = TextBoxState::new("héllo");
        u.clear_text_on_focus = false;
        u.focus(None);
        assert_eq!(u.cursor, 7); // 'é' is 2 bytes -> len=6 -> pos=7
        u.apply(EditAction::Left { shift: false, word: false });
        assert_eq!(u.cursor, 6);
        u.apply(EditAction::Home { shift: false });
        u.apply(EditAction::Right { shift: false, word: false }); // past 'h'
        assert_eq!(u.cursor, 2);
        u.apply(EditAction::Right { shift: false, word: false }); // past 'é' (2 bytes)
        assert_eq!(u.cursor, 4);
        u.apply(EditAction::Backspace { word: false }); // deletes 'é'
        assert_eq!((u.text.as_str(), u.cursor), ("hllo", 2));
    }
}
