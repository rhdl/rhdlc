use crate::resolution::r#pub::VisibilitySolver;
use rhdl::ast::Ident;
use rhdl::ast::PathSep;

use crate::error::*;
use crate::resolution::{FileId, ResolutionGraph, ResolutionIndex};

pub mod simple;
pub mod r#type;

pub struct TracingContext<'ast> {
    pub file: FileId,
    pub root: ResolutionIndex,
    pub dest: ResolutionIndex,
    pub previous_idents: Vec<&'ast Ident>,
    pub leading_sep: Option<&'ast PathSep>,
}

impl<'ast> TracingContext<'ast> {
    pub fn new(
        resolution_graph: &ResolutionGraph,
        dest: ResolutionIndex,
        leading_sep: Option<&'ast PathSep>,
    ) -> Self {
        let mut root = dest;
        while let Some(parent) = resolution_graph[root].parent() {
            root = parent;
        }
        Self {
            file: resolution_graph.file(dest),
            dest,
            root,
            previous_idents: vec![],
            leading_sep,
        }
    }
}

fn handle_special_ident<'ast>(
    resolution_graph: &ResolutionGraph,
    vis_solver: &VisibilitySolver<'ast>,
    ctx: &TracingContext<'ast>,
    scope: ResolutionIndex,
    ident: &Ident,
) -> Result<Option<ResolutionIndex>, Diagnostic> {
    let is_entry = ctx.previous_idents.is_empty();

    let is_special_ident =
        ident == "super" || ident == "crate" || ident == "self" || ident == "Self";
    let is_chained_supers = ctx
        .previous_idents
        .last()
        .map(|ident| *ident == "super")
        .unwrap_or(true)
        && ident == "super";
    if !is_entry && is_special_ident && !is_chained_supers {
        Err(special_ident_not_at_start_of_path(ctx.file, &ident))
    } else if ctx.leading_sep.is_some() && is_special_ident {
        Err(global_path_cannot_have_special_ident(
            ctx.file,
            &ident,
            ctx.leading_sep.unwrap(),
        ))
    } else if ident == "self" {
        Ok(Some(scope))
    } else if ident == "super" {
        let mut use_grandparent = resolution_graph[scope].parent();
        while use_grandparent
            .map(|i| !resolution_graph[i].is_valid_use_path_segment())
            .unwrap_or_default()
        {
            use_grandparent = resolution_graph[use_grandparent.unwrap()].parent();
        }
        if let Some(use_grandparent) = use_grandparent {
            Ok(Some(use_grandparent))
        } else {
            Err(too_many_supers(ctx.file, &ident))
        }
    } else if ident == "crate" {
        let mut root = scope;
        while let Some(next_parent) = resolution_graph[root].parent() {
            root = next_parent;
        }
        Ok(Some(root))
    } else {
        Ok(None)
    }
}

fn find_children_from_local_and_global<'ast>(
    resolution_graph: &ResolutionGraph,
    vis_solver: &VisibilitySolver<'ast>,
    ctx: &TracingContext<'ast>,
    ident: &Ident,
    paths_only: bool,
    mut local: Vec<ResolutionIndex>,
    mut global: Vec<ResolutionIndex>,
) -> Result<Option<Vec<ResolutionIndex>>, Diagnostic> {
    let is_entry = ctx.previous_idents.is_empty();
    let hint = if paths_only && is_entry {
        ItemHint::InternalNamedChildOrExternalNamedScope
    } else if paths_only {
        ItemHint::InternalNamedChildScope
    } else {
        ItemHint::Item
    };

    // TODO: once drain_filter is stabilized, dedupe this call
    // https://github.com/rust-lang/rust/issues/43244
    let local_not_visible = local
        .iter()
        .copied()
        .filter(|i| !vis_solver.is_target_visible(ctx.dest, *i))
        .collect::<Vec<ResolutionIndex>>();
    local.retain(|i| vis_solver.is_target_visible(ctx.dest, *i));
    if global.is_empty() && !local_not_visible.is_empty() && local.is_empty() {
        let declaration_idx = *local_not_visible.first().unwrap();
        return Err(item_visibility(
            ctx.file,
            &ident,
            resolution_graph.file(declaration_idx),
            resolution_graph[declaration_idx].name().unwrap(),
            hint,
        ));
    }
    let global_not_visible = global
        .iter()
        .copied()
        .filter(|i| !vis_solver.is_target_visible(ctx.dest, *i))
        .collect::<Vec<ResolutionIndex>>();
    global.retain(|i| vis_solver.is_target_visible(ctx.dest, *i));
    if local.is_empty() && !global_not_visible.is_empty() && global.is_empty() {
        let declaration_idx = *global_not_visible.first().unwrap();
        return Err(item_visibility(
            ctx.file,
            &ident,
            resolution_graph.file(declaration_idx),
            resolution_graph[declaration_idx].name().unwrap(),
            hint,
        ));
    }
    match (global.is_empty(), local.is_empty()) {
        (false, false) => Err(disambiguation_needed(
            ctx.file,
            &ident,
            AmbiguitySource::Item(hint),
        )),
        (true, false) => Ok(Some(local)),
        (false, true) => Ok(Some(global)),
        (true, true) => {
            if !(ctx.leading_sep.is_some() && is_entry) {
                Ok(None)
            } else {
                Err(unresolved_item(
                    ctx.file,
                    ctx.previous_idents.last().copied(),
                    &ident,
                    ItemHint::ExternalNamedScope,
                    vec![],
                ))
            }
        }
    }
}

fn find_children_from_globs<'ast>(
    resolution_graph: &ResolutionGraph,
    vis_solver: &VisibilitySolver<'ast>,
    ctx: &TracingContext<'ast>,
    ident: &Ident,
    paths_only: bool,
    mut local_from_globs: Vec<ResolutionIndex>,
) -> Result<Vec<ResolutionIndex>, Diagnostic> {
    let is_entry = ctx.previous_idents.is_empty();
    let hint = if paths_only && is_entry {
        ItemHint::InternalNamedChildOrExternalNamedScope
    } else if paths_only {
        ItemHint::InternalNamedChildScope
    } else {
        ItemHint::Item
    };
    let local_from_globs_not_visible = local_from_globs
        .iter()
        .copied()
        .filter(|i| !vis_solver.is_target_visible(ctx.dest, *i))
        .collect::<Vec<ResolutionIndex>>();
    local_from_globs.retain(|i| vis_solver.is_target_visible(ctx.dest, *i));
    if !local_from_globs_not_visible.is_empty() && local_from_globs.is_empty() {
        let declaration_idx = *local_from_globs_not_visible.first().unwrap();
        Err(item_visibility(
            ctx.file,
            &ident,
            resolution_graph.file(declaration_idx),
            resolution_graph[declaration_idx].name().unwrap(),
            hint,
        ))
    } else if local_from_globs.is_empty() {
        Err(unresolved_item(
            ctx.file,
            ctx.previous_idents.last().copied(),
            &ident,
            hint,
            vec![],
        ))
    } else {
        Ok(local_from_globs)
    }
}
