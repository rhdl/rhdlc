/// Build a scope digraph.
/// Nodes are items with visibility
/// Directional edges connect nodes to the places where they are visible, i.e.:
/// * fn in mod
/// * struct in mod
/// * struct fields
///     * fields do have visibility
///     * they aren't items, but...
///     * struct effectively acts as a node containing field nodes & conditional edges to them
/// * pub fn in mod visible in the parent scope
///     * if parent scope is fully visible to another scope, it is 
/// * special pub types (pub(crate), etc.)
/// * type aliases
/// * `use super::ABC as XYZ;`
///
/// Possible scope violations, in order of precedence:
/// * In scope, but not allowed to be
///     * Not public
///         * if it's owned by the user, suggest that they should pub it
///         * crates are not owned by user, but any source in the local tree is
///     * Name conflict
///         * can't have two structs in scope with the same name
/// * Out of scope
///     * Exists, but not in scope
///         * fix by adding a use
///             * find disconnected nodes with the same name (expensive?)
///             * see if it's possible to create an edge (reachable)
///                 * don't offer this if it isn't. if it's owned by user it's private and you can could pub it.
///     * Not Found
///         * look for similarly named disconnected nodes and offer a "did you mean"
///             * use [strsim](https://docs.rs/strsim/0.10.0/strsim/) for Ident similarity
///             * heuristic guess by type (fn, struct, var, mod, etc.)
///         * fall back all the way to "not found" if nothing is similar

use petgraph::{Graph, Directed, graph::NodeIndex};
use syn::Item;
use syn::visit_mut::VisitMut;

type ScopeGraph = Graph<Item, (), Directed, usize>;

/// Find all nodes in the scope graph
/// Will also build the default set of scope edges
#[derive(Default, Debug)]
struct NodeScoper {
    graph: ScopeGraph,
    enclosing: Option<NodeIndex<usize>>
}

#[derive(Debug)]
struct EdgeScoper<'a> {
    graph: &'a ScopeGraph
}


// impl VisitMut for NodeScoper {
//     /// If the code is in a mod.rhdl file, there could be more modules that need to be recursively resolved.
//     fn visit_item_mod_mut(&mut self, item_mod: &mut ItemMod) {
//     }
// }
