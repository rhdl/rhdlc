use std::collections::HashSet;
use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{ItemMod, ItemUse, UseName, UseRename, UseTree};

use super::{File, Node, ResolutionError, ScopeGraph};
use crate::error::{
    DisambiguationError, GlobAtEntryError, GlobalPathCannotHaveSpecialIdentError,
    SelfNameNotInGroupError, SpecialIdentNotAtStartOfPathError, TooManySupersError,
    UnresolvedItemError, VisibilityError,
};

#[derive(Debug)]
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
    dest: NodeIndex,
    previous_idents: Vec<syn::Ident>,
    has_leading_colon: bool,
}

impl TracingContext {
    fn try_new(scope_graph: &ScopeGraph, dest: NodeIndex) -> Option<Self> {
        match &scope_graph[dest] {
            Node::Use { item_use, file, .. } => Some(Self {
                file: file.clone(),
                dest,
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
                        let is_last_super = ctx
                            .previous_idents
                            .last()
                            .map(|ident| ident == "super")
                            .unwrap_or_default();
                        if !is_entry && !(path_ident == "super" && is_last_super) {
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
                    _ => {
                        // TODO: check uses for same ident
                        // i.e. use a::b; use b::c;
                        let same_ident_finder = |child: &NodeIndex| {
                            match &self.scope_graph[*child] {
                                Node::Mod { item_mod, .. } => {
                                    item_mod.ident == path.ident.to_string()
                                }
                                // this will work just fine since n is a string
                                Node::Root { name: Some(n), .. } => path.ident == n,
                                _ => false,
                            }
                        };
                        let child = if is_entry && ctx.has_leading_colon {
                            // we know the scope can be ignored in this case...
                            self.scope_graph
                                .externals(Direction::Incoming)
                                .find(same_ident_finder)
                        } else if is_entry {
                            let global_child = self
                                .scope_graph
                                .externals(Direction::Incoming)
                                .find(same_ident_finder);
                            let local_child =
                                self.scope_graph.neighbors(scope).find(same_ident_finder);

                            if let (Some(_gc), Some(_lc)) = (global_child, local_child) {
                                self.errors.push(
                                    DisambiguationError {
                                        file: ctx.file.clone(),
                                        ident: path.ident.clone(),
                                    }
                                    .into(),
                                );
                                return;
                            }
                            global_child.or(local_child)
                        } else {
                            self.scope_graph.neighbors(scope).find(same_ident_finder)
                        };
                        if child.is_none() {
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
                        }
                        child.unwrap()
                    }
                };
                if !super::visibility::is_target_visible(self.scope_graph, ctx.dest, new_scope)
                    .unwrap()
                {
                    self.errors.push(
                        VisibilityError {
                            name_file: ctx.file.clone(),
                            name_ident: path.ident.clone(),
                        }
                        .into(),
                    );
                    return;
                }
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
                    vec![scope]
                } else {
                    // reentrancy behavior
                    if !(is_entry && ctx.has_leading_colon) {
                        for reentrant in self
                            .scope_graph
                            .neighbors(scope)
                            .filter(|candidate| *candidate != ctx.dest)
                            .filter(|candidate| !self.reentrancy.contains(&candidate))
                            .filter(
                                |candidate: &NodeIndex| match &self.scope_graph[*candidate] {
                                    Node::Use {
                                        imports: other_use_imports,
                                        ..
                                    } => other_use_imports.is_empty(),
                                    _ => false,
                                },
                            )
                            .collect::<Vec<NodeIndex>>()
                        {
                            let other_use_tree = match &self.scope_graph[reentrant] {
                                Node::Use {
                                    item_use:
                                        ItemUse {
                                            tree: other_use_tree,
                                            ..
                                        },
                                    ..
                                } => other_use_tree,
                                _ => continue,
                            };
                            let mut rebuilt_ctx =
                                TracingContext::try_new(self.scope_graph, reentrant).unwrap();
                            self.trace_use_entry_reenterable(&mut rebuilt_ctx, other_use_tree);
                        }
                    }
                    let child_no_glob_matcher = |child: &NodeIndex| match &self.scope_graph[*child]
                    {
                        Node::Var { ident, .. }
                        | Node::Macro { ident, .. }
                        | Node::Type { ident, .. } => **ident == original_name_string,
                        Node::Fn { item_fn, .. } => item_fn.sig.ident == original_name_string,
                        Node::Mod {
                            item_mod: ItemMod { ident, .. },
                            ..
                        } => *ident == original_name_string,
                        Node::Root { name: Some(n), .. } => original_name_string == *n,
                        Node::Root { name: None, .. } => false,
                        Node::Impl { .. } => false,
                        Node::Use {
                            imports: other_use_imports,
                            ..
                        } => {
                            if other_use_imports.is_empty() {
                                error!(
                                    "a use failed to resolve, or a recursive use was encountered"
                                );
                                return false;
                            }
                            other_use_imports.iter().any(|(_, use_types)| {
                                use_types.iter().any(|use_type| match use_type {
                                    UseType::Name { name, .. } => {
                                        name.ident == original_name_string
                                    }
                                    UseType::Rename { rename, .. } => {
                                        rename.rename == original_name_string
                                    }
                                    _ => false,
                                })
                            })
                        }
                    };

                    if is_entry && ctx.has_leading_colon {
                        // special resolution required
                        self.scope_graph
                            .externals(Direction::Incoming)
                            .find(child_no_glob_matcher)
                            .map(|child| vec![child])
                            .unwrap_or_default()
                    } else if is_entry {
                        let global_child = self
                            .scope_graph
                            .externals(Direction::Incoming)
                            .find(child_no_glob_matcher);
                        let local_children = self
                            .scope_graph
                            .neighbors(scope)
                            .filter(|child| *child != ctx.dest)
                            .filter(child_no_glob_matcher)
                            .collect::<Vec<NodeIndex>>();
                        if let (Some(_gc), true) = (global_child, !local_children.is_empty()) {
                            self.errors.push(
                                DisambiguationError {
                                    file: ctx.file.clone(),
                                    ident: ident.clone(),
                                }
                                .into(),
                            );
                            return;
                        }
                        global_child.map(|gc| vec![gc]).unwrap_or(local_children)
                    } else {
                        self.scope_graph
                            .neighbors(scope)
                            .filter(|child| *child != ctx.dest)
                            .filter(child_no_glob_matcher)
                            .collect()
                    }
                };
                // TODO: attempt to save by using matching glob children instead
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
                };
                let found_children = found_children
                    .iter()
                    .filter(|index| {
                        super::visibility::is_target_visible(self.scope_graph, ctx.dest, **index)
                            .unwrap()
                    })
                    .cloned()
                    .collect::<Vec<NodeIndex>>();
                if found_children.is_empty() {
                    self.errors.push(
                        VisibilityError {
                            name_file: ctx.file.clone(),
                            name_ident: ident.clone(),
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
}
