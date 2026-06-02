use tree_sitter::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub stmts: Vec<(usize, usize)>,
    pub succ: Vec<BlockId>,
}

#[derive(Debug, Clone)]
pub struct Cfg {
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    pub exit: BlockId,
}

pub fn build_cfg(function_node: Node) -> Cfg {
    let mut blocks = vec![
        BasicBlock {
            stmts: Vec::new(),
            succ: vec![BlockId(1)],
        },
        BasicBlock {
            stmts: Vec::new(),
            succ: Vec::new(),
        },
    ];

    if let Some(body) = find_descendant(function_node, "compound_statement") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if is_statement_like(child.kind()) {
                blocks[0]
                    .stmts
                    .push((child.start_byte(), child.end_byte()));
            }
        }
    }

    Cfg {
        blocks,
        entry: BlockId(0),
        exit: BlockId(1),
    }
}

fn find_descendant<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_descendant(child, kind) {
            return Some(found);
        }
    }

    None
}

fn is_statement_like(kind: &str) -> bool {
    matches!(
        kind,
        "declaration"
            | "expression_statement"
            | "return_statement"
            | "if_statement"
            | "for_statement"
            | "while_statement"
            | "switch_statement"
            | "compound_statement"
    )
}
