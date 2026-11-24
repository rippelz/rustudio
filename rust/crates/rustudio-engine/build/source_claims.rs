// Build-script source analysis shared by the root crate's build.rs and
// rustudio-engine's build.rs: the item / string-literal walk the
// member-claim tables are made from. It is `include!`d by both (build
// scripts have no dependencies of their own), so both see one
// implementation and the two generated tables cannot drift apart.

use std::collections::BTreeSet;
use std::path::Path;

fn words(text: &str, out: &mut BTreeSet<String>) {
    let mut cur = String::new();
    for c in text.chars().chain([' ']) {
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
        } else {
            if cur.chars().next().is_some_and(|c| !c.is_ascii_digit()) {
                out.insert(std::mem::take(&mut cur));
            }
            cur.clear();
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Open(u8),
    Close(u8),
    Semi,
    Comma,
    Pipe,
    OrOr,
    EqEq,
    Bang,
    Dot,
    Other,
}

/// Rust source as tokens: identifiers, string literals (plain, byte, raw),
/// brackets; comments and char literals dropped.
fn lex(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b'\'' {
            // A char literal ('"', '\'', 'x'); a lifetime ('a) is skipped.
            if b.get(i + 1) == Some(&b'\\') {
                i += 2;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            } else if b.get(i + 2) == Some(&b'\'') {
                i += 3;
            } else {
                // A lifetime: skipped with its name (`&'static str` read as
                // a `static` item named `str`, whose body was the rest of
                // the function, and linked anything naming `str` to it).
                i += 1;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
            }
        } else if (c == b'r' || c == b'b')
            && (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#') || (c == b'b' && b.get(i + 1) == Some(&b'r')))
        {
            let mut j = i + 1;
            if c == b'b' && b.get(j) == Some(&b'r') {
                j += 1;
            }
            let raw = c == b'r' || b[j - 1] == b'r';
            let mut hashes = 0;
            while raw && b.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) != Some(&b'"') {
                // An identifier starting with r / b.
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(Tok::Ident(src[s..i].to_string()));
                continue;
            }
            if raw {
                let close = format!("\"{}", "#".repeat(hashes));
                let start = j + 1;
                let end = src[start..].find(&close).map_or(b.len(), |e| start + e);
                out.push(Tok::Str(src[start..end].to_string()));
                i = end + close.len();
            } else {
                i = j;
                continue;
            }
        } else if c == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            out.push(Tok::Str(src[start..j.min(b.len())].to_string()));
            i = j + 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(src[s..i].to_string()));
        } else if c.is_ascii_whitespace() {
            i += 1;
        } else {
            out.push(match c {
                b'{' | b'(' | b'[' => Tok::Open(c),
                b'}' | b')' | b']' => Tok::Close(c),
                b';' => Tok::Semi,
                b',' => Tok::Comma,
                b'|' if b.get(i + 1) == Some(&b'|') => {
                    i += 1;
                    Tok::OrOr
                }
                b'|' => Tok::Pipe,
                b'=' if b.get(i + 1) == Some(&b'=') => {
                    i += 1;
                    Tok::EqEq
                }
                b'!' if b.get(i + 1) != Some(&b'=') => Tok::Bang,
                b'.' => Tok::Dot,
                _ => Tok::Other,
            });
            i += 1;
        }
    }
    out
}

