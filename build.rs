use std::path::PathBuf;

fn main() {
    let grammar_dir = PathBuf::from("vendor/tree-sitter-gdscript");

    let mut c = cc::Build::new();
    c.std("c11").include(&grammar_dir);

    #[cfg(target_env = "msvc")]
    c.flag("-utf-8");

    let parser = grammar_dir.join("parser.c");
    c.file(&parser);
    println!("cargo:rerun-if-changed={}", parser.display());

    let scanner = grammar_dir.join("scanner.c");
    c.file(&scanner);
    println!("cargo:rerun-if-changed={}", scanner.display());

    c.compile("tree-sitter-gdscript");
}
