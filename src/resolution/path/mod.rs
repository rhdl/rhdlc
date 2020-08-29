use fnv::FnvHashSet as HashSet;
use std::rc::Rc;

use petgraph::{graph::NodeIndex, Direction};
use syn::{Ident, Path};

use super::{r#use::UseType, Node, ScopeGraph};
use crate::error::*;
use crate::find_file::File;

pub mod r#mut;

pub struct TracingContext<'ast> {
    pub file: Rc<File>,
    pub root: NodeIndex,
    pub dest: NodeIndex,
    pub previous_idents: Vec<&'ast Ident>,
    pub has_leading_colon: bool,
}

impl<'ast> TracingContext<'ast> {
    pub fn new(scope_graph: &ScopeGraph, dest: NodeIndex, has_leading_colon: bool) -> Self {
        let mut root = dest;
        while let Some(parent) = scope_graph
            .neighbors_directed(root, Direction::Incoming)
            .next()
        {
            root = parent;
        }
        Self {
            file: Node::file(scope_graph, dest).clone(),
            dest,
            root,
            previous_idents: vec![],
            has_leading_colon,
        }
    }
}

pub struct PathFinder<'a, 'ast> {
    pub scope_graph: &'a ScopeGraph<'ast>,
    pub visited_glob_scopes: HashSet<NodeIndex>,
}

impl<'a, 'ast> PathFinder<'a, 'ast> {
    pub fn find_at_path(
        &mut self,
        dest: NodeIndex,
        path: &'a Path,
    ) -> Result<Vec<NodeIndex>, ResolutionError> {
        self.visited_glob_scopes.clear();
        let mut ctx = TracingContext::new(self.scope_graph, dest, path.leading_colon.is_some());
        let mut dest_scope = dest;
        while self.scope_graph[dest_scope].is_nameless_scope() {
            dest_scope = self
                .scope_graph
                .neighbors_directed(dest_scope, Direction::Incoming)
                .next()
                .unwrap();
        }
        let mut scopes = vec![dest_scope];
        for (i, segment) in path.segments.iter().enumerate() {
            let ident = &segment.ident;
            let mut results: Vec<Result<Vec<NodeIndex>, ResolutionError>> = scopes
                .iter()
                .map(|scope| self.find_children(&ctx, *scope, ident, i + 1 != path.segments.len()))
                .collect();
            if results.iter().all(|res| res.is_err()) {
                return results.drain(..).next().unwrap();
            }
            scopes = results
                .drain(..)
                .filter_map(|res| res.ok())
                .flatten()
                .collect();
            ctx.previous_idents.push(&segment.ident);
        }
        Ok(scopes)
    }

