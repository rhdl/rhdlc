use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{ItemMod, UseName, UseRename, UseTree};

use super::{File, Node, ScopeError, ScopeGraph};
use crate::error::{
    PathDisambiguationError, SpecialIdentNotAtStartOfPathError, UnresolvedImportError,
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
}

/// TODO: Disambiguation errors can be done at this point instead of during tracing
pub fn trace_use_entry<'a, 'ast>(
    scope_graph: &'a mut ScopeGraph<'ast>,
    errors: &mut Vec<ScopeError>,
    dest: NodeIndex,
) {
    let (tree, has_leading_colon) = match &scope_graph[dest] {
        Node::Use { item_use, .. } => (&item_use.tree, item_use.leading_colon.is_some()),
        _ => return,
    };

    let scope = if has_leading_colon {
        // TODO: this is wrong, roots need names now
        let mut root = dest;
        while match &scope_graph[root] {
            Node::Root { .. } => false,
            _ => true,
        } {
            root = scope_graph
                .neighbors_directed(root, Direction::Incoming)
                .next()
                .unwrap();
        }
        root
    } else {
        scope_graph
            .neighbors_directed(dest, Direction::Incoming)
            .next()
            .unwrap()
    };

    let file = match &scope_graph[scope_graph
        .neighbors_directed(dest, Direction::Incoming)
        .next()
        .unwrap()]
    {
        Node::Root { file, .. } => file.clone(),
        Node::Mod { file, .. } => file.clone(),
        _ => return,
    };

    let mut ctx = TracingContext {
        scope_graph,
        errors,
        file: file,
        dest,
        previous_idents: vec![],
    }
    .into();

    trace_use(&mut ctx, scope, tree);
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
fn trace_use<'a, 'ast>(ctx: &mut TracingContext<'a, 'ast>, scope: NodeIndex, tree: &'ast UseTree) {
    use syn::UseTree::*;
    // Is this the tracing entry point? (value comparison)
    // `item_use.tree` will always be either equal to or a superset of `tree`
    let is_entry = ctx.previous_idents.is_empty();
    match tree {
        Path(path) => {
            let path_ident = path.ident.to_string();
            match path_ident.as_str() {
                // Special keyword cases
                "self" | "super" | "crate" => {
                    let is_last_super = ctx
                        .previous_idents
                        .last()
                        .map(|ident| ident == "self")
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
                    let use_parent = ctx
                        .scope_graph
                        .neighbors_directed(ctx.dest, Direction::Incoming)
                        .next()
                        .unwrap();
                    if match path.tree.as_ref() {
                        Name(name) => name.ident == "self",
                        Rename(rename) => rename.ident == "self",
                        _ => false,
                    } {
                        // Handle bad selves now.
                        todo!("a self that isn't in a group is an error")
                    }
                    if path_ident == "self" {
                        ctx.previous_idents.push(path.ident.clone());
                        trace_use(ctx, use_parent, &path.tree);
                        ctx.previous_idents.pop();
                    } else if path_ident == "super" {
                        let use_grandparent = ctx
                            .scope_graph
                            .neighbors_directed(use_parent, Direction::Incoming)
                            .next()
                            .expect("todo, going beyond the root is an error");
                        ctx.previous_idents.push(path.ident.clone());
                        trace_use(ctx, use_grandparent, &path.tree);
                        ctx.previous_idents.pop();
                    } else if path_ident == "crate" {
                        let mut root = use_parent;
                        while let Some(next_parent) = ctx
                            .scope_graph
                            .neighbors_directed(root, Direction::Incoming)
                            .next()
                        {
                            root = next_parent;
                        }
                        ctx.previous_idents.push(path.ident.clone());
                        trace_use(ctx, root, &path.tree);
                        ctx.previous_idents.pop();
                    }
                }
                // Default case: enter the matching child scope
                _ => {
                    let child = ctx.scope_graph.neighbors(scope).find(|child| {
                        if let Node::Mod { item_mod, .. } = ctx.scope_graph[*child] {
                            item_mod.ident == path.ident.to_string()
                        } else {
                            false
                        }
                    });
                    if child.is_none() {
                        ctx.errors.push(
                            UnresolvedImportError {
                                file: ctx.file.clone(),
                                previous_idents: ctx.previous_idents.clone(),
                                unresolved_ident: path.ident.clone(),
                            }
                            .into(),
                        );
                        return;
                    }
                    ctx.previous_idents.push(path.ident.clone());
                    trace_use(ctx, child.unwrap(), &path.tree);
                    ctx.previous_idents.pop();
                }
            };
        }
        Name(UseName { ident, .. }) | Rename(UseRename { ident, .. }) => {
            let original_name_string = ident.to_string();
            let found_index = if original_name_string == "self" {
                Some(scope)
            } else {
                let child =
                    ctx.scope_graph
                        .neighbors(scope)
                        .find(|child| match &ctx.scope_graph[*child] {
                            Node::Item { ident, .. } => **ident == original_name_string,
                            Node::Mod {
                                item_mod: ItemMod { ident, .. },
                                ..
                            } => *ident == original_name_string,
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
                            _ => false,
                        });
                // TODO: check for implicit glob imports
                child
            };
            if found_index.is_none() {
                //
                ctx.errors.push(
                    UnresolvedImportError {
                        file: ctx.file.clone(),
                        previous_idents: ctx.previous_idents.clone(),
                        unresolved_ident: ident.clone(),
                    }
                    .into(),
                );
                return;
            }
            let index = found_index.unwrap();
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
        Glob(_) => {
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
            .for_each(|tree| trace_use(ctx, scope, tree)),
    }
}
