use std::collections::HashSet;
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
                    _ => {
                        // TODO: check uses for same ident
                        // TODO disambiguation error for this ^
                        // i.e. use a::b; use b::c;
                        let same_ident_finder = |child: &NodeIndex| {
                            match &self.scope_graph[*child] {
                                Node::Mod { item_mod, .. } => {
                                    item_mod.ident == path.ident.to_string()
                                }
                                // this will work just fine since n is a string
                                Node::Root { name: Some(n), .. } => path.ident == n,
                                // Node::Use {imports, ..} => {

                                // }
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
                                // CLAIM: these will always be names, because globs are not included
                                self.errors.push(
                                    DisambiguationError {
                                        file: ctx.file.clone(),
                                        ident: path.ident.clone(),
                                        this: AmbiguitySource::Name,
                                        other: AmbiguitySource::Name,
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
                if !super::r#pub::is_target_visible(self.scope_graph, ctx.dest, new_scope).unwrap()
                {
                    self.errors.push(
                        ItemVisibilityError {
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
                        // TODO: self in group but the group is the first thing
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
                            .filter(|candidate| match &self.scope_graph[*candidate] {
                                Node::Use {
                                    item_use, imports, ..
                                } => {
                                    imports.is_empty() && {
                                        let mut checker = ReentrancyNeededChecker {
                                            name_to_look_for: &original_name_string,
                                            needed: false,
                                        };
                                        checker.visit_item_use(item_use);
                                        checker.needed
                                    }
                                }
                                _ => false,
                            })
                            .filter(|candidate| !self.reentrancy.contains(&candidate))
                            .collect::<Vec<NodeIndex>>()
                        {
                            let other_use_tree = match &self.scope_graph[reentrant] {
                                Node::Use { item_use, .. } => &item_use.tree,
                                _ => continue,
                            };
                            let mut rebuilt_ctx =
                                TracingContext::try_new(self.scope_graph, reentrant).unwrap();
                            self.trace_use_entry_reenterable(&mut rebuilt_ctx, other_use_tree);
                        }
                    }

                    let global_iterator = self
                        .scope_graph
                        .externals(Direction::Incoming)
                        .filter(|child| *child != ctx.root)
                        .filter(|child| self.matches(child, &original_name_string, false));
                    let local_iterator = self
                        .scope_graph
                        .neighbors(scope)
                        .filter(|child| *child != ctx.dest)
                        .filter(|child| self.matches(child, &original_name_string, false));
                    if is_entry {
                        let global = global_iterator.collect();
                        if ctx.has_leading_colon {
                            global
                        } else {
                            let local: Vec<NodeIndex> = local_iterator.collect();
                            if !global.is_empty() && !local.is_empty() {
                                // CLAIM: these will always be names, because globs are not included
                                self.errors.push(
                                    DisambiguationError {
                                        file: ctx.file.clone(),
                                        ident: ident.clone(),
                                        this: AmbiguitySource::Name,
                                        other: AmbiguitySource::Glob,
                                    }
                                    .into(),
                                );
                                return;
                            }
                            global.iter().chain(local.iter()).cloned().collect()
                        }
                    } else {
                        local_iterator.collect()
                    }
                };

                let found_children =
                    if found_children.is_empty() && !(ctx.has_leading_colon && is_entry) {
                        let local_matched_globs: Vec<NodeIndex> = self
                            .scope_graph
                            .neighbors(scope)
                            .filter(|child| *child != ctx.dest)
                            .filter(|child| self.matches(child, &original_name_string, true))
                            .collect();
                        if local_matched_globs.len() > 1 {
                            // CLAIM: these will always be globs
                            self.errors.push(
                                DisambiguationError {
                                    file: ctx.file.clone(),
                                    ident: ident.clone(),
                                    this: AmbiguitySource::Glob,
                                    other: AmbiguitySource::Glob,
                                }
                                .into(),
                            );
                            return;
                        } else {
                            local_matched_globs
                        }
                    } else {
                        found_children
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

                let found_children = found_children
                    .iter()
                    .filter(|index| {
                        super::r#pub::is_target_visible(self.scope_graph, ctx.dest, **index)
                            .unwrap()
                    })
                    .cloned()
                    .collect::<Vec<NodeIndex>>();
                if found_children.is_empty() {
                    self.errors.push(
                        ItemVisibilityError {
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

    fn matches(&self, node: &NodeIndex, name_to_look_for: &str, glob_only: bool) -> bool {
        match &self.scope_graph[*node] {
            Node::Var { ident, .. } | Node::Macro { ident, .. } | Node::Type { ident, .. } => {
                !glob_only && *ident == name_to_look_for
            }
            Node::Fn { item_fn, .. } => !glob_only && item_fn.sig.ident == name_to_look_for,
            Node::Mod {
                item_mod: ItemMod { ident, .. },
                ..
            } => !glob_only && *ident == name_to_look_for,
            Node::Root { name: Some(n), .. } => !glob_only && n == name_to_look_for,
            Node::Use {
                imports: other_use_imports,
                item_use,
                ..
            } => {
                if other_use_imports.is_empty() {
                    if self.reentrancy.contains(node) {
                        error!("a recursive use was encountered and cut off");
                    } else if {
                        let mut checker = ReentrancyNeededChecker {
                            name_to_look_for,
                            needed: false,
                        };
                        checker.visit_item_use(item_use);
                        checker.needed
                    } {
                        error!("this use failed to resolve");
                    }
                    return false;
                }
                other_use_imports.iter().any(|(_, use_types)| {
                    use_types.iter().any(|use_type| match use_type {
                        UseType::Name { name, .. } => !glob_only && name.ident == name_to_look_for,
                        UseType::Rename { rename, .. } => {
                            !glob_only && rename.rename == name_to_look_for
                        }
                        UseType::Glob { scope } => {
                            glob_only
                                && self.scope_graph.neighbors(*scope).any(|child| {
                                    self.matches(&child, name_to_look_for, false)
                                        || self.matches(&child, name_to_look_for, true)
                                })
                        }
                    })
                })
            }
            Node::Root { name: None, .. } => false,
            Node::Impl { .. } => false,
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
