use std::collections::HashSet;
use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{visit::Visit, Path, UseGlob, UseName, UsePath, UseRename, UseTree};

use super::{r#use::{UseType, UseResolver}, Node, ScopeGraph};
use crate::error::*;
use crate::find_file::File;

pub struct TracingContext {
    pub file: Rc<File>,
    pub root: NodeIndex,
    pub dest: NodeIndex,
    pub previous_idents: Vec<syn::Ident>,
    pub has_leading_colon: bool,
}

impl TracingContext {
    pub fn new(scope_graph: &ScopeGraph, dest: NodeIndex, file: &Rc<File>, has_leading_colon: bool) -> Self {
        let mut root = dest;
        while let Some(parent) = scope_graph
            .neighbors_directed(root, Direction::Incoming)
            .next()
        {
            root = parent;
        }
        Self {
            file: file.clone(),
            dest,
            root,
            previous_idents: vec![],
            has_leading_colon,
        }
    }
}

pub struct PathFinder<'a, 'ast> {
    pub scope_graph: &'a mut ScopeGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
    pub reentrancy: &'a mut HashSet<NodeIndex>,
}

impl<'a, 'ast> Into<UseResolver<'a, 'ast>> for &'a mut PathFinder<'a, 'ast> {
    fn into(self) -> UseResolver<'a, 'ast> {
        UseResolver {
            scope_graph: self.scope_graph,
            errors: self.errors,
            reentrancy: self.reentrancy,
        }
    }
}