/// A string literal that is one identifier-like word.
fn word_lit(t: &Tok) -> Option<&str> {
    match t {
        Tok::Str(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => Some(s),
        _ => None,
    }
}

/// `"A" | "B" | …` at `toks[i..]`: the words and the index after them.
fn alts(toks: &[Tok], mut i: usize) -> Option<(Vec<String>, usize)> {
    let mut out = vec![word_lit(toks.get(i)?)?.to_string()];
    i += 1;
    while toks.get(i) == Some(&Tok::Pipe) {
        out.push(word_lit(toks.get(i + 1)?)?.to_string());
        i += 2;
    }
    Some((out, i))
}

/// Literal pairs from `if` conditions that test the class and the member
/// with literals and nothing but `&&` (`if class == "A" && key == "K"`,
/// `if matches!(class, "A" | "B") && matches!(key, "K" | "L")`). Returns
/// the pairs and the member literals' token indices.
fn cond_pairs(toks: &[Tok]) -> (Vec<(String, String)>, BTreeSet<usize>) {
    let (mut out, mut used) = (Vec::new(), BTreeSet::new());
    for i in 0..toks.len() {
        if toks[i] != Tok::Ident("if".into()) {
            continue;
        }
        // The condition: up to the `{` at its own depth.
        let mut depth = 0i32;
        let mut end = None;
        for (j, t) in toks.iter().enumerate().skip(i + 1) {
            match t {
                Tok::Open(b'{') if depth == 0 => {
                    end = Some(j);
                    break;
                }
                Tok::Open(_) => depth += 1,
                Tok::Close(_) => depth -= 1,
                Tok::Semi => break,
                _ => {}
            }
            if depth < 0 {
                break;
            }
        }
        let Some(end) = end else { continue };
        let cond = &toks[i + 1..end];
        // (Any negation or alternative: left alone, its literals stay
        // unconstrained. A `!` is only allowed as `matches!`'s.)
        let negated = cond
            .iter()
            .enumerate()
            .any(|(j, t)| *t == Tok::Bang && (j == 0 || cond[j - 1] != Tok::Ident("matches".into())));
        if negated || cond.contains(&Tok::OrOr) || cond.contains(&Tok::Ident("let".into())) {
            continue;
        }
        let (mut classes, mut keys, mut key_at) = (Vec::new(), Vec::new(), Vec::new());
        for (j, t) in cond.iter().enumerate() {
            let Tok::Ident(var) = t else { continue };
            let is_class = var == "class";
            let is_key = var == "key" || var == "name";
            if !is_class && !is_key {
                continue;
            }
            // `var == "A"`
            let mut found: Vec<(String, usize)> = Vec::new();
            if cond.get(j + 1) == Some(&Tok::EqEq) {
                if let Some(w) = cond.get(j + 2).and_then(word_lit) {
                    found.push((w.to_string(), i + 1 + j + 2));
                }
            }
            // `matches!(var, "A" | "B")`
            if j >= 3
                && cond[j - 1] == Tok::Open(b'(')
                && cond[j - 2] == Tok::Bang
                && cond[j - 3] == Tok::Ident("matches".into())
                && cond.get(j + 1) == Some(&Tok::Comma)
            {
                let mut k = j + 2;
                while let Some(w) = cond.get(k).and_then(word_lit) {
                    found.push((w.to_string(), i + 1 + k));
                    if cond.get(k + 1) != Some(&Tok::Pipe) {
                        break;
                    }
                    k += 2;
                }
            }
            for (w, at) in found {
                if is_class {
                    classes.push(w);
                } else {
                    keys.push(w);
                    key_at.push(at);
                }
            }
        }
        if classes.is_empty() || keys.is_empty() {
            continue;
        }
        for c in &classes {
            for k in &keys {
                out.push((c.clone(), k.clone()));
            }
        }
        used.extend(key_at);
    }
    (out, used)
}

/// Literal pairs: tuples (patterns or values) that start with two
/// alternations of one-word string literals, `("A" | "B", "K" | "L", …)`
/// (a (class, member) table entry or match arm). Returns the pairs and the
/// token indices they used.
fn pairs(toks: &[Tok]) -> (Vec<(String, String)>, BTreeSet<usize>) {
    let (mut out, mut used) = (Vec::new(), BTreeSet::new());
    for i in 0..toks.len() {
        if toks[i] != Tok::Open(b'(') {
            continue;
        }
        let Some((firsts, j)) = alts(toks, i + 1) else { continue };
        if toks.get(j) != Some(&Tok::Comma) {
            continue;
        }
        let Some((seconds, k)) = alts(toks, j + 1) else { continue };
        if !matches!(toks.get(k), Some(Tok::Comma) | Some(Tok::Close(b')'))) {
            continue;
        }
        for a in &firsts {
            for b in &seconds {
                out.push((a.clone(), b.clone()));
            }
        }
        used.extend(i + 1..k);
    }
    (out, used)
}

/// Token indices of string literals that are plain arguments of a function
/// or method call (`d.prop(r, "CFrame")`, `is_a(class, "BasePart")`): a
/// property read or a class test, not a member the code answers. A call
/// that involves the member being answered (`key` / `name` among its
/// arguments or just before it, as in `key.starts_with("Get")`) keeps its
/// literals; so do macros (`matches!(key, "A" | "B")`).
fn call_args(toks: &[Tok]) -> BTreeSet<usize> {
    const NOT_CALLS: &[&str] = &["if", "while", "match", "in", "return", "for", "let", "else", "move", "fn"];
    // Message macros: their literals are text, not members.
    const TEXT_MACROS: &[&str] =
        &["format", "println", "eprintln", "print", "eprint", "panic", "write", "writeln", "unreachable", "todo"];
    let mut out = BTreeSet::new();
    for i in 0..toks.len() {
        let Tok::Ident(m) = &toks[i] else { continue };
        if !TEXT_MACROS.contains(&m.as_str()) || toks.get(i + 1) != Some(&Tok::Bang) {
            continue;
        }
        let Some(Tok::Open(_)) = toks.get(i + 2) else { continue };
        let mut depth = 0;
        for (j, t) in toks.iter().enumerate().skip(i + 2) {
            match t {
                Tok::Open(_) => depth += 1,
                Tok::Close(_) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Str(_) => {
                    out.insert(j);
                }
                _ => {}
            }
        }
    }
    // (open index, is a call)
    let mut stack: Vec<(usize, bool)> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Open(b'(') => {
                let call = matches!(
                    i.checked_sub(1).and_then(|p| toks.get(p)),
                    Some(Tok::Ident(f)) if !NOT_CALLS.contains(&f.as_str()) && f.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                );
                stack.push((i, call));
            }
            Tok::Close(b')') => {
                let Some((open, call)) = stack.pop() else { continue };
                if !call {
                    continue;
                }
                let member = |t: &Tok| matches!(t, Tok::Ident(x) if x == "key" || x == "name");
                let before = &toks[open.saturating_sub(8)..open];
                if toks[open + 1..i].iter().any(member) || before.iter().any(member) {
                    continue;
                }
                // Only the call's own arguments (not nested calls', already
                // decided, nor nested literal pairs), including the fields
                // of a tuple argument (`writes.push((r, "CFrame", v))`).
                let mut nest: Vec<bool> = Vec::new();
                for (j, t) in toks.iter().enumerate().take(i).skip(open + 1) {
                    match t {
                        Tok::Open(b) => {
                            let tuple = *b == b'('
                                && !matches!(toks.get(j - 1), Some(Tok::Ident(_)))
                                && nest.iter().all(|t| *t);
                            nest.push(tuple);
                        }
                        Tok::Close(_) => {
                            nest.pop();
                        }
                        Tok::Str(_) if nest.iter().all(|t| *t) => {
                            out.insert(j);
                        }
                        _ => {}
                    }
                }
            }
            Tok::Open(_) => stack.push((i, false)),
            Tok::Close(_) => {
                stack.pop();
            }
            _ => {}
        }
    }
    out
}

