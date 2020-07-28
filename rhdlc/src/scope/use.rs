use log::warn;
use petgraph::{graph::NodeIndex, Direction};
use syn::{ItemMod, UseName, UseRename, UseTree};

use super::{Node, ScopeGraph};

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

/// TODO: Disambiguation errors can be done at this point instead of during tracing
pub fn trace_use_entry<'ast>(scope_graph: &mut ScopeGraph<'ast>, dest: NodeIndex) {
    let (tree, has_leading_colon) = match &scope_graph[dest] {
        Node::Use { item_use, .. } => (&item_use.tree, item_use.leading_colon.is_some()),
        _ => return,
    };

    let scope = if has_leading_colon {
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

    trace_use(scope_graph, dest, scope, tree);
}

/// Trace usages
/// TODOs:
/// * Handle "self" properly
///     * self in a group
///     * self at the beginning of a path (anywhere else is technically an error since it's a nop)
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
fn trace_use<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    dest: NodeIndex,
    // Begins at the root
    scope: NodeIndex,
    tree: &'ast UseTree,
) {
    use syn::UseTree::*;
    // Is this the tracing entry point? (value comparison)
    // `item_use.tree` will always be either equal to or a superset of `tree`
    let is_entry = if let Node::Use { item_use, .. } = &scope_graph[dest] {
        *tree == item_use.tree
    } else {
        return;
    };
    match tree {
        Path(path) => {
            let path_ident = path.ident.to_string();
            match path_ident.as_str() {
                // Special keyword cases
                "self" | "super" | "crate" => {
                    if !is_entry {
                        todo!(
                            "a `{}` that isn't at the beginning of a path is an error",
                            path_ident
                        );
                    }
                    let use_parent = scope_graph
                        .neighbors_directed(dest, Direction::Incoming)
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
                        trace_use(scope_graph, dest, use_parent, &path.tree);
                    } else if path_ident == "super" {
                        let use_grandparent = scope_graph
                            .neighbors_directed(use_parent, Direction::Incoming)
                            .next()
                            .expect("todo, going beyond the root is an error");
                        trace_use(scope_graph, dest, use_grandparent, &path.tree);
                    } else if path_ident == "crate" {
                        let mut root = use_parent;
                        while let Some(next_parent) = scope_graph
                            .neighbors_directed(root, Direction::Incoming)
                            .next()
                        {
                            root = next_parent;
                        }
                        trace_use(scope_graph, dest, root, &path.tree);
                    }
                }
                // Default case: enter the matching child scope
                _ => {
                    let child = scope_graph.neighbors(scope).find(|child| {
                        if let Node::Mod { item_mod, .. } = scope_graph[*child] {
                            item_mod.ident == path.ident.to_string()
                        } else {
                            false
                        }
                    });
                    trace_use(
                        scope_graph,
                        dest,
                        child.expect("todo, entering a non-existent module is an error"),
                        &path.tree,
                    );
                }
            };
        }
        Name(UseName { ident, .. }) | Rename(UseRename { ident, .. }) => {
            let original_name_string = ident.to_string();
            let found_index = if original_name_string == "self" {
                Some(scope)
            } else {
                let child = scope_graph
                    .neighbors(scope)
                    .find(|child| match &scope_graph[*child] {
                        Node::Item { ident, .. } => **ident == original_name_string,
                        Node::Mod {
                            item_mod: ItemMod { ident, .. },
                            ..
                        } => *ident == original_name_string,
                        Node::Use { .. } => {
                            warn!("uses aren't recursively traced (yet)");
                            false
                        }
                        _ => false,
                    });
                child
            };
            let index = found_index.expect("uses that aren't found are an error");
            if let Node::Use { imports, .. } = &mut scope_graph[dest] {
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
            if let Node::Use { imports, .. } = &mut scope_graph[dest] {
                imports
                    .entry(scope)
                    .or_default()
                    .push(UseType::Glob { scope })
            }
        }
        Group(group) => group
            .items
            .iter()
            .for_each(|tree| trace_use(scope_graph, dest, scope, tree)),
    }
}
