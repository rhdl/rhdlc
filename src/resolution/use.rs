use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{visit::Visit, ItemMod, UseGlob, UseName, UsePath, UseRename, UseTree};

use super::{File, Node, ResolutionError, ScopeGraph};
use crate::error::{
    AmbiguitySource, DisambiguationError, GlobAtEntryError, GlobalPathCannotHaveSpecialIdentError,
    ItemVisibilityError, SelfNameNotInGroupError, SpecialIdentNotAtStartOfPathError,
    TooManySupersError, UnresolvedItemError,
};

#[derive(Debug, Clone)]
pub enum UseType<'ast> {
    /// Pull a particular name into scope
    /// Could be ambiguous
    Name {
        name: &'ast UseName,
        indices: Vec<NodeIndex>,
    },
    /// Optionally include all items/mods from the scope
    Glob { scope: NodeIndex },
    /// Pull a particular name into scope, but give it a new name (so as to avoid any conflicts)
    /// Could be ambiguous
    Rename {
        rename: &'ast UseRename,
        indices: Vec<NodeIndex>,
    },
}

struct TracingContext {
    file: Rc<File>,
    root: NodeIndex,
    dest: NodeIndex,
    previous_idents: Vec<syn::Ident>,
    has_leading_colon: bool,
}

impl TracingContext {
    fn try_new(scope_graph: &ScopeGraph, dest: NodeIndex) -> Option<Self> {
        let mut root = dest;
        while let Some(parent) = scope_graph
            .neighbors_directed(root, Direction::Incoming)
            .next()
        {
            root = parent;
        }
        match &scope_graph[dest] {
            Node::Use { item_use, file, .. } => Some(Self {
                file: file.clone(),
                dest,
                root,
                previous_idents: vec![],
                has_leading_colon: item_use.leading_colon.is_some(),
            }),
            _ => None,
        }
    }
}

pub struct UseResolver<'a, 'ast> {
    scope_graph: &'a mut ScopeGraph<'ast>,
    errors: &'a mut Vec<ResolutionError>,
    reentrancy: HashSet<NodeIndex>,
}

impl<'a, 'ast> UseResolver<'a, 'ast> {
    pub fn new(
        scope_graph: &'a mut ScopeGraph<'ast>,
        errors: &'a mut Vec<ResolutionError>,
    ) -> Self {
        Self {
            scope_graph,
            errors,
            reentrancy: HashSet::default(),
        }
    }

    pub fn resolve_use(&mut self, dest: NodeIndex) {
        let tree = match self.scope_graph[dest] {
            Node::Use { item_use, .. } => &item_use.tree,
            _ => return,
        };
        self.trace_use_entry_reenterable(
            &mut TracingContext::try_new(self.scope_graph, dest).unwrap(),
            tree,
        );
    }

    fn trace_use_entry_reenterable(&mut self, ctx: &mut TracingContext, tree: &'ast UseTree) {
        if self.reentrancy.contains(&ctx.dest) {
            return;
        }
        self.reentrancy.insert(ctx.dest);
        let scope = if ctx.has_leading_colon {
            // just give any old dummy node because it'll have to be ignored in path/name finding
            NodeIndex::new(0)
        } else {
            self.scope_graph
                .neighbors_directed(ctx.dest, Direction::Incoming)
                .next()
                .unwrap()
        };
        self.trace_use(ctx, scope, tree, false);
    }

