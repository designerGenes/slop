use std::path::Path;

use tree_sitter::{Language, Parser, Query, QueryCursor};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TagKind {
    Def,
    Ref,
}

#[derive(Debug, Clone)]
pub struct Tag {
    pub rel_fname: String,
    pub line: usize,
    pub name: String,
    pub kind: TagKind,
}

struct LangEntry {
    language: Language,
    query_src: &'static str,
}

// tree-sitter-gdscript 6.x ships its grammar via the new tree-sitter-language
// crate and does not export a TAGS_QUERY, so we link the C symbol directly
// (it returns a `TSLanguage*`, which is ABI-compatible with tree-sitter 0.22's
// `#[repr(transparent)] Language`) and supply our own SCM query.
unsafe extern "C" {
    unsafe fn tree_sitter_gdscript() -> Language;
}

// GDScript tags query. Capture convention matches the other grammars:
//   `@name`              → the identifier node whose text becomes the tag name
//   `@definition.<kind>` → marks the match as a definition
//   `@reference.<kind>`  → marks the match as a reference
const GDSCRIPT_TAGS_QUERY: &str = r#"
(class_definition
  name: (name) @name) @definition.class

(enum_definition
  name: (name) @name) @definition.enum

(function_definition
  name: (name) @name) @definition.function

(constructor_definition) @definition.function

(signal_statement
  name: (name) @name) @definition.macro

(variable_statement
  name: (name) @name) @definition.constant

(export_variable_statement
  name: (name) @name) @definition.constant

(onready_variable_statement
  name: (name) @name) @definition.constant

(class_name_statement
  (name) @name) @definition.class

(call
  (identifier) @name) @reference.call

(attribute_call
  (identifier) @name) @reference.call
"#;

fn lang_entry(path: &Path) -> Option<LangEntry> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    let entry = match ext.as_str() {
        "rs" => LangEntry {
            language: tree_sitter_rust::language(),
            query_src: tree_sitter_rust::TAGS_QUERY,
        },
        "py" => LangEntry {
            language: tree_sitter_python::language(),
            query_src: tree_sitter_python::TAGS_QUERY,
        },
        "js" | "jsx" | "mjs" | "cjs" => LangEntry {
            language: tree_sitter_javascript::language(),
            query_src: tree_sitter_javascript::TAGS_QUERY,
        },
        "ts" => LangEntry {
            language: tree_sitter_typescript::language_typescript(),
            query_src: tree_sitter_typescript::TAGS_QUERY,
        },
        "tsx" => LangEntry {
            language: tree_sitter_typescript::language_tsx(),
            query_src: tree_sitter_typescript::TAGS_QUERY,
        },
        "go" => LangEntry {
            language: tree_sitter_go::language(),
            query_src: tree_sitter_go::TAGS_QUERY,
        },
        "c" | "h" => LangEntry {
            language: tree_sitter_c::language(),
            query_src: tree_sitter_c::TAGS_QUERY,
        },
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => LangEntry {
            language: tree_sitter_cpp::language(),
            query_src: tree_sitter_cpp::TAGS_QUERY,
        },
        "java" => LangEntry {
            language: tree_sitter_java::language(),
            query_src: tree_sitter_java::TAGS_QUERY,
        },
        "rb" => LangEntry {
            language: tree_sitter_ruby::language(),
            query_src: tree_sitter_ruby::TAGS_QUERY,
        },
        "gd" => LangEntry {
            language: unsafe { tree_sitter_gdscript() },
            query_src: GDSCRIPT_TAGS_QUERY,
        },
        "swift" => LangEntry {
            language: tree_sitter_swift::language(),
            query_src: tree_sitter_swift::TAGS_QUERY,
        },
        _ => return None,
    };
    Some(entry)
}

