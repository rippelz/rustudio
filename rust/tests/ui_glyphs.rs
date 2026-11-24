//! Check Rust's hard-coded UI text against the egui families available on a
//! clean machine. Tokenize Rust so comments, raw strings and escapes work.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use skrifa::MetadataProvider;

fn collect(tokens: TokenStream, path: &Path, glyphs: &mut BTreeMap<char, BTreeSet<String>>) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => {
                // Doc comments are desugared to #[doc = "..."].
                if group.delimiter() == Delimiter::Bracket
                    && matches!(group.stream().into_iter().next(), Some(TokenTree::Ident(id)) if id == "doc")
                {
                    continue;
                }
                collect(group.stream(), path, glyphs);
            }
            TokenTree::Literal(literal) => {
                let line = literal.span().start().line;
                let Ok(value) = syn::parse2::<syn::Lit>(TokenTree::Literal(literal).into()) else {
                    continue;
                };
                let value = match value {
                    syn::Lit::Str(text) => text.value(),
                    syn::Lit::Char(ch) => ch.value().to_string(),
                    _ => continue,
                };
                for ch in value.chars().filter(|c| !c.is_ascii()) {
                    let file = path.file_name().unwrap().to_str().unwrap();
                    if matches!(
                        (file, ch),
                        // Unicode transport/segmentation test data.
                        ("scripts.rs" | "writer.rs", '🦊')
                            | ("globals_parity.rs", '\u{200d}')
                        // The Studio font fixtures exercise runtime-provided fonts.
                            | ("gui.rs", 'ו' | 'ל' | 'ם' | 'ע' | 'ש' | '\u{200d}' | '\u{fe0f}' | '🛒')
                        // The Output stack marker is stripped before display.
                            | ("runtime.rs" | "ui_shots.rs", '\u{2063}')
                    ) {
                        continue;
                    }
                    glyphs
                        .entry(ch)
                        .or_default()
                        .insert(format!("{}:{line}", path.display()));
                }
            }
            _ => {}
        }
    }
}

fn rust_files(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in dir.read_dir().unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, paths);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            paths.push(path);
        }
    }
}

#[test]
fn ui_literals_use_bundled_glyphs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    rust_files(&root.join("src"), &mut paths);
    if let Ok(crates) = root.join("crates").read_dir() {
        for krate in crates {
            let src = krate.unwrap().path().join("src");
            if src.is_dir() {
                rust_files(&src, &mut paths);
            }
        }
    }

    let mut glyphs = BTreeMap::new();
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        let tokens = source
            .parse()
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        collect(tokens, path.strip_prefix(root).unwrap(), &mut glyphs);
    }

    let defs = rustudio_engine::ti::bundled_fonts();
    let covered = |family: &egui::FontFamily, ch| {
        defs.families[family].iter().any(|name| {
            let font = &defs.font_data[name];
            skrifa::FontRef::from_index(&font.font, font.index)
                .unwrap()
                .charmap()
                .map(ch)
                .is_some()
        })
    };
    for name in ["check", "chevron-down", "pencil", "x"] {
        let (ch, family) = rustudio_engine::ti::glyph(name).unwrap();
        assert!(covered(&family, ch), "Missing bundled icon {name}");
    }
    let missing: Vec<_> = glyphs
        .iter()
        .filter_map(|(&ch, paths)| {
            (!covered(&egui::FontFamily::Proportional, ch)
                || !covered(&egui::FontFamily::Monospace, ch))
            .then(|| format!("{ch} (U+{:04X}) {paths:?}", ch as u32))
        })
        .collect();
    println!(
        "Checked {} glyphs: {}",
        glyphs.len(),
        glyphs.keys().collect::<String>()
    );
    assert!(
        missing.is_empty(),
        "Missing bundled UI glyphs:\n{}",
        missing.join("\n")
    );
}
