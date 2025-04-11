use rustc_hir::{Expr, HirId, intravisit::{self, Visitor}};
use rustc_lint::LateContext;
use rustc_middle::hir::nested_filter;

/// Visitor to check if a HirId is used in code
pub(crate) struct UsageVisitor {
    hir_id: HirId,
    found: bool,
}

impl<'tcx> Visitor<'tcx> for UsageVisitor {
    type NestedFilter = nested_filter::OnlyBodies;

    fn nested_visit_map(&mut self) -> Self::NestedFilter {
        nested_filter::OnlyBodies
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        // Early return if we've already found what we're looking for
        if self.found {
            return;
        }
        
        // Check if this is the HirId we're looking for
        if expr.hir_id == self.hir_id {
            self.found = true;
            return;
        }
        
        // Let intravisit handle the recursion uniformly for all expression types
        intravisit::walk_expr(self, expr);
    }
}

/// Helper function that checks whether a given node contains the HirId usage
fn usage_found_in<'tcx, T>(
    hir_id: HirId,
    node: &'tcx T,
    visit_fn: impl FnOnce(&mut UsageVisitor, &'tcx T),
) -> bool {
    let mut visitor = UsageVisitor {
        hir_id,
        found: false,
    };
    visit_fn(&mut visitor, node);
    visitor.found
}

/// Checks if the given HirId is used later in the code after the specified span
pub(crate) fn is_used_later<'tcx>(
    cx: &LateContext<'tcx>,
    hir_id: HirId,
    call_span: rustc_span::Span,
) -> bool {
    let body_id = cx.tcx.hir().enclosing_body_owner(hir_id);
    let body = cx.tcx.hir().body(body_id).unwrap();
    
    // Check all statements in the body
    for stmt in body.value.stmts.iter() {
        if stmt.span > call_span && usage_found_in(hir_id, stmt, |v, s| v.visit_stmt(s)) {
            return true;
        }
    }
    
    // Also check the body's return expression if it exists
    if let Some(expr) = body.value.expr {
        if expr.span > call_span && usage_found_in(hir_id, expr, |v, e| v.visit_expr(e)) {
            return true;
        }
    }
    
    false
} 