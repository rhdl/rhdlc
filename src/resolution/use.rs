use fxhash::FxHashSet as HashSet;
use rhdl::ast::{UseTree, UseTreeRename};

use super::{
    path::{r#mut::PathFinder, TracingContext},
    Branch, Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode,
};
use crate::error::*;

pub struct UseResolver<'a, 'ast> {
    pub resolution_graph: &'a mut ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<Diagnostic>,
    pub resolved_uses: &'a mut HashSet<ResolutionIndex>,
}

impl<'a, 'ast> UseResolver<'a, 'ast> {
    pub fn resolve_use(&mut self, dest: ResolutionIndex) {
        match &self.resolution_graph.inner[dest] {
            ResolutionNode::Branch {
                branch: Branch::Use(_),
                ..
            } => {}
            _ => return,
        }
        self.trace_use_entry_reenterable(&mut TracingContext::new(
            self.resolution_graph,
            dest,
            None,
        ));
    }

    pub fn trace_use_entry_reenterable(&mut self, ctx: &mut TracingContext<'ast>) {
        let tree = match &self.resolution_graph.inner[ctx.dest] {
            ResolutionNode::Branch {
                branch: Branch::Use(item_use),
                ..
            } => &item_use.tree,
            _ => return,
        };
        if self.resolved_uses.contains(&ctx.dest) {
            return;
        }
        self.resolved_uses.insert(ctx.dest);
        let scope = if ctx.leading_sep.is_some() {
            // just give any old dummy node because it'll have to be ignored in path/name finding
            0
        } else {
            let mut scope = ctx.dest;
            while !self.resolution_graph.inner[scope].is_valid_use_path_segment() {
                scope = self.resolution_graph.inner[scope].parent().unwrap();
            }
            scope
        };
        self.trace_use(ctx, scope, tree, false);
    }

    /// Trace usages
    fn trace_use(
        &mut self,
        ctx: &mut TracingContext<'ast>,
        scope: ResolutionIndex,
        tree: &'ast UseTree,
        in_group: bool,
    ) {
        use rhdl::ast::UseTree::*;
        let is_entry = ctx.previous_idents.is_empty();
        match tree {
            Path(path_tree) => {
                if ctx.previous_idents.is_empty() {
                    ctx.leading_sep = path_tree.path.leading_sep.as_ref();
                } else if !ctx.previous_idents.is_empty() && path_tree.path.leading_sep.is_some() {
                    self.errors
                        .push(global_path_in_prefixed_use_group(ctx.file, &path_tree.path));
                    return;
                }
                let mut path_finder = PathFinder {
                    resolution_graph: self.resolution_graph,
                    errors: self.errors,
                    resolved_uses: self.resolved_uses,
                    visited_glob_scopes: Default::default(),
                };
                let found_children = match path_finder.find_at_path(scope, &path_tree.path) {
                    Ok(v) => v,
                    Err(err) => {
                        self.errors.push(err);
                        return;
                    }
                };
                if found_children.len() > 1 {
                    self.errors.push(disambiguation_needed(
                        ctx.file,
                        path_tree.path.segments.last().unwrap(),
                        AmbiguitySource::Item(
                            if is_entry
                                && ctx.leading_sep.is_some()
                                && path_tree.path.segments.len() == 1
                            {
                                ItemHint::ExternalNamedScope
                            } else if is_entry && path_tree.path.segments.len() == 1 {
                                ItemHint::InternalNamedChildOrExternalNamedScope
                            } else {
                                ItemHint::InternalNamedChildScope
                            },
                        ),
                    ));
                }
                let new_scope = *found_children.first().unwrap();
                ctx.previous_idents.extend(path_tree.path.segments.iter());
                self.trace_use(ctx, new_scope, &path_tree.tree, false);
                ctx.previous_idents
                    .truncate(ctx.previous_idents.len() - path_tree.path.segments.len());
            }
            Name(ident) | Rename(UseTreeRename { name: ident, .. }) => {
                let found_children: Vec<ResolutionIndex> = if ident == "self" {
                    let cause = if !in_group {
                        Some(SelfUsageErrorCause::NotInGroup)
                    } else if ctx.previous_idents.is_empty() {
                        Some(SelfUsageErrorCause::InGroupAtRoot)
                    } else {
                        None
                    };
                    if let Some(cause) = cause {
                        self.errors.push(self_usage(ctx.file, &ident, cause));
                        return;
                    }
                    vec![scope]
                } else {
                    let mut path_finder = PathFinder {
                        resolution_graph: self.resolution_graph,
                        errors: self.errors,
                        resolved_uses: self.resolved_uses,
                        visited_glob_scopes: Default::default(),
                    };
                    match path_finder.find_children(ctx, scope, ident, false) {
                        Ok(v) => v,
                        Err(err) => {
                            self.errors.push(err);
                            return;
                        }
                    }
                };
                match tree {
                    Name(name) => {
                        let idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
                            leaf: Leaf::UseName(name, found_children),
                            parent: ctx.dest,
                        });
                        self.resolution_graph.add_child(ctx.dest, idx);
                    }
                    Rename(rename) => {
                        let idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
                            leaf: Leaf::UseRename(rename, found_children),
                            parent: ctx.dest,
                        });
                        self.resolution_graph.add_child(ctx.dest, idx);
                    }
                    _ => {}
                }
            }
            Glob(glob) => {
                if is_entry
                    || ctx.leading_sep.is_some()
                    || ctx
                        .previous_idents
                        .last()
                        .map(|ident| *ident == "self")
                        .unwrap_or_default()
                {
                    self.errors.push(glob_at_entry(
                        ctx.file,
                        glob,
                        ctx.leading_sep,
                        ctx.previous_idents.last().copied(),
                    ));
                    return;
                }
                let glob_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
                    leaf: Leaf::UseGlob(glob, scope),
                    parent: ctx.dest,
                });
                self.resolution_graph.add_child(ctx.dest, glob_idx);
            }
            Group(group) => group
                .trees
                .iter()
                .for_each(|tree| self.trace_use(ctx, scope, tree, true)),
        }
    }
}
