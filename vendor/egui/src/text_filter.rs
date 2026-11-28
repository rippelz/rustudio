//! rustudio patch: an app-wide hook that can rewrite the text of widgets and
//! `Painter::text` before layout (used by rustudio's Luau Panic Mode).
//! Monospace text (code, logs) is left alone.

use std::sync::OnceLock;

use epaint::text::{ByteIndex, ByteRangeExt as _, LayoutJob, LayoutSection};
use epaint::FontFamily;

/// Returns the replacement for a piece of text, or `None` to keep it.
pub type Filter = fn(&str) -> Option<String>;

static FILTER: OnceLock<Filter> = OnceLock::new();

/// Installs the filter (the first call wins).
pub fn set(filter: Filter) {
    let _ = FILTER.set(filter);
}

/// `text` through the filter.
pub fn apply(text: &str, family: &FontFamily) -> Option<String> {
    if *family == FontFamily::Monospace {
        return None;
    }
    FILTER.get().and_then(|f| f(text))
}

/// The job with each section's text through the filter.
pub fn apply_job(job: LayoutJob) -> LayoutJob {
    if FILTER.get().is_none() {
        return job;
    }
    let replaced: Vec<Option<String>> = job
        .sections
        .iter()
        .map(|s| apply(s.byte_range.slice(&job.text), &s.format.font_id.family))
        .collect();
    if replaced.iter().all(Option::is_none) {
        return job;
    }
    let mut out = LayoutJob { text: String::new(), sections: Vec::new(), ..job.clone() };
    for (section, new) in job.sections.into_iter().zip(replaced) {
        let start = out.text.len();
        out.text.push_str(new.as_deref().unwrap_or(section.byte_range.slice(&job.text)));
        out.sections.push(LayoutSection { byte_range: ByteIndex(start)..ByteIndex(out.text.len()), ..section });
    }
    out
}
