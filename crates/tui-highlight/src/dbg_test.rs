#[test]
fn debug_c_comment_tree() {
    let src = "int x; /* open\n  still inside\n */ int y;\n";
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter::Language::from(tree_sitter_c::LANGUAGE);
    parser.set_language(&lang).unwrap();
    let tree = parser.parse(src, None).unwrap();
    fn walk(n: tree_sitter::Node, depth: usize, src: &str) {
        let t = n.utf8_text(src.as_bytes()).map(|s| s.replace('\n','\\n')).unwrap_or("?".into());
        println!("{:width$}{:?} named={} [{}..{}]", "", n.kind(), n.is_named(), n.start_byte(), n.end_byte(), width=depth*2);
        for i in 0..n.child_count() {
            if let Some(c) = n.child(i) { walk(c, depth+1, src); }
        }
    }
    walk(tree.root_node(), 0, src);
}