/// Items of a file: (name, identifiers used, string-literal words outside
/// literal pairs and call arguments, literal pairs), for every `fn`, `const` and `static` at
/// any depth (an item nested in another is also part of the outer one).
fn items(toks: &[Tok]) -> Vec<(String, BTreeSet<String>, BTreeSet<String>, Vec<(String, String)>)> {
    let mut out = Vec::new();
    for k in 0..toks.len() {
        let Tok::Ident(kw) = &toks[k] else { continue };
        if !matches!(kw.as_str(), "fn" | "const" | "static") {
            continue;
        }
        let Some(Tok::Ident(name)) = toks.get(k + 1) else { continue };
        if kw == "static" && name == "mut" {
            continue;
        }
        // fn: through its body's closing brace; const / static: to the `;`
        // at its own depth.
        let mut depth = 0i32;
        let mut end = toks.len();
        let mut j = k + 2;
        while j < toks.len() {
            match &toks[j] {
                Tok::Open(_) => depth += 1,
                Tok::Close(c) => {
                    depth -= 1;
                    if depth < 0 {
                        end = j;
                        break;
                    }
                    if depth == 0 && *c == b'}' && kw == "fn" {
                        end = j + 1;
                        break;
                    }
                }
                Tok::Semi if depth == 0 => {
                    end = j + 1;
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        let (mut ids, mut strs) = (BTreeSet::new(), BTreeSet::new());
        let body = &toks[k + 2..end];
        let (mut prs, mut used) = pairs(body);
        let (cprs, cused) = cond_pairs(body);
        prs.extend(cprs);
        used.extend(cused);
        let args = call_args(body);
        // (A fn's references start at its body: its signature only names types.)
        let refs_from = if kw == "fn" { body.iter().position(|t| *t == Tok::Open(b'{')).unwrap_or(0) } else { 0 };
        for (n, t) in body.iter().enumerate() {
            match t {
                // An item reference: a call `f(`, a function value `f,` /
                // `f)`, or a CONST / STATIC.
                // (`f.call(…)` is a Lua function's method, not the domain's
                // `call` dispatcher: it linked ext's prop_get to every
                // literal its call reaches. Only the three dispatcher names:
                // a domain's own methods called as `state.foo(…)` stay
                // linked.)
                Tok::Ident(i)
                    if n >= refs_from
                        && !(matches!(i.as_str(), "call" | "prop_get" | "newindex")
                            && matches!(n.checked_sub(1).and_then(|p| body.get(p)), Some(Tok::Dot)))
                        && (matches!(body.get(n + 1), Some(Tok::Open(b'(')) | Some(Tok::Comma) | Some(Tok::Close(b')')))
                            || (i.len() > 1 && i.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()))) =>
                {
                    ids.insert(i.clone());
                }
                // (A literal with whitespace, brackets or quotes in it is
                // text, a message or Luau source the code builds, never a
                // member key: the words of `game:GetService("CoreGui")` in
                // a CoreScript shim made gui_input claim GetService on
                // every class, so every GetService took the general path.)
                Tok::Str(s)
                    if !used.contains(&n)
                        && !args.contains(&n)
                        && !s.chars().any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\'')) =>
                {
                    words(s, &mut strs)
                }
                _ => {}
            }
        }
        out.push((name.clone(), ids, strs, prs));
    }
    out
}

