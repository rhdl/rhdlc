use petgraph::{graph::NodeIndex, Direction};
use syn::{UseName, UseRename, UseTree};

use super::{Node, ScopeGraph};

#[derive(Debug)]
pub enum UseType {
    /// All children from the root/mod are included
    Name {
        name: UseName,
        index: NodeIndex,
    },
    Glob {
        scope: NodeIndex,
    },
    Rename {
        rename: UseRename,
        index: NodeIndex,
    },
}
/// Trace usages
/// TODOs:
/// * Handle "self" properly
///     * self in a group
///     * self at the beginning of a path (anywhere else is technically an error since it's a nop)
/// * Disambiguate between crate imports and local module imports
///     * A beginning :: explicitly refers to the global scope
///     * A beginning `self` explicitly refers to the local scope
///     * A beginning `super` explicitly refers to the parent scope
///     * A beginning `crate` explicitly refers to the root scope
///     * Any other word is implicitly the global or local scope
///         * Error if there is a root with the same name as a module in the local scope.
///             * Requires explicit disambiguation
/// * Check scope visibility
/// * Global imports
///     * Roots need names: `crate` is "this" root, vs. any other identifier
pub fn trace_use<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    dest: NodeIndex,
    // Begins at the root
    scope: NodeIndex,
    tree: &UseTree,
) {
    use syn::UseTree::*;
    if let Node::Use {
        imports, item_use, ..
    } = &mut scope_graph[dest]
    {
        // Is this the tracing entry point?
        let is_entry = tree == &item_use.tree;
        match tree {
            Path(path) => {
                if path.ident == "self" {
                    if !is_entry {
                        todo!("a `self` that isn't at the beginning of a path is an error");
                    }
                    let use_parent = scope_graph
                        .neighbors_directed(dest, Direction::Incoming)
                        .next()
                        .unwrap();
                    trace_use(scope_graph, dest, use_parent, &path.tree);
                } else if path.ident == "super" {
                    if !is_entry {
                        todo!("a `super` that isn't at the beginning of a path is an error");
                    }
                    let use_parent = scope_graph
                        .neighbors_directed(dest, Direction::Incoming)
                        .next()
                        .unwrap();
                    let use_grandparent = scope_graph
                        .neighbors_directed(use_parent, Direction::Incoming)
                        .next()
                        .expect("todo, going beyond the root is an error");
                    trace_use(scope_graph, dest, use_grandparent, &path.tree);
                } else if path.ident == "crate" {
                    if !is_entry {
                        todo!("a `crate` that isn't at the beginning of a path is an error");
                    }
                    todo!("crate-level import not handled");
                } else {
                    // TODO: global vs local disambiguation
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
            }
            Name(name) => {
                imports.entry(scope).or_default().push(UseType::Name {
                    name: name.clone(),
                    // this is wrong, placeholder
                    index: scope,
                })
            }
            Rename(rename) => imports.entry(scope).or_default().push(UseType::Rename {
                rename: rename.clone(),
                // this is wrong, placeholder
                index: scope,
            }),
            Glob(_) => imports
                .entry(scope)
                .or_default()
                .push(UseType::Glob { scope }),
            Group(group) => group
                .items
                .iter()
                .for_each(|tree| trace_use(scope_graph, dest, scope, tree)),
        }
    }
}