impl<'a, 'ast> PathFinder<'a, 'ast> {
    pub fn find_children(
        &mut self,
        ctx: &TracingContext,
        scope: NodeIndex,
        ident: &syn::Ident,
        original_name_string: &str,
        paths_only: bool,
    ) -> Result<Vec<NodeIndex>, ResolutionError> {
        let is_entry = ctx.previous_idents.is_empty();
        let local = if !is_entry || (is_entry && !ctx.has_leading_colon) {
            let local_nodes: Vec<NodeIndex> = self
                .scope_graph
                .neighbors(scope)
                .filter(|child| *child != ctx.dest)
                .collect();
            local_nodes
                .iter()
                .map(|child| self.matches(&child, original_name_string, paths_only, false))
                .flatten()
                .collect()
        } else {
            vec![]
        };
        let global = if is_entry {
            let global_nodes: Vec<NodeIndex> = self
                .scope_graph
                .externals(Direction::Incoming)
                .filter(|child| *child != ctx.root)
                .collect();
            global_nodes
                .iter()
                .map(|child| self.matches(&child, original_name_string, paths_only, false))
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
                name_file: ctx.file.clone(),
                name_ident: ident.clone(),
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
                name_file: ctx.file.clone(),
                name_ident: ident.clone(),
            }
            .into());
        }
        let local = visible_local;
        let global = visible_global;
        match (global.is_empty(), local.is_empty()) {
            (false, false) => Err(DisambiguationError {
                file: ctx.file.clone(),
                ident: ident.clone(),
                this: AmbiguitySource::Name,
                other: AmbiguitySource::Name,
            }
            .into()),
            (true, false) => Ok(local),
            (false, true) => Ok(global),
            (true, true) => {
                if !(ctx.has_leading_colon && is_entry) {
                    let local_nodes: Vec<NodeIndex> = self
                        .scope_graph
                        .neighbors(scope)
                        .filter(|child| *child != ctx.dest)
                        .collect();
                    let local_from_globs: Vec<NodeIndex> = local_nodes
                        .iter()
                        .map(|child| self.matches(&child, &original_name_string, false, true))
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
                        return Err(ItemVisibilityError {
                            name_file: ctx.file.clone(),
                            name_ident: ident.clone(),
                        }
                        .into());
                    }
                    Ok(visible_local_from_globs)
                } else {
                    Ok(vec![])
                }
            }
        }
    }

    fn matches(
        &mut self,
        node: &NodeIndex,
        name_to_look_for: &str,
        paths_only: bool,
        glob_only: bool,
    ) -> Vec<NodeIndex> {
        if let Some(exact_match) = self.matches_exact(node, name_to_look_for, paths_only) {
            return vec![exact_match];
        }

        let rebuilt_ctx_opt = match &self.scope_graph[*node] {
            Node::Use {
                item_use,
                imports,
                file,
                ..
            } => {
                if self.reentrancy.contains(node) {
                    None
                } else if !imports.is_empty() {
                    None
                } else if {
                    let mut checker = UseMightMatchChecker {
                        name_to_look_for,
                        needed: false,
                    };
                    checker.visit_item_use(item_use);
                    checker.needed
                } {
                    Some((
                        TracingContext::new(self.scope_graph, *node, file, item_use.leading_colon.is_some()),
                        &item_use.tree,
                    ))
                } else {
                    // claim: if might not match returned empty, it definitely will not match
                    return vec![];
                }
            }
            _ => return vec![],
        };
        if let Some((mut rebuilt_ctx, tree)) = rebuilt_ctx_opt {
            let mut use_resolver = UseResolver {
                scope_graph: self.scope_graph,
                errors: self.errors,
                reentrancy: self.reentrancy,
            };
            use_resolver.trace_use_entry_reenterable(&mut rebuilt_ctx, tree);
        }
        let imports = match &self.scope_graph[*node] {
            Node::Use { imports, .. } => imports.clone(),
            bad => panic!("this should not be reached: {:?}", bad),
        };
        // TODO: try to avoid recursing into private use matches
        imports
            .values()
            .map(|use_types| {
                use_types
                    .iter()
                    .map(|use_type| match use_type {
                        UseType::Name { name, indices } => {
                            if name.ident == name_to_look_for {
                                indices
                                    .iter()
                                    .map(|i| {
                                        self.matches(i, name_to_look_for, paths_only, glob_only)
                                    })
                                    .flatten()
                                    .collect::<Vec<NodeIndex>>()
                            } else {
                                vec![]
                            }
                        }
                        UseType::Rename { rename, indices } => {
                            // match on new name, recurse on original name
                            if rename.rename == name_to_look_for {
                                indices
                                    .iter()
                                    .map(|i| {
                                        self.matches(
                                            i,
                                            &rename.ident.to_string(),
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
                                let neighbors = self
                                    .scope_graph
                                    .neighbors(*scope)
                                    .collect::<Vec<NodeIndex>>();
                                neighbors
                                    .iter()
                                    .map(|child| {
                                        let nonglob_matches = self.matches(
                                            &child,
                                            name_to_look_for,
                                            paths_only,
                                            false,
                                        );
                                        if nonglob_matches.is_empty() {
                                            self.matches(&child, name_to_look_for, paths_only, true)
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

    fn matches_exact(
        &self,
        node: &NodeIndex,
        name_to_look_for: &str,
        paths_only: bool,
    ) -> Option<NodeIndex> {
        let exact_match = match &self.scope_graph[*node] {
            Node::Var { ident, .. } | Node::Macro { ident, .. } | Node::Type { ident, .. } => {
                !paths_only && *ident == name_to_look_for
            }
            Node::Fn { item_fn, .. } => !paths_only && item_fn.sig.ident == name_to_look_for,
            Node::Root { name, .. } => name == name_to_look_for,
            Node::Mod { item_mod, .. } => item_mod.ident == name_to_look_for,
            Node::Use { .. } | Node::Impl { .. } | Node::MacroUsage { .. } => false,
        };
        if exact_match {
            Some(*node)
        } else {
            None
        }
    }
}

struct UseMightMatchChecker<'a> {
    name_to_look_for: &'a str,
    needed: bool,
}

impl<'a, 'ast> Visit<'ast> for UseMightMatchChecker<'a> {
    fn visit_use_path(&mut self, path: &'ast UsePath) {
        // this replaces the default trait impl, need to call use_tree for use name visitation
        self.visit_use_tree(path.tree.as_ref());
        self.needed |= path.ident == self.name_to_look_for
            && match path.tree.as_ref() {
                UseTree::Group(group) => group.items.iter().any(|tree| match tree {
                    UseTree::Rename(rename) => rename.ident == "self",
                    UseTree::Name(name) => name.ident == "self",
                    _ => false,
                }),
                _ => false,
            }
    }

    fn visit_use_name(&mut self, name: &'ast UseName) {
        self.needed |= name.ident == self.name_to_look_for
    }

    fn visit_use_rename(&mut self, rename: &'ast UseRename) {
        self.needed |= rename.rename == self.name_to_look_for
    }

    fn visit_use_glob(&mut self, _: &'ast UseGlob) {
        self.needed |= true;
    }
}
