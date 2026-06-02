use anyhow::Result;
use rusqlite::Connection;

use crate::diagnostics::{DiagnosticItem, SemaContext};
use crate::query::member_index::MemberHotIndex;

pub(crate) fn collect(
    conn: &Connection,
    engine_conn: Option<&Connection>,
    member_hot_index: Option<&MemberHotIndex>,
    engine_member_hot_index: Option<&MemberHotIndex>,
    sema_ctx: Option<&SemaContext>,
    known_names: &super::super::DiagnosticKnownNames,
    content: &str,
    file_path: Option<&str>,
    parsed_root: Option<tree_sitter::Node>,
) -> Result<Vec<DiagnosticItem>> {
    super::super::unknown_symbol_diagnostics(
        conn,
        engine_conn,
        member_hot_index,
        engine_member_hot_index,
        sema_ctx,
        known_names,
        content,
        file_path,
        parsed_root,
    )
}
