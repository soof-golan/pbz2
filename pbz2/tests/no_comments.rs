use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use proc_macro2::{LineColumn, TokenStream, TokenTree};
use syn::visit::{self, Visit};
use syn::{Attribute, Fields, ImplItem, Item, TraitItem, Visibility};

fn rust_files_in(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("the folder entry can be read").path();
        if path.is_dir() {
            rust_files_in(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

fn rust_files() -> Vec<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    for krate in ["pbz2", "pbz2-cli", "pbz2-core"] {
        for folder in ["src", "tests"] {
            rust_files_in(&workspace.join(krate).join(folder), &mut found);
        }
    }
    found.sort();
    assert!(!found.is_empty());
    found
}

fn token_places(stream: TokenStream, places: &mut Vec<(LineColumn, LineColumn)>) {
    for tree in stream {
        match tree {
            TokenTree::Group(group) => {
                places.push((group.span_open().start(), group.span_open().end()));
                token_places(group.stream(), places);
                places.push((group.span_close().start(), group.span_close().end()));
            }
            other => places.push((other.span().start(), other.span().end())),
        }
    }
}

fn byte_offset(text: &str, place: LineColumn) -> usize {
    let line_start: usize = text
        .split_inclusive('\n')
        .take(place.line - 1)
        .map(str::len)
        .sum();
    let line = &text[line_start..];
    line_start
        + line
            .char_indices()
            .nth(place.column)
            .map_or(line.len(), |(offset, _)| offset)
}

fn comments_outside_tokens(text: &str) -> Vec<usize> {
    let stream: TokenStream = text.parse().expect("the file tokenizes");
    let mut places = Vec::new();
    token_places(stream, &mut places);
    let mut covered: Vec<(usize, usize)> = places
        .into_iter()
        .map(|(start, end)| (byte_offset(text, start), byte_offset(text, end)))
        .collect();
    covered.sort_unstable();
    let mut lines = Vec::new();
    let mut from = 0;
    for (start, end) in covered.into_iter().chain([(text.len(), text.len())]) {
        let gap = text.get(from..start).unwrap_or_default();
        if !gap.trim().is_empty() {
            let comment_at = from + gap.len() - gap.trim_start().len();
            lines.push(text[..comment_at].matches('\n').count() + 1);
        }
        from = from.max(end);
    }
    lines
}

fn is_doc(attribute: &Attribute) -> bool {
    attribute.path().is_ident("doc")
}

fn place_of(attribute: &Attribute) -> (usize, usize) {
    let start = attribute.pound_token.span.start();
    (start.line, start.column)
}

#[derive(Default)]
struct EveryDoc(BTreeSet<(usize, usize)>);

impl Visit<'_> for EveryDoc {
    fn visit_attribute(&mut self, attribute: &Attribute) {
        if is_doc(attribute) {
            self.0.insert(place_of(attribute));
        }
        visit::visit_attribute(self, attribute);
    }
}

fn allow(attributes: &[Attribute], allowed: &mut BTreeSet<(usize, usize)>) {
    allowed.extend(
        attributes
            .iter()
            .filter(|attribute| is_doc(attribute))
            .map(place_of),
    );
}

const fn is_public(visibility: &Visibility) -> bool {
    matches!(visibility, Visibility::Public(_))
}

fn allow_public_fields(fields: &Fields, allowed: &mut BTreeSet<(usize, usize)>) {
    for field in fields {
        if is_public(&field.vis) {
            allow(&field.attrs, allowed);
        }
    }
}

fn allow_public_interface(items: &[Item], allowed: &mut BTreeSet<(usize, usize)>) {
    for item in items {
        match item {
            Item::Struct(item) if is_public(&item.vis) => {
                allow(&item.attrs, allowed);
                allow_public_fields(&item.fields, allowed);
            }
            Item::Enum(item) if is_public(&item.vis) => {
                allow(&item.attrs, allowed);
                for variant in &item.variants {
                    allow(&variant.attrs, allowed);
                    allow_public_fields(&variant.fields, allowed);
                }
            }
            Item::Trait(item) if is_public(&item.vis) => {
                allow(&item.attrs, allowed);
                for trait_item in &item.items {
                    if let TraitItem::Fn(function) = trait_item {
                        allow(&function.attrs, allowed);
                    }
                }
            }
            Item::Impl(item) if item.trait_.is_none() => {
                for impl_item in &item.items {
                    match impl_item {
                        ImplItem::Fn(function) if is_public(&function.vis) => {
                            allow(&function.attrs, allowed);
                        }
                        ImplItem::Const(constant) if is_public(&constant.vis) => {
                            allow(&constant.attrs, allowed);
                        }
                        _ => {}
                    }
                }
            }
            Item::Mod(item) if is_public(&item.vis) => {
                allow(&item.attrs, allowed);
                if let Some((_, items)) = &item.content {
                    allow_public_interface(items, allowed);
                }
            }
            Item::Fn(item) if is_public(&item.vis) => allow(&item.attrs, allowed),
            Item::Const(item) if is_public(&item.vis) => allow(&item.attrs, allowed),
            Item::Static(item) if is_public(&item.vis) => allow(&item.attrs, allowed),
            Item::Type(item) if is_public(&item.vis) => allow(&item.attrs, allowed),
            Item::Use(item) if is_public(&item.vis) => allow(&item.attrs, allowed),
            _ => {}
        }
    }
}

fn docs_off_the_public_interface(path: &Path, text: &str) -> Vec<usize> {
    let file = syn::parse_file(text).expect("the file parses");
    let mut every = EveryDoc::default();
    every.visit_file(&file);
    let mut allowed = BTreeSet::new();
    if path.ends_with("src/lib.rs") {
        allow(&file.attrs, &mut allowed);
    }
    allow_public_interface(&file.items, &mut allowed);
    every
        .0
        .difference(&allowed)
        .map(|(line, _)| *line)
        .collect()
}

#[test]
fn only_public_items_have_comments() {
    let mut problems = Vec::new();
    for path in rust_files() {
        let text = std::fs::read_to_string(&path).expect("the file can be read");
        let shown = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&path)
            .display()
            .to_string();
        for line in comments_outside_tokens(&text) {
            problems.push(format!(
                "{shown}:{line}: a comment; only doc comments on the public interface are allowed"
            ));
        }
        for line in docs_off_the_public_interface(&path, &text) {
            problems.push(format!(
                "{shown}:{line}: a doc comment on something that is not public"
            ));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn comment_check_finds_violations() {
    let text = "/// public\npub fn shown() {}\n\n/// private\nfn hidden() {\n    let x = 1; // why\n}\n/* block */\n";
    assert_eq!(comments_outside_tokens(text), [6, 8]);
    assert_eq!(
        docs_off_the_public_interface(Path::new("src/x.rs"), text),
        [4]
    );
}
