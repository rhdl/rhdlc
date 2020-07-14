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
///
/// Possible out of scope violations, in order of precedence:
/// * Publicity
///     * if it's owned by the user, offer that they need to pub it
///     * crates are not owned by user, but any source in the local tree is
/// * Out of scope: exists but can't use
///     * fix by adding a use
///         * might not work if out of scope is private
/// * Not Found
///     * look for similar names and offer a "did you mean"
///         * use [strsim](https://docs.rs/strsim/0.10.0/strsim/) for Ident similarity
///         * smart guess by type (fn, struct, var, mod, etc.)
///     * fall back all the way to "not found" if nothing is similar

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