    /// Ok is guaranteed to have >= 1 node, else an unresolved error will be returned
    fn find_children(
        &mut self,
        ctx: &TracingContext,
        scope: NodeIndex,
        ident: &Ident,
        paths_only: bool,
    ) -> Result<Vec<NodeIndex>, ResolutionError> {
        let is_entry = ctx.previous_idents.is_empty();
        let hint = if paths_only && is_entry {
            ItemHint::InternalNamedChildOrExternalNamedScope
        } else if paths_only {
            ItemHint::InternalNamedChildScope
        } else {
            ItemHint::Item
        };
        let local = if !is_entry || !ctx.has_leading_colon {
            self.scope_graph
                .neighbors(scope)
                .filter(|child| *child != ctx.dest)
                .map(|child| self.matches(ctx, &child, ident, paths_only, false))
                .flatten()
                .collect()
        } else {
            vec![]
        };
        let global = if is_entry {
            self.scope_graph
                .externals(Direction::Incoming)
                .filter(|child| *child != ctx.root)
                .map(|child| self.matches(ctx, &child, ident, paths_only, false))
                .flatten()
                .collect()
        } else {
            vec![]
        };
        let visible_local: Vec<NodeIndex> = local
            .iter()
            .filter(|i| super::r#pub::is_target_visible(self.scope_graph, ctx.dest, **i))
            .cloned()
            .collect();
        if global.is_empty() && !local.is_empty() && visible_local.is_empty() {
            return Err(ItemVisibilityError {
                file: ctx.file.clone(),
                ident: ident.clone(),
                hint,
            }
            .into());
        }
        let visible_global: Vec<NodeIndex> = global
            .iter()
            .filter(|i| super::r#pub::is_target_visible(self.scope_graph, ctx.dest, **i))
            .cloned()
            .collect();
        if local.is_empty() && !global.is_empty() && visible_global.is_empty() {
            return Err(ItemVisibilityError {
                file: ctx.file.clone(),
                ident: ident.clone(),
                hint,
            }
            .into());
        }
        let local = visible_local;
        let global = visible_global;
        match (global.is_empty(), local.is_empty()) {
            (false, false) => Err(DisambiguationError {
                file: ctx.file.clone(),
                ident: ident.clone(),
                src: AmbiguitySource::Item(hint),
            }
            .into()),
            (true, false) => Ok(local),
            (false, true) => Ok(global),
            (true, true) => {
                if !(ctx.has_leading_colon && is_entry) {
                    let local_from_globs: Vec<NodeIndex> = self
                        .scope_graph
                        .neighbors(scope)
                        .filter(|child| *child != ctx.dest)
                        .map(|child| self.matches(ctx, &child, &ident, paths_only, true))
                        .flatten()
                        .collect();
                    let visible_local_from_globs: Vec<NodeIndex> = local_from_globs
                        .iter()
                        .filter(|i| {
                            super::r#pub::is_target_visible(self.scope_graph, ctx.dest, **i)
                        })
                        .cloned()
                        .collect();
                    if !local_from_globs.is_empty() && visible_local_from_globs.is_empty() {
                        Err(ItemVisibilityError {
                            file: ctx.file.clone(),
                            ident: ident.clone(),
                            hint,
                        }
                        .into())
                    } else if visible_local_from_globs.is_empty() {
                        Err(UnresolvedItemError {
                            file: ctx.file.clone(),
                            previous_ident: ctx.previous_idents.last().cloned().cloned(),
                            unresolved_ident: ident.clone(),
                            hint,
                        }
                        .into())
                    } else {
                        Ok(visible_local_from_globs)
                    }
                } else {
                    Err(UnresolvedItemError {
                        file: ctx.file.clone(),
                        previous_ident: ctx.previous_idents.last().cloned().cloned(),
                        unresolved_ident: ident.clone(),
                        hint: ItemHint::ExternalNamedScope,
                    }
                    .into())
                }
            }
        }
    }

    fn matches(
        &mut self,
        ctx: &TracingContext,
        node: &NodeIndex,
        ident_to_look_for: &Ident,
        paths_only: bool,
        glob_only: bool,
    ) -> Vec<NodeIndex> {
        let imports = match &self.scope_graph[*node] {
            Node::Use { imports, .. } => imports,
            _ => {
                return if glob_only {
                    vec![]
                } else if self.matches_exact(node, ident_to_look_for, paths_only) {
                    vec![*node]
                } else {
                    vec![]
                };
            }
        };

        if !super::r#pub::is_target_visible(self.scope_graph, ctx.dest, *node) {
            return vec![];
        }
        imports
            .values()
            .map(|use_types| {
                use_types
                    .iter()
                    .map(|use_type| match use_type {
                        UseType::Name { name, indices } => {
                            if glob_only {
                                vec![]
                            } else if name.ident == *ident_to_look_for {
                                indices
                                    .iter()
                                    .map(|i| {
                                        self.matches(
                                            ctx,
                                            i,
                                            ident_to_look_for,
                                            paths_only,
                                            glob_only,
                                        )
                                    })
                                    .flatten()
                                    .collect::<Vec<NodeIndex>>()
                            } else {
                                vec![]
                            }
                        }
                        UseType::Rename { rename, indices } => {
                            // match on new name, recurse on original name
                            if glob_only {
                                vec![]
                            } else if rename.rename == *ident_to_look_for {
                                indices
                                    .iter()
                                    .map(|i| {
                                        self.matches(
                                            ctx,
                                            i,
                                            &rename.ident,
                                            paths_only,
                                            glob_only,
                                        )
                                    })
                                    .flatten()
                                    .collect::<Vec<NodeIndex>>()
                            } else {
                                vec![]
                            }
                        }
                        UseType::Glob { scope } => {
                            if glob_only {
                                if self.visited_glob_scopes.contains(node) {
                                    return vec![];
                                } else {
                                    self.visited_glob_scopes.insert(*node);
                                }
                                let neighbors = self
                                    .scope_graph
                                    .neighbors(*scope)
                                    .collect::<Vec<NodeIndex>>();
                                neighbors
                                    .iter()
                                    .map(|child| {
                                        let nonglob_matches = self.matches(
                                            ctx,
                                            &child,
                                            ident_to_look_for,
                                            paths_only,
                                            false,
                                        );
                                        if nonglob_matches.is_empty() {
                                            self.matches(
                                                ctx,
                                                &child,
                                                ident_to_look_for,
                                                paths_only,
                                                true,
                                            )
                                        } else {
                                            nonglob_matches
                                        }
                                    })
                                    .flatten()
                                    .collect()
                            } else {
                                vec![]
                            }
                        }
                    })
                    .flatten()
                    .collect::<Vec<NodeIndex>>()
            })
            .flatten()
            .collect()
    }

    fn matches_exact(&self, node: &NodeIndex, ident_to_look_for: &Ident, paths_only: bool) -> bool {
        let is_path = match &self.scope_graph[*node] {
            Node::Mod { .. } | Node::Root { .. } => true,
            // TODO: look for associated consts, but NOT for uses
            _ => false,
        };
        if is_path || !paths_only {
            let names = self.scope_graph[*node].names();
            names.len() == 1 && names.first().unwrap().ident() == ident_to_look_for
        } else {
            false
        }
    }
}
