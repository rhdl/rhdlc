use fnv::FnvHashSet as HashSet;

use log::error;
use syn::{UseName, UseRename, UseTree};

use super::{
    path::{r#mut::PathFinder, TracingContext},
    Branch, ResolutionError, ResolutionGraph, ResolutionIndex, ResolutionNode,
};
use crate::error::{
    AmbiguitySource, DisambiguationError, GlobAtEntryError, GlobalPathCannotHaveSpecialIdentError,
    ItemHint, SelfUsageError, SelfUsageErrorCause, SpecialIdentNotAtStartOfPathError,
    TooManySupersError,
};

#[derive(Debug, Clone)]
pub enum UseType<'ast> {
    /// Pull a particular name into scope
    /// Could be ambiguous
    Name {
        name: &'ast UseName,
        indices: Vec<ResolutionIndex>,
    },
    /// Optionally include all items/mods from the scope
    Glob { scope: ResolutionIndex },
    /// Pull a particular name into scope, but give it a new name (so as to avoid any conflicts)
    /// Could be ambiguous
    Rename {
        rename: &'ast UseRename,
        indices: Vec<ResolutionIndex>,
    },
}

pub struct UseResolver<'a, 'ast> {
    pub resolution_graph: &'a mut ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
    pub resolved_uses: &'a mut HashSet<ResolutionIndex>,
}

impl<'a, 'ast> UseResolver<'a, 'ast> {
    pub fn resolve_use(&mut self, dest: ResolutionIndex) {
        let item_use = match &self.resolution_graph.inner[dest] {
            ResolutionNode::Branch {
                branch: Branch::Use(item_use),
                ..
            } => item_use,
            _ => return,
        };
        let has_leading_colon = item_use.leading_colon.is_some();
        self.trace_use_entry_reenterable(&mut TracingContext::new(
            self.resolution_graph,
            dest,
            has_leading_colon,
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
        let scope = if ctx.has_leading_colon {
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
        use syn::UseTree::*;
        let is_entry = ctx.previous_idents.is_empty();
        match tree {
            Path(path) => {
                let path_ident = path.ident.to_string();
                let new_scope = match path.ident == "self"
                    || path.ident == "super"
                    || path.ident == "crate"
                {
                    // Special keyword cases
                    true => {
                        let is_chained_supers = ctx
                            .previous_idents
                            .last()
                            .map(|ident| *ident == "super")
                            .unwrap_or(true)
                            && path_ident == "super";
                        if !is_entry && !is_chained_supers {
                            self.errors.push(
                                SpecialIdentNotAtStartOfPathError {
                                    file: ctx.file.clone(),
                                    path_ident: path.ident.clone(),
                                }
                                .into(),
                            );
                            return;
                        }
                        if ctx.has_leading_colon {
                            self.errors.push(
                                GlobalPathCannotHaveSpecialIdentError {
                                    file: ctx.file.clone(),
                                    path_ident: path.ident.clone(),
                                }
                                .into(),
                            );
                            return;
                        }
                        if path_ident == "self" {
                            scope
                        } else if path_ident == "super" {
                            let mut use_grandparent = self.resolution_graph.inner[scope].parent();
                            while use_grandparent
                                .map(|i| {
                                    !self.resolution_graph.inner[i].is_valid_use_path_segment()
                                })
                                .unwrap_or_default()
                            {
                                use_grandparent =
                                    self.resolution_graph.inner[use_grandparent.unwrap()].parent();
                            }
                            if let Some(use_grandparent) = use_grandparent {
                                use_grandparent
                            } else {
                                self.errors.push(
                                    TooManySupersError {
                                        file: ctx.file.clone(),
                                        ident: path.ident.clone(),
                                    }
                                    .into(),
                                );
                                return;
                            }
                        } else if path_ident == "crate" {
                            let mut root = scope;
                            while let Some(next_parent) = self.resolution_graph.inner[root].parent()
                            {
                                root = next_parent;
                            }
                            root
                        } else {
                            error!("the match that led to this arm should prevent this from ever happening");
                            scope
                        }
                    }
                    // Default case: enter the matching child scope
                    false => {
                        let mut path_finder = PathFinder {
                            resolution_graph: self.resolution_graph,
                            errors: self.errors,
                            resolved_uses: self.resolved_uses,
                            visited_glob_scopes: Default::default(),
                        };
                        let found_children =
                            match path_finder.find_children(ctx, scope, &path.ident, true) {
                                Ok(v) => v,
                                Err(err) => {
                                    self.errors.push(err);
                                    return;
                                }
                            };
                        if found_children.len() > 1 {
                            self.errors.push(
                                DisambiguationError {
                                    file: ctx.file.clone(),
                                    ident: path.ident.clone(),
                                    src: AmbiguitySource::Item(
                                        if is_entry && ctx.has_leading_colon {
                                            ItemHint::ExternalNamedScope
                                        } else if is_entry {
                                            ItemHint::InternalNamedChildOrExternalNamedScope
                                        } else {
                                            ItemHint::InternalNamedChildScope
                                        },
                                    ),
                                }
                                .into(),
                            );
                        }
                        *found_children.first().unwrap()
                    }
                };
                ctx.previous_idents.push(&path.ident);
                self.trace_use(ctx, new_scope, &path.tree, false);
                ctx.previous_idents.pop();
            }
            Name(UseName { ident, .. }) | Rename(UseRename { ident, .. }) => {
                let found_children: Vec<ResolutionIndex> = if ident == "self" {
                    if !in_group {
                        self.errors.push(
                            SelfUsageError {
                                file: ctx.file.clone(),
                                name_ident: ident.clone(),
                                cause: SelfUsageErrorCause::NotInGroup,
                            }
                            .into(),
                        );
                        return;
                    } else if ctx.previous_idents.is_empty() {
                        self.errors.push(
                            SelfUsageError {
                                file: ctx.file.clone(),
                                name_ident: ident.clone(),
                                cause: SelfUsageErrorCause::InGroupAtRoot,
                            }
                            .into(),
                        );
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
                if let Some(children) = self.resolution_graph.inner[ctx.dest].children_mut() {
                    match tree {
                        Name(name) => children
                            .entry(Some(&name.ident))
                            .or_default()
                            .extend(&found_children),
                        Rename(rename) => children
                            .entry(Some(&rename.rename))
                            .or_default()
                            .extend(&found_children),
                        _ => {}
                    }
                }
            }
            Glob(glob) => {
                if is_entry
                    || ctx.has_leading_colon
                    || ctx
                        .previous_idents
                        .last()
                        .map(|ident| *ident == "self")
                        .unwrap_or_default()
                {
                    self.errors.push(
                        GlobAtEntryError {
                            file: ctx.file.clone(),
                            star_span: glob.star_token.spans[0],
                            has_leading_colon: ctx.has_leading_colon,
                            previous_ident: ctx
                                .previous_idents
                                .last()
                                .map(|ident| (*ident).clone()),
                        }
                        .into(),
                    );
                    return;
                }
                if let Some(mut children) = self.resolution_graph.inner[ctx.dest].children_mut() {
                    children.entry(None).or_default().push(scope)
                }
            }
            Group(group) => group
                .items
                .iter()
                .for_each(|tree| self.trace_use(ctx, scope, tree, true)),
        }
    }
}