    /// Trace usages
    fn trace_use(
        &mut self,
        ctx: &mut TracingContext,
        scope: NodeIndex,
        tree: &'ast UseTree,
        in_group: bool,
    ) {
        use syn::UseTree::*;
        let is_entry = ctx.previous_idents.is_empty();
        match tree {
            Path(path) => {
                let path_ident = path.ident.to_string();
                let new_scope = match path_ident.as_str() {
                    // Special keyword cases
                    "self" | "super" | "crate" => {
                        let is_chained_supers = ctx
                            .previous_idents
                            .last()
                            .map(|ident| ident == "super")
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
                            if let Some(use_grandparent) = self
                                .scope_graph
                                .neighbors_directed(scope, Direction::Incoming)
                                .next()
                            {
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
                            while let Some(next_parent) = self
                                .scope_graph
                                .neighbors_directed(root, Direction::Incoming)
                                .next()
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
                    path_ident_str => {
                        let found_children =
                            match self.find_children(ctx, scope, &path.ident, path_ident_str, true)
                            {
                                Ok(v) => v,
                                Err(err) => {
                                    self.errors.push(err);
                                    return;
                                }
                            };
                        if found_children.is_empty() {
                            self.errors.push(
                                UnresolvedItemError {
                                    file: ctx.file.clone(),
                                    previous_idents: ctx.previous_idents.clone(),
                                    unresolved_ident: path.ident.clone(),
                                    has_leading_colon: ctx.has_leading_colon,
                                }
                                .into(),
                            );
                            return;
                        } else if found_children.len() > 1 {
                            todo!("disambiguation error");
                        }
                        *found_children.first().unwrap()
                    }
                };
                ctx.previous_idents.push(path.ident.clone());
                self.trace_use(ctx, new_scope, &path.tree, false);
                ctx.previous_idents.pop();
            }
            Name(UseName { ident, .. }) | Rename(UseRename { ident, .. }) => {
                let original_name_string = ident.to_string();
                let found_children: Vec<NodeIndex> = if original_name_string == "self" {
                    if !in_group {
                        self.errors.push(
                            SelfNameNotInGroupError {
                                file: ctx.file.clone(),
                                name_ident: ident.clone(),
                            }
                            .into(),
                        );
                        return;
                    }
                    if ctx.previous_idents.is_empty() {
                        todo!("self in group but the group is the first thing");
                    }
                    vec![scope]
                } else {
                    match self.find_children(ctx, scope, ident, &original_name_string, false) {
                        Ok(v) => v,
                        Err(err) => {
                            self.errors.push(err);
                            return;
                        }
                    }
                };

                if found_children.is_empty() {
                    self.errors.push(
                        UnresolvedItemError {
                            file: ctx.file.clone(),
                            previous_idents: ctx.previous_idents.clone(),
                            unresolved_ident: ident.clone(),
                            has_leading_colon: ctx.has_leading_colon,
                        }
                        .into(),
                    );
                    return;
                }
                if let Node::Use { imports, .. } = &mut self.scope_graph[ctx.dest] {
                    match tree {
                        Name(name) => imports.entry(scope).or_default().push(UseType::Name {
                            name,
                            indices: found_children,
                        }),
                        Rename(rename) => imports.entry(scope).or_default().push(UseType::Rename {
                            rename,
                            indices: found_children,
                        }),
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
                        .map(|ident| ident == "self")
                        .unwrap_or_default()
                {
                    self.errors.push(
                        GlobAtEntryError {
                            file: ctx.file.clone(),
                            star_span: glob.star_token.spans[0],
                            has_leading_colon: ctx.has_leading_colon,
                            previous_ident: ctx.previous_idents.last().cloned(),
                        }
                        .into(),
                    );
                    return;
                }
                if let Node::Use { imports, .. } = &mut self.scope_graph[ctx.dest] {
                    imports
                        .entry(scope)
                        .or_default()
                        .push(UseType::Glob { scope })
                }
            }
            Group(group) => group
                .items
                .iter()
                .for_each(|tree| self.trace_use(ctx, scope, tree, true)),
        }
    }

    fn find_children(
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
            Node::Use { item_use, .. } => {
                if self.reentrancy.contains(node) {
                    None
                } else if {
                    let mut checker = ReentrancyNeededChecker {
                        name_to_look_for,
                        needed: false,
                    };
                    checker.visit_item_use(item_use);
                    checker.needed
                } {
                    Some((
                        TracingContext::try_new(self.scope_graph, *node).unwrap(),
                        &item_use.tree,
                    ))
                } else {
                    None
                }
            }
            _ => return vec![],
        };
        if let Some((mut rebuilt_ctx, tree)) = rebuilt_ctx_opt {
            self.trace_use_entry_reenterable(&mut rebuilt_ctx, tree);
        }
        let imports = match &self.scope_graph[*node] {
            Node::Use { imports, .. } => imports.clone(),
            bad => panic!("this should not be reached: {:?}", bad),
        };
        if imports.is_empty() {
            error!("this use failed to resolve");
        }
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

struct ReentrancyNeededChecker<'a> {
    name_to_look_for: &'a str,
    needed: bool,
}

impl<'a, 'ast> Visit<'ast> for ReentrancyNeededChecker<'a> {
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