/// One claimer's claim: the literal words its `root` fn reaches through the
/// items it names, and the (key, class) literal pairs in that code.
fn claim_set(prefixes: &[&str], root: &str) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let mut all = Vec::new();
    for prefix in prefixes {
        let (dir, stem) = match prefix.rsplit_once('/') {
            Some((d, s)) => (Path::new("src").join(d), s.to_string()),
            None => (Path::new("src").to_path_buf(), prefix.to_string()),
        };
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        files.sort();
        for p in files {
            let Some(file) = p.file_name().and_then(|f| f.to_str()) else { continue };
            let Some(base) = file.strip_suffix(".rs") else { continue };
            if base != stem && !base.starts_with(&format!("{stem}_")) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&p) else { continue };
            all.extend(items(&lex(&src)));
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue = vec![root.to_string()];
    let mut set = BTreeSet::new();
    let mut prs = BTreeSet::new();
    while let Some(n) = queue.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        for (iname, ids, strs, p) in &all {
            if *iname == n {
                set.extend(strs.iter().cloned());
                prs.extend(p.iter().cloned());
                queue.extend(ids.iter().filter(|i| !seen.contains(*i)).cloned());
            }
        }
    }
    (set, prs)
}

/// `(key, class)` pairs sorted by key, as the generated tables write them.
fn pair_row(prs: &BTreeSet<(String, String)>) -> String {
    let mut by_key: Vec<_> = prs.iter().map(|(a, b)| (b.clone(), a.clone())).collect();
    by_key.sort();
    let mut out = String::new();
    for (k, a) in by_key {
        out += &format!("({k:?},{a:?}),");
    }
    out
}
