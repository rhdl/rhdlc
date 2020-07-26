use std::collections::HashMap;

use petgraph::{graph::NodeIndex, Direction};
use syn::{ItemUse, UseName, UseRename, UseTree};

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
    destination: NodeIndex,
    scope: NodeIndex,
    tree: &UseTree,
) {
    use syn::UseTree::*;
    match &mut scope_graph[destination] {
        Node::Use { imports, .. } => {
            match tree {
                Path(path) => {
                    if path.ident == "super" {
                        let parent = scope_graph
                            .neighbors_directed(scope, Direction::Incoming)
                            .next()
                            .expect("todo, going beyond the root is an error");
                        trace_use(scope_graph, destination, parent, &path.tree);
                    } else {
                        let child = scope_graph.neighbors(scope).find(|child| {
                            if let Node::Mod { item_mod, .. } = scope_graph[*child] {
                                item_mod.ident.to_string() == path.ident.to_string()
                            } else {
                                false
                            }
                        });
                        trace_use(
                            scope_graph,
                            destination,
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
                Glob(glob) => imports
                    .entry(scope)
                    .or_default()
                    .push(UseType::Glob { scope }),
                Group(group) => group
                    .items
                    .iter()
                    .for_each(|tree| trace_use(scope_graph, destination, scope, tree)),
            }
        }
        _ => {}
    }
}
