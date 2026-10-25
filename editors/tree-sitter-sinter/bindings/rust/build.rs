//! Compile the generated parser (`src/parser.c`).

fn main() {
    let src = std::path::Path::new("src");
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(src)
        .file(src.join("parser.c"))
        .warnings(false);
    build.compile("tree-sitter-sinter");
    println!("cargo:rerun-if-changed=src/parser.c");
}
