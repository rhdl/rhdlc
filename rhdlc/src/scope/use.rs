use log::warn;
use petgraph::{graph::NodeIndex, Direction};
use syn::{ItemMod, UseName, UseRename, UseTree};

use super::{Node, ScopeGraph};

#[derive(Debug)]
pub enum UseType<'ast> {
    /// All children from the root/mod are included
    Name {
        name: &'ast UseName,
        index: NodeIndex,
    },
    Glob {
        scope: NodeIndex,
    },
    Rename {
        rename: &'ast UseRename,
        index: NodeIndex,
    },
}

/// TODO: Disambiguation errors can be done at this point instead of during tracing
pub fn trace_use_entry<'ast>(scope_graph: &mut ScopeGraph<'ast>, i: NodeIndex) {
    let (tree, has_leading_colon) = match &scope_graph[i] {
        Node::Use { item_use, .. } => (&item_use.tree, item_use.leading_colon.is_some()),
        _ => return,
    };

    let scope = if has_leading_colon {
        let mut root = i;
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
            .neighbors_directed(i, Direction::Incoming)
            .next()
            .unwrap()
    };

    trace_use(scope_graph, i, scope, tree);
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
/// * Check scope visibility
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
    let dest_tree = if let Node::Use { item_use, .. } = &scope_graph[dest] {
        &item_use.tree
    } else {
        return;
    };
    // Is this the tracing entry point?
    let is_entry = tree == dest_tree;
    match tree {
        Path(path) => {
            let path_ident = path.ident.to_string();
            match path_ident.as_str() {
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
                    if path_ident == "self" {
                        trace_use(scope_graph, dest, use_parent, &path.tree);
                    } else if path_ident == "super" {
                        let use_grandparent = scope_graph
                            .neighbors_directed(use_parent, Direction::Incoming)
                            .next()
                            .expect("todo, going beyond the root is an error");
                        trace_use(scope_graph, dest, use_grandparent, &path.tree);
                    } else {
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
        Name(name) => {
            let name_string = name.ident.to_string();
            let found_index = if name_string == "self" {
                Some(scope)
            } else {
                let child = scope_graph
                    .neighbors(scope)
                    .find(|child| match &scope_graph[*child] {
                        Node::Item { ident, .. } => **ident == name_string,
                        Node::Mod {
                            item_mod: ItemMod { ident, .. },
                            ..
                        } => *ident == name_string,
                        Node::Use { .. } => {
                            warn!("uses aren't recursively traced (yet)");
                            false
                        }
                        _ => false,
                    });
                child
            };
            if let Node::Use { imports, .. } = &mut scope_graph[dest] {
                imports.entry(scope).or_default().push(UseType::Name {
                    name,
                    index: found_index.expect("uses that aren't found are an error"),
                })
            }
        }
        Rename(rename) => {
            let name_string = rename.ident.to_string();
            let found_index = if name_string == "self" {
                Some(scope)
            } else {
                let child = scope_graph
                    .neighbors(scope)
                    .find(|child| match &scope_graph[*child] {
                        Node::Item { ident, .. } => **ident == name_string,
                        Node::Mod {
                            item_mod: ItemMod { ident, .. },
                            ..
                        } => *ident == name_string,
                        Node::Use { .. } => {
                            warn!("uses aren't recursively traced (yet)");
                            false
                        }
                        _ => false,
                    });
                child
            };
            if let Node::Use { imports, .. } = &mut scope_graph[dest] {
                imports.entry(scope).or_default().push(UseType::Rename {
                    rename,
                    index: found_index.expect("uses that aren't found are an error"),
                })
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
