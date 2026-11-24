//! The script documents open in the editor, shared by the editor's script
//! tabs and the plugins' ScriptEditorService (api_script_editor.rs). One
//! registry per process: the editor opens / closes tabs and moves the
//! cursor; plugins open / close documents, edit them and set the
//! selection. Each change queues the event Studio fires
//! (TextDocumentDidOpen / DidClose / DidChange, ScriptDocument
//! SelectionChanged / ViewportChanged); the plugin host fires them after
//! the frame's plugin code yielded (REF's scenarios capture: DidOpen after
//! OpenScriptDocumentAsync returned). Editor-side requests (show a tab,
//! close one, move the cursor) queue for the editor.
//!
//! Positions are Studio's: 1-based lines and characters, a range's end
//! exclusive (the whole of "a\n-- b\n" is 1,1 to 3,1).


use rbx_dom_weak::types::Ref;

#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub script: Ref,
    /// (cursor line, cursor char, anchor line, anchor char).
    pub selection: (i64, i64, i64, i64),
    /// First and last visible line.
    pub viewport: (i64, i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextChange {
    pub start: (i64, i64),
    pub end: (i64, i64),
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DocEvent {
    Opened(Ref),
    Closed(Ref),
    Changed(Ref, Vec<TextChange>),
    Selection(Ref, (i64, i64, i64, i64)),
    Viewport(Ref, (i64, i64)),
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditorReq {
    /// Show the script's tab (focus it), with the cursor on `line` if given.
    Show(Ref, Option<i64>),
    Close(Ref),
    /// Put the cursor / selection there.
    Select(Ref, (i64, i64, i64, i64)),
}

#[derive(Default)]
struct State {
    docs: Vec<Doc>,
    active: Option<Ref>,
    events: Vec<DocEvent>,
    reqs: Vec<EditorReq>,
    /// The editor's script tabs at the last `sync_tabs`.
    tabs: Vec<Ref>,
}

thread_local! {
    /// The editor and the plugin runtime both run on the UI thread; per
    /// thread, so parallel tests (each its own host and document) don't
    /// see each other's documents.
    static STATE: std::cell::RefCell<State> = std::cell::RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

const MAX_EVENTS: usize = 512;

fn event(s: &mut State, e: DocEvent) {
    if s.events.len() >= MAX_EVENTS {
        s.events.remove(0);
    }
    s.events.push(e);
}

/// Opens (or focuses) a script's document; true if it was newly opened.
/// `from_editor`: the person opened the tab (no request back).
pub fn open(script: Ref, line: Option<i64>, from_editor: bool) -> bool {
    with(|s| {
        let new = !s.docs.iter().any(|d| d.script == script);
        if new {
            let l = line.unwrap_or(1).max(1);
            s.docs.push(Doc { script, selection: (l, 1, l, 1), viewport: (1, 1) });
            event(s, DocEvent::Opened(script));
        } else if let (Some(l), Some(d)) = (line, s.docs.iter_mut().find(|d| d.script == script)) {
            d.selection = (l.max(1), 1, l.max(1), 1);
        }
        s.active = Some(script);
        if !from_editor {
            s.reqs.push(EditorReq::Show(script, line));
        }
        new
    })
}

/// Closes a document; true if it was open.
pub fn close(script: Ref, from_editor: bool) -> bool {
    with(|s| {
        let Some(i) = s.docs.iter().position(|d| d.script == script) else { return false };
        s.docs.remove(i);
        event(s, DocEvent::Closed(script));
        if s.active == Some(script) {
            s.active = s.docs.last().map(|d| d.script);
        }
        if !from_editor {
            s.reqs.push(EditorReq::Close(script));
        }
        true
    })
}

pub fn is_open(script: Ref) -> bool {
    with(|s| s.docs.iter().any(|d| d.script == script))
}

pub fn docs() -> Vec<Doc> {
    with(|s| s.docs.clone())
}

pub fn doc(script: Ref) -> Option<Doc> {
    with(|s| s.docs.iter().find(|d| d.script == script).cloned())
}

/// StudioService.ActiveScript: the script whose tab is in front.
pub fn active() -> Option<Ref> {
    with(|s| s.active)
}

/// The editor brought a tab to the front.
pub fn set_active(script: Option<Ref>) {
    with(|s| s.active = script)
}

/// The text of an open document changed (`changes` in Studio's form).
pub fn changed(script: Ref, changes: Vec<TextChange>) {
    with(|s| {
        if s.docs.iter().any(|d| d.script == script) && !changes.is_empty() {
            event(s, DocEvent::Changed(script, changes));
        }
    })
}

/// The cursor / selection moved (`from_editor`: the person moved it;
/// otherwise the editor is asked to show it).
pub fn set_selection(script: Ref, sel: (i64, i64, i64, i64), from_editor: bool) {
    with(|s| {
        let Some(d) = s.docs.iter_mut().find(|d| d.script == script) else { return };
        if d.selection == sel {
            return;
        }
        d.selection = sel;
        event(s, DocEvent::Selection(script, sel));
        if !from_editor {
            s.reqs.retain(|r| !matches!(r, EditorReq::Select(x, _) if *x == script));
            s.reqs.push(EditorReq::Select(script, sel));
        }
    })
}

pub fn set_viewport(script: Ref, vp: (i64, i64)) {
    with(|s| {
        let Some(d) = s.docs.iter_mut().find(|d| d.script == script) else { return };
        if d.viewport != vp {
            d.viewport = vp;
            event(s, DocEvent::Viewport(script, vp));
        }
    })
}

pub fn has_events() -> bool {
    with(|s| !s.events.is_empty())
}

pub fn take_events() -> Vec<DocEvent> {
    with(|s| std::mem::take(&mut s.events))
}

fn event_script(e: &DocEvent) -> Ref {
    match e {
        DocEvent::Opened(r) | DocEvent::Closed(r) | DocEvent::Changed(r, _) | DocEvent::Selection(r, _) | DocEvent::Viewport(r, _) => *r,
    }
}

/// The events of scripts `mine` accepts (the ones in the caller's
/// document); the others stay for the runtime they belong to.
pub fn take_events_where(mine: impl Fn(Ref) -> bool) -> Vec<DocEvent> {
    with(|s| {
        let (take, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut s.events).into_iter().partition(|e| mine(event_script(e)));
        s.events = keep;
        take
    })
}

/// The editor's Show / Close requests (a tab's selection requests stay
/// for [`take_selection_request`]).
pub fn take_requests() -> Vec<EditorReq> {
    with(|s| {
        let (sel, other): (Vec<_>, Vec<_>) = std::mem::take(&mut s.reqs).into_iter().partition(|r| matches!(r, EditorReq::Select(..)));
        s.reqs = sel;
        other
    })
}

/// The latest selection a plugin asked for in `script`'s tab, once.
pub fn take_selection_request(script: Ref) -> Option<(i64, i64, i64, i64)> {
    with(|s| {
        let mut out = None;
        s.reqs.retain(|r| match r {
            EditorReq::Select(x, sel) if *x == script => {
                out = Some(*sel);
                false
            }
            _ => true,
        });
        out
    })
}

/// The editor's script tabs this frame: a tab that went away closes its
/// document (the person closed it), a new one opens it.
pub fn sync_tabs(tabs: &[Ref]) {
    let (gone, new) = with(|s| {
        let gone: Vec<Ref> = s.tabs.iter().copied().filter(|r| !tabs.contains(r)).collect();
        let new: Vec<Ref> = tabs.iter().copied().filter(|r| !s.tabs.contains(r)).collect();
        s.tabs = tabs.to_vec();
        (gone, new)
    });
    for r in gone {
        close(r, true);
    }
    for r in new {
        if !is_open(r) {
            open(r, None, true);
        }
    }
}

/// A new place: every document closes (no events: the plugins reload too).
pub fn reset() {
    with(|s| *s = State::default())
}

// ------------------------------------------------------------- positions

/// (line, character) of a char index (1-based).
pub fn pos_of(text: &str, ci: usize) -> (i64, i64) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in text.chars().enumerate() {
        if i == ci {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// The char index of (line, character), clamped into the text.
pub fn index_of(text: &str, line: i64, ch: i64) -> usize {
    let mut cur = 1;
    let mut start = 0usize;
    let chars: Vec<char> = text.chars().collect();
    if line > 1 {
        for (i, c) in chars.iter().enumerate() {
            if *c == '\n' {
                cur += 1;
                if cur == line {
                    start = i + 1;
                    break;
                }
            }
        }
        if cur < line {
            return chars.len();
        }
    }
    let mut end = start;
    while end < chars.len() && chars[end] != '\n' {
        end += 1;
    }
    (start + (ch.max(1) as usize - 1)).min(end)
}

/// Lines as Studio counts them ("a\n-- b\n" has 3).
pub fn line_count(text: &str) -> i64 {
    text.split('\n').count() as i64
}

/// The end position of a text (the last line, one past its last char).
pub fn end_of(text: &str) -> (i64, i64) {
    pos_of(text, text.chars().count())
}

/// The one change turning `old` into `new` (common prefix and suffix kept).
pub fn diff(old: &str, new: &str) -> Option<TextChange> {
    if old == new {
        return None;
    }
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    let mut q = 0;
    while q < a.len() - p && q < b.len() - p && a[a.len() - 1 - q] == b[b.len() - 1 - q] {
        q += 1;
    }
    Some(TextChange {
        start: pos_of(old, p),
        end: pos_of(old, a.len() - q),
        text: b[p..b.len() - q].iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_studios() {
        let t = "print('hello')\n-- line 2\n";
        assert_eq!(line_count(t), 3);
        assert_eq!(end_of(t), (3, 1));
        assert_eq!(index_of(t, 2, 1), 15);
        assert_eq!(index_of(t, 1, 1), 0);
        assert_eq!(index_of(t, 9, 1), t.len());
        let c = diff("abc\ndef", "abXc\ndef").unwrap();
        assert_eq!((c.start, c.end, c.text.as_str()), ((1, 3), (1, 3), "X"));
    }
}
