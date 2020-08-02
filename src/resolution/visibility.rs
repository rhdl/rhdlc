//! Note that the parent(s) iteration is overkill so no unwrap()s are done.
//! Ideally, the scope graph is a tree and there cannot be multiple parents.

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{spanned::Spanned, Visibility};

use super::{Node, ScopeGraph};
use crate::error::{IncorrectVisibilityError, ResolutionError};

/// If a node overrides its own visibility, make a note of it in the parent node(s) as an "export".
/// TODO: pub in enum: "not allowed because it is implied"
pub fn apply_visibility<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
    use syn::Item::*;
    use syn::*;
    let vis_and_file = match &scope_graph[node] {
        Node::Item { item, file, .. } => match item {
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
            | Union(ItemUnion { vis, .. }) => Some((vis, file.clone())),
            _ => None,
        },
        Node::Mod {
            item_mod: ItemMod { vis, .. },
            file,
            ..
        } => Some((vis, file.clone())),
        Node::Use {
            item_use: ItemUse { vis, .. },
            file,
            ..
        } => Some((vis, file.clone())),
        _ => None,
    };

    if let Some((vis, file)) = vis_and_file {
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
                        "self" => Ok(()),
                        // Same as crate pub
                        "crate" => apply_visibility_crate(scope_graph, node),
                        // Same as pub
                        "super" => apply_visibility_pub(scope_graph, node),
                        _other => Err(IncorrectVisibilityError {
                            file: file,
                            vis_span: r.span(),
                        }
                        .into()),
                    }
                }
            }
            Inherited => Ok(()),
        }
    } else {
        Ok(())
    }
}

fn apply_visibility_pub<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
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
    Ok(())
}

/// https://github.com/rust-lang/rust/issues/53120
/// TODO: check validity of a crate-level pub if this isn't a crate
/// Bottom-up BFS
fn apply_visibility_crate<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
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
    Ok(())
}

/// Target is always a child of scope
/// Check if the target is visible in the context of the original use
/// Possibilities:
/// * dest_parent == target_parent (self, always visible)
/// * target_parent == root && dest_root == root (crate, always visible)
/// * target == target_parent (use a::{self, b}, always visible)
/// * target is actually a parent of target_parent (use super::super::b, always visible)
/// * target_parent is a parent of dest_parent (use super::a, always visible)
pub fn is_target_visible<'ast>(
    scope_graph: &mut ScopeGraph,
    dest: NodeIndex,
    target_parent: NodeIndex,
    target: NodeIndex,
) -> Option<bool> {
    let dest_parent = scope_graph
        .neighbors_directed(dest, Direction::Incoming)
        .next()
        .unwrap();
    if dest_parent == target_parent
        || target == target_parent
        || scope_graph
            .neighbors_directed(target_parent, Direction::Incoming)
            .any(|n| n == target)
        || scope_graph
            .neighbors_directed(dest_parent, Direction::Incoming)
            .any(|n| n == target_parent)
    {
        return Some(true);
    }
    dbg!(dest_parent, dest, target_parent, target);
    let target_grandparent = scope_graph
        .neighbors_directed(target_parent, Direction::Incoming)
        .next();

    match &scope_graph[target_parent] {
        Node::Root { exports, .. } => {
            let mut dest_root = dest_parent;
            while let Some(next_dest_parent) = scope_graph
                .neighbors_directed(dest_root, Direction::Incoming)
                .next()
            {
                dest_root = next_dest_parent;
            }
            Some(target_parent == dest_root || exports.contains(&target))
        }
        Node::Mod { exports, .. } => Some(
            exports
                .get(&target)
                .map(|exports| {
                    target_grandparent
                        .as_ref()
                        .map(|tgp| exports.contains(tgp))
                        .unwrap_or_default()
                        || exports.contains(&dest_parent)
                })
                .unwrap_or_default(),
        ),
        _ => None,
    }
}
