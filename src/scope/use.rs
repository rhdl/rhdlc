use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{ItemMod, UseName, UseRename, UseTree};

use super::{File, Node, ScopeError, ScopeGraph};
use crate::error::{
    DisambiguationError, GlobAtEntryError, GlobalPathCannotHaveSpecialIdentError,
    SelfNameNotInGroupError, SpecialIdentNotAtStartOfPathError, TooManySupersError,
    UnresolvedImportError, VisibilityError,
};

#[derive(Debug)]
pub enum UseType<'ast> {
    /// Pull a particular name into scope
    Name {
        name: &'ast UseName,
        index: NodeIndex,
    },
    /// Optionally include all items/mods from the scope
    Glob { scope: NodeIndex },
    /// Pull a particular name into scope, but give it a new name (so as to avoid any conflicts)
    Rename {
        rename: &'ast UseRename,
        index: NodeIndex,
    },
}

struct TracingContext<'a, 'ast> {
    scope_graph: &'a mut ScopeGraph<'ast>,
    errors: &'a mut Vec<ScopeError>,
    file: Rc<File>,
    dest: NodeIndex,
    previous_idents: Vec<syn::Ident>,
    has_leading_colon: bool,
}

/// TODO: Disambiguation errors can be done at this point instead of during tracing
pub fn trace_use_entry<'a, 'ast>(
    scope_graph: &'a mut ScopeGraph<'ast>,
    errors: &mut Vec<ScopeError>,
    dest: NodeIndex,
) {
    let (tree, file, has_leading_colon) = match &scope_graph[dest] {
        Node::Use { item_use, file, .. } => (
            &item_use.tree,
            file.clone(),
            item_use.leading_colon.is_some(),
        ),
        _ => return,
    };

    let scope = if has_leading_colon {
        // just give any old dummy node because it'll have to be ignored in path/name finding
        NodeIndex::new(0)
    } else {
        scope_graph
            .neighbors_directed(dest, Direction::Incoming)
            .next()
            .unwrap()
    };

    let mut ctx = TracingContext {
        scope_graph,
        errors,
        file: file,
        dest,
        previous_idents: vec![],
        has_leading_colon,
    }
    .into();

    trace_use(&mut ctx, scope, tree, false);
}

