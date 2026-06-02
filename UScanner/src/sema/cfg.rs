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
    let mut builder = CfgBuilder::new();
    if let Some(body) = find_descendant(function_node, "compound_statement") {
        let open = builder.build_stmt(body, vec![builder.entry], builder.exit, None, None);
        builder.connect_many(open, builder.exit);
    } else {
        builder.connect(builder.entry, builder.exit);
    }
    builder.finish()
}

struct CfgBuilder {
    blocks: Vec<BasicBlock>,
    entry: BlockId,
    exit: BlockId,
}

impl CfgBuilder {
    fn new() -> Self {
        let blocks = vec![
            BasicBlock {
                stmts: Vec::new(),
                succ: Vec::new(),
            },
            BasicBlock {
                stmts: Vec::new(),
                succ: Vec::new(),
            },
        ];
        Self {
            blocks,
            entry: BlockId(0),
            exit: BlockId(1),
        }
    }

    fn finish(self) -> Cfg {
        Cfg {
            blocks: self.blocks,
            entry: self.entry,
            exit: self.exit,
        }
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock {
            stmts: Vec::new(),
            succ: Vec::new(),
        });
        id
    }

    fn block_with_stmt(&mut self, node: Node) -> BlockId {
        let id = self.new_block();
        if let Some(block) = self.blocks.get_mut(id.0 as usize) {
            block.stmts.push((node.start_byte(), node.end_byte()));
        }
        id
    }

    fn connect(&mut self, from: BlockId, to: BlockId) {
        let Some(block) = self.blocks.get_mut(from.0 as usize) else {
            return;
        };
        if !block.succ.contains(&to) {
            block.succ.push(to);
        }
    }

    fn connect_many(&mut self, from_blocks: Vec<BlockId>, to: BlockId) {
        for from in from_blocks {
            self.connect(from, to);
        }
    }

    fn build_stmt(
        &mut self,
        node: Node,
        preds: Vec<BlockId>,
        exit: BlockId,
        break_target: Option<BlockId>,
        continue_target: Option<BlockId>,
    ) -> Vec<BlockId> {
        match node.kind() {
            "compound_statement" => {
                let mut open = preds;
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if !is_statement_like(child.kind()) {
                        continue;
                    }
                    open = self.build_stmt(child, open, exit, break_target, continue_target);
                    if open.is_empty() {
                        break;
                    }
                }
                open
            }
            "else_clause" => {
                let mut cursor = node.walk();
                let Some(child) = node.named_children(&mut cursor).next() else {
                    return preds;
                };
                self.build_stmt(child, preds, exit, break_target, continue_target)
            }
            "return_statement" | "co_return_statement" | "throw_statement" => {
                let block = self.block_with_stmt(node);
                self.connect_many(preds, block);
                self.connect(block, exit);
                Vec::new()
            }
            "break_statement" => {
                let block = self.block_with_stmt(node);
                self.connect_many(preds, block);
                self.connect(block, break_target.unwrap_or(exit));
                Vec::new()
            }
            "continue_statement" => {
                let block = self.block_with_stmt(node);
                self.connect_many(preds, block);
                self.connect(block, continue_target.unwrap_or(exit));
                Vec::new()
            }
            "if_statement" => self.build_if(node, preds, exit, break_target, continue_target),
            "while_statement" | "for_statement" | "do_statement" => {
                self.build_loop(node, preds, exit, break_target)
            }
            "switch_statement" => self.build_switch(node, preds, exit),
            _ => {
                let block = self.block_with_stmt(node);
                self.connect_many(preds, block);
                vec![block]
            }
        }
    }

    fn build_if(
        &mut self,
        node: Node,
        preds: Vec<BlockId>,
        exit: BlockId,
        break_target: Option<BlockId>,
        continue_target: Option<BlockId>,
    ) -> Vec<BlockId> {
        let cond = self.block_with_stmt(node);
        self.connect_many(preds, cond);

        let Some(consequence) = node.child_by_field_name("consequence") else {
            return vec![cond];
        };

        let mut open = self.build_stmt(
            consequence,
            vec![cond],
            exit,
            break_target,
            continue_target,
        );

        if let Some(alternative) = node.child_by_field_name("alternative") {
            open.extend(self.build_stmt(
                alternative,
                vec![cond],
                exit,
                break_target,
                continue_target,
            ));
        } else {
            open.push(cond);
        }

        open
    }

    fn build_loop(
        &mut self,
        node: Node,
        preds: Vec<BlockId>,
        exit: BlockId,
        outer_break_target: Option<BlockId>,
    ) -> Vec<BlockId> {
        let cond = self.block_with_stmt(node);
        let after = self.new_block();
        self.connect_many(preds, cond);
        self.connect(cond, after);

        let body = loop_body_node(node);
        if let Some(body) = body {
            let body_open = self.build_stmt(body, vec![cond], exit, Some(after), Some(cond));
            self.connect_many(body_open, cond);
        }

        let _ = outer_break_target;
        vec![after]
    }

    fn build_switch(&mut self, node: Node, preds: Vec<BlockId>, exit: BlockId) -> Vec<BlockId> {
        let dispatch = self.block_with_stmt(node);
        let after = self.new_block();
        self.connect_many(preds, dispatch);
        self.connect(dispatch, after);

        let mut open = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "compound_statement" {
                let mut inner = child.walk();
                for case in child.named_children(&mut inner) {
                    if matches!(case.kind(), "case_statement" | "default_statement") {
                        open.extend(self.build_stmt(case, vec![dispatch], exit, Some(after), None));
                    }
                }
            }
        }
        self.connect_many(open, after);
        vec![after]
    }
}

fn loop_body_node(node: Node) -> Option<Node> {
    node.child_by_field_name("body").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "compound_statement" | "expression_statement"))
    })
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
            | "co_return_statement"
            | "throw_statement"
            | "break_statement"
            | "continue_statement"
            | "if_statement"
            | "for_statement"
            | "while_statement"
            | "do_statement"
            | "switch_statement"
            | "case_statement"
            | "default_statement"
            | "compound_statement"
            | "else_clause"
    )
}

#[cfg(test)]
mod tests {
    use super::build_cfg;
    use tree_sitter::Parser;

    fn parse_root(content: &str) -> tree_sitter::Node<'_> {
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_unreal_cpp::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(content, None).unwrap();
        Box::leak(Box::new(tree)).root_node()
    }

    #[test]
    fn cfg_creates_branch_edges_for_if_statement() {
        let root = parse_root("void Run(){ if (true) { return; } else { int32 Value = 0; } }");
        let function = root.named_child(0).unwrap();
        let cfg = build_cfg(function);
        assert!(cfg.blocks.iter().any(|block| block.succ.len() >= 2));
    }

    #[test]
    fn cfg_creates_loop_back_edge() {
        let root = parse_root("void Run(){ while (true) { int32 Value = 0; } }");
        let function = root.named_child(0).unwrap();
        let cfg = build_cfg(function);
        let has_back_edge = cfg
            .blocks
            .iter()
            .enumerate()
            .any(|(index, block)| block.succ.iter().any(|succ| succ.0 as usize <= index));
        assert!(has_back_edge);
    }
}
