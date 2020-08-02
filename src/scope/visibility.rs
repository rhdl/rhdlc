//! Note that the parent(s) iteration is overkill so no unwrap()s are done.
//! Ideally, the scope graph is a tree and there cannot be multiple parents.

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::Visibility;

use super::{Node, ScopeGraph};

/// If a node overrides its own visibility, make a note of it in the parent node(s) as an "export".
/// TODO: pub in enum: "not allowed because it is implied"
pub fn apply_visibility<'ast>(scope_graph: &mut ScopeGraph<'ast>, node: NodeIndex) {
    use syn::Item::*;
    use syn::*;
    let vis = match scope_graph[node] {
        Node::Item { item, .. } => match item {
            ExternCrate(ItemExternCrate { vis, .. })
            | Type(ItemType { vis, .. })
            | Static(ItemStatic { vis, .. })
            | Const(ItemConst { vis, .. })
            | Fn(ItemFn {
                sig: Signature { .. },
                vis,
                ..
            })
            | Macro2(ItemMacro2 { vis, .. })
            | Struct(ItemStruct { vis, .. })
            | Enum(ItemEnum { vis, .. })
            | Trait(ItemTrait { vis, .. })
            | TraitAlias(ItemTraitAlias { vis, .. })
            | Union(ItemUnion { vis, .. }) => Some(vis),
            _ => None,
        },
        Node::Mod {
            item_mod: ItemMod { vis, .. },
            ..
        } => Some(vis),
        Node::Use {
            item_use: ItemUse { vis, .. },
            ..
        } => Some(vis),
        _ => None,
    };

    if let Some(vis) = vis {
        use Visibility::*;
        match vis {
            Public(_) => apply_visibility_pub(scope_graph, node),
            Crate(_) => apply_visibility_crate(scope_graph, node),
            Restricted(r) => {
                if let Some(_in) = r.in_token {
                    todo!("restricted visibility in paths is not implemented yet");
                // Edition Differences: Starting with the 2018 edition, paths for pub(in path) must start with crate, self, or super. The 2015 edition may also use paths starting with :: or modules from the crate root.
                } else {
                    match r
                        .path
                        .get_ident()
                        .map(|ident| ident.to_string())
                        .expect("error if the path is not an ident")
                        .as_str()
                    {
                        // No-op
                        "self" => {}
                        // Same as crate pub
                        "crate" => apply_visibility_crate(scope_graph, node),
                        // Same as pub
                        "super" => apply_visibility_pub(scope_graph, node),
                        _ => todo!("error if none of the above"),
                    }
                }
            }
            Inherited => {}
        }
    }
}

fn apply_visibility_pub<'ast>(scope_graph: &mut ScopeGraph<'ast>, node: NodeIndex) {
    let parents: Vec<NodeIndex> = scope_graph
        .neighbors_directed(node, Direction::Incoming)
        .collect();
    let grandparents: Vec<NodeIndex> = parents
        .iter()
        .map(|parent| scope_graph.neighbors_directed(*parent, Direction::Incoming))
        .flatten()
        .collect();
    for parent in parents {
        match &mut scope_graph[parent] {
            // export node to grandparents
            Node::Mod { exports, .. } => exports.entry(node).or_default().extend(&grandparents),
            // export node to root
            Node::Root { exports, .. } => exports.push(node),
            other => {
                error!("parent is not a mod or root {:?}", other);
            }
        }
    }
}

/// https://github.com/rust-lang/rust/issues/53120
/// TODO: check validity of a crate-level pub if this isn't a crate
/// Bottom-up BFS
fn apply_visibility_crate<'ast>(scope_graph: &mut ScopeGraph<'ast>, node: NodeIndex) {
    let parents: Vec<NodeIndex> = scope_graph
        .neighbors_directed(node, Direction::Incoming)
        .collect();
    let roots = {
        let mut roots: Vec<NodeIndex> = vec![];
        let mut level: Vec<NodeIndex> = vec![];
        let mut next: Vec<NodeIndex> = vec![node];
        while !next.is_empty() {
            level.append(&mut next);
            level.iter().for_each(|n| {
                if let Node::Root { .. } = scope_graph[*n] {
                    roots.push(*n);
                } else {
                    next.extend(scope_graph.neighbors_directed(*n, Direction::Incoming));
                }
            });
        }
        roots
    };
    for parent in parents {
        match &mut scope_graph[parent] {
            // export node to roots
            Node::Mod { exports, .. } => exports.entry(node).or_default().extend(&roots),
            // export node to root
            Node::Root { exports, .. } => exports.push(node),
            other => {
                error!("parent is not a mod or root {:?}", other);
            }
        }
    }
}

pub fn is_target_visible<'ast>(
    scope_graph: &mut ScopeGraph,
    scope: NodeIndex,
    target: NodeIndex,
) -> Option<bool> {
    let scope_parent = scope_graph
        .neighbors_directed(scope, Direction::Incoming)
        .next()
        .unwrap();
    let target_parent = scope_graph
        .neighbors_directed(target, Direction::Incoming)
        .next()
        .unwrap();
    match &scope_graph[target_parent] {
        Node::Root { exports, .. } => Some(exports.contains(&target)),
        Node::Mod { exports, .. } => Some(
            exports
                .get(&target)
                .map(|exports| exports.contains(&scope_parent))
                .unwrap_or_default(),
        ),
        _ => None,
    }
}