/// Trace usages
/// TODOs:
/// * Handle "self" properly
///     * self in a group
///     * self at the beginning of a path (anywhere else is technically an error since it's a nop)
/// * Handle "super", "super::super"
/// * Disambiguate between crate imports and local module imports
///     * A beginning :: explicitly refers to the global scope (handled in call)
///     * A beginning `self` explicitly refers to the local scope
///     * A beginning `super` explicitly refers to the parent scope
///     * A beginning `crate` explicitly refers to the root scope
///     * Any other word is implicitly the global or local scope
///         * Error if there is a root with the same name as a module in the local scope.
///             * Requires explicit disambiguation
/// * Check scope visibility (!important)
/// * Global imports
///     * Roots need names: `crate` is "this" root, vs. any other identifier
fn trace_use<'a, 'ast>(
    ctx: &mut TracingContext<'a, 'ast>,
    scope: NodeIndex,
    tree: &'ast UseTree,
    in_group: bool,
) {
    use syn::UseTree::*;
    // Is this the tracing entry point? (value comparison)
    // `item_use.tree` will always be either equal to or a superset of `tree`
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
                        ctx.errors.push(
                            SpecialIdentNotAtStartOfPathError {
                                file: ctx.file.clone(),
                                path_ident: path.ident.clone(),
                            }
                            .into(),
                        );
                        return;
                    }
                    if ctx.has_leading_colon {
                        ctx.errors.push(
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
                        let use_grandparent = ctx
                            .scope_graph
                            .neighbors_directed(scope, Direction::Incoming)
                            .next();
                        if use_grandparent.is_none() {
                            ctx.errors.push(
                                TooManySupersError {
                                    file: ctx.file.clone(),
                                    ident: path.ident.clone(),
                                }
                                .into(),
                            );
                            return;
                        }
                        use_grandparent.unwrap()
                    } else if path_ident == "crate" {
                        let mut root = scope;
                        while let Some(next_parent) = ctx
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
                    let same_ident_finder = |child: &NodeIndex| {
                        match &ctx.scope_graph[*child] {
                            Node::Mod { item_mod, .. } => item_mod.ident == path.ident.to_string(),
                            // this will work just fine since n is a string
                            Node::Root { name: Some(n), .. } => path.ident == n,
                            _ => false,
                        }
                    };
                    let child = if is_entry && ctx.has_leading_colon {
                        // we know the scope can be ignored in this case...
                        ctx.scope_graph
                            .externals(Direction::Incoming)
                            .find(same_ident_finder)
                    } else if is_entry {
                        let global_child = ctx
                            .scope_graph
                            .externals(Direction::Incoming)
                            .find(same_ident_finder);
                        let local_child = ctx.scope_graph.neighbors(scope).find(same_ident_finder);

                        if let (Some(_gc), Some(_lc)) = (global_child, local_child) {
                            ctx.errors.push(
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
                        ctx.scope_graph.neighbors(scope).find(same_ident_finder)
                    };
                    if child.is_none() {
                        ctx.errors.push(
                            UnresolvedImportError {
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
            // Only check visibility if scope differs
            let last_is_self = ctx
                .previous_idents
                .last()
                .map(|ident| ident == "self")
                .unwrap_or_default();
            if !is_entry
                && !last_is_self
                && !super::visibility::is_target_visible(ctx.scope_graph, scope, new_scope).unwrap()
            {
                ctx.errors.push(
                    VisibilityError {
                        name_file: ctx.file.clone(),
                        name_ident: path.ident.clone(),
                    }
                    .into(),
                );
                return;
            }
            ctx.previous_idents.push(path.ident.clone());
            trace_use(ctx, new_scope, &path.tree, false);
            ctx.previous_idents.pop();
        }
        Name(UseName { ident, .. }) | Rename(UseRename { ident, .. }) => {
            let original_name_string = ident.to_string();
            let found_index = if original_name_string == "self" {
                if !in_group {
                    ctx.errors.push(
                        SelfNameNotInGroupError {
                            file: ctx.file.clone(),
                            name_ident: ident.clone(),
                        }
                        .into(),
                    );
                    return;
                }
                Some(scope)
            } else {
                let finder = |child: &NodeIndex| match &ctx.scope_graph[*child] {
                    Node::Item { ident, .. } => **ident == original_name_string,
                    Node::Mod {
                        item_mod: ItemMod { ident, .. },
                        ..
                    } => *ident == original_name_string,
                    Node::Root { name: Some(n), .. } => original_name_string == *n,
                    Node::Use {
                        imports: other_use_imports,
                        ..
                    } => {
                        if other_use_imports.is_empty() {
                            error!("uses that aren't traced yet can't be resolved");
                            return false;
                        }
                        other_use_imports.iter().any(|(_, use_types)| {
                            use_types.iter().any(|use_type| match use_type {
                                UseType::Name { name, .. } => name.ident == original_name_string,
                                UseType::Rename { rename, .. } => {
                                    rename.rename == original_name_string
                                }
                                _ => false,
                            })
                        })
                    }
                    _ => false,
                };

                let child = if is_entry && ctx.has_leading_colon {
                    // special resolution required
                    ctx.scope_graph.externals(Direction::Incoming).find(finder)
                } else if is_entry {
                    // todo: disambiguation error
                    let global_child = ctx.scope_graph.externals(Direction::Incoming).find(finder);
                    let local_child = ctx
                        .scope_graph
                        .neighbors(scope)
                        .filter(|child| *child != ctx.dest)
                        .find(finder);
                    if let (Some(_gc), Some(_lc)) = (global_child, local_child) {
                        ctx.errors.push(
                            DisambiguationError {
                                file: ctx.file.clone(),
                                ident: ident.clone(),
                            }
                            .into(),
                        );
                        return;
                    }
                    global_child.or(local_child)
                } else {
                    // todo: unwrap_or_else look for glob implicit imports
                    ctx.scope_graph
                        .neighbors(scope)
                        .filter(|child| *child != ctx.dest)
                        .find(finder)
                };
                child
            };
            if found_index.is_none() {
                ctx.errors.push(
                    UnresolvedImportError {
                        file: ctx.file.clone(),
                        previous_idents: ctx.previous_idents.clone(),
                        unresolved_ident: ident.clone(),
                        has_leading_colon: ctx.has_leading_colon,
                    }
                    .into(),
                );
                return;
            }
            let index = found_index.unwrap();
            if !is_entry
                && original_name_string != "self"
                && !super::visibility::is_target_visible(ctx.scope_graph, scope, index).unwrap()
            {
                ctx.errors.push(
                    VisibilityError {
                        name_file: ctx.file.clone(),
                        name_ident: ident.clone(),
                    }
                    .into(),
                );
                return;
            }
            if let Node::Use { imports, .. } = &mut ctx.scope_graph[ctx.dest] {
                match tree {
                    Name(name) => imports
                        .entry(scope)
                        .or_default()
                        .push(UseType::Name { name, index }),
                    Rename(rename) => imports
                        .entry(scope)
                        .or_default()
                        .push(UseType::Rename { rename, index }),
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
                ctx.errors.push(
                    GlobAtEntryError {
                        file: ctx.file.clone(),
                        star_span: glob.star_token.spans[0].clone(),
                        has_leading_colon: ctx.has_leading_colon,
                    }
                    .into(),
                );
                return;
            }
            if let Node::Use { imports, .. } = &mut ctx.scope_graph[ctx.dest] {
                imports
                    .entry(scope)
                    .or_default()
                    .push(UseType::Glob { scope })
            }
        }
        Group(group) => group
            .items
            .iter()
            .for_each(|tree| trace_use(ctx, scope, tree, true)),
    }
}