pub fn extract_tags(fname: &str, rel_fname: &str) -> Vec<Tag> {
    let path = Path::new(fname);
    let entry = match lang_entry(path) {
        Some(e) => e,
        None => return Vec::new(),
    };

    let source = match std::fs::read_to_string(fname) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut parser = Parser::new();
    if parser.set_language(&entry.language).is_err() {
        return Vec::new();
    }

    let tree = match parser.parse(source.as_bytes(), None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let query = match Query::new(&entry.language, entry.query_src) {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };

    let capture_names = query.capture_names();

    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

    let mut tags = Vec::new();

    for m in matches {
        let mut kind = None;
        let mut name_node = None;

        for cap in m.captures {
            let cap_name = capture_names
                .get(cap.index as usize)
                .copied()
                .unwrap_or("");

            if cap_name.starts_with("definition.") {
                kind = Some(TagKind::Def);
            } else if cap_name.starts_with("reference.") {
                kind = Some(TagKind::Ref);
            } else if cap_name == "name" {
                name_node = Some(cap.node);
            }
        }

        if let (Some(kind), Some(node)) = (kind, name_node) {
            let line = node.start_position().row + 1;
            let name = node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .to_string();

        tags.push(Tag {
            rel_fname: rel_fname.to_string(),
            line,
            name,
            kind,
        });
        }
    }

    tags
}

#[cfg(test)]
mod tests {
    use super::{extract_tags, TagKind};
    use std::io::Write;

    fn write_temp(name: &str, contents: &str) -> (String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join("soupify_tags_tests");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        (path.to_string_lossy().to_string(), path)
    }

    #[test]
    fn extracts_gdscript_definitions_and_calls() {
        let src = "extends Node\n\
                   class_name Enemy\n\
                   signal hit_landed(damage)\n\
                   var max_health := 100\n\
                   func take_damage(amount: int) -> void:\n\
                       apply_hit(amount)\n\
                   func apply_hit(_amount: int) -> void:\n\
                       pass\n";
        let (fname, _path) = write_temp("enemy.gd", src);
        let tags = extract_tags(&fname, "enemy.gd");

        let def_names: Vec<&str> = tags
            .iter()
            .filter(|t| t.kind == TagKind::Def)
            .map(|t| t.name.as_str())
            .collect();

        assert!(def_names.contains(&"Enemy"), "defs: {:?}", def_names);
        assert!(def_names.contains(&"take_damage"), "defs: {:?}", def_names);
        assert!(def_names.contains(&"apply_hit"), "defs: {:?}", def_names);
        assert!(def_names.contains(&"hit_landed"), "defs: {:?}", def_names);
        assert!(def_names.contains(&"max_health"), "defs: {:?}", def_names);

        let ref_names: Vec<&str> = tags
            .iter()
            .filter(|t| t.kind == TagKind::Ref)
            .map(|t| t.name.as_str())
            .collect();
        assert!(ref_names.contains(&"apply_hit"), "refs: {:?}", ref_names);

        let take_damage_line = tags
            .iter()
            .find(|t| t.name == "take_damage")
            .map(|t| t.line)
            .unwrap();
        assert!(take_damage_line >= 5 && take_damage_line <= 6);
    }

    #[test]
    fn extracts_swift_definitions() {
        let src = "protocol Drawable {\n\
                    func draw()\n\
                   }\n\
                   class Shape: Drawable {\n\
                       let name: String\n\
                       func draw() { print(\"x\") }\n\
                   }\n\
                   struct Point { var x: Double }\n\
                   enum Color { case red }\n\
                   func makeShape() -> Shape { return Shape() }\n";
        let (fname, _path) = write_temp("App.swift", src);
        let tags = extract_tags(&fname, "App.swift");

        let def_names: Vec<&str> = tags
            .iter()
            .filter(|t| t.kind == TagKind::Def)
            .map(|t| t.name.as_str())
            .collect();

        assert!(def_names.contains(&"Drawable"), "defs: {:?}", def_names);
        assert!(def_names.contains(&"Shape"), "defs: {:?}", def_names);
        assert!(def_names.contains(&"makeShape"), "defs: {:?}", def_names);
    }

    #[test]
    fn unknown_extension_yields_no_tags() {
        let (fname, _path) = write_temp("notes.txt", "func nothing here");
        let tags = extract_tags(&fname, "notes.txt");
        assert!(tags.is_empty());
    }
}
