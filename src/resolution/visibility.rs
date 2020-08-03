//! Note that the parent(s) iteration is overkill so no unwrap()s are done.
//! Ideally, the scope graph is a tree and there cannot be multiple parents.

use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{spanned::Spanned, Visibility};

use super::{Node, ScopeGraph};
use crate::error::{
    IncorrectVisibilityError, ResolutionError, SpecialIdentNotAtStartOfPathError,
    TooManySupersError, UnresolvedItemError,
};
use crate::find_file::File;

/// If a node overrides its own visibility, make a note of it in the parent node(s) as an "export".
/// TODO: pub in enum: "not allowed because it is implied"
pub fn apply_visibility<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
    use syn::Item::*;
    use syn::*;
    let vis_and_file = match &scope_graph[node] {
        Node::Var { item, file, .. }
        | Node::Macro { item, file, .. }
        | Node::Type { item, file, .. } => match item {
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
        Node::Fn {
            item_fn: ItemFn { vis, .. },
            file,
            ..
        } => Some((vis, file.clone())),
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
        Node::Root { .. } | Node::Impl { .. } => None,
    };

    if let Some((vis, file)) = vis_and_file {
        use Visibility::*;
        match vis {
            Public(_) => apply_visibility_pub(scope_graph, node),
            Crate(_) => apply_visibility_crate(scope_graph, node),
            Restricted(r) => {
                apply_visibility_in(scope_graph, node, &file, r.in_token.is_some(), &r.path)
            }
            Inherited => Ok(()),
        }
    } else {
        Ok(())
    }
}

fn apply_visibility_in<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
    file: &Rc<File>,
    has_in_token: bool,
    path: &syn::Path,
) -> Result<(), ResolutionError> {
    if !has_in_token && path.segments.len() > 1 {
        todo!("wacky pub")
    }
    if path.leading_colon.is_some() {
        todo!("2015 syntax unsupported")
    }
    let parents = collect_parents(scope_graph, node);

    let first_segment = path
        .segments
        .first()
        .expect("error if no first segment, this should never happen");
    let mut export_entries: Vec<NodeIndex> = if first_segment.ident == "crate" {
        collect_roots(scope_graph, node)
    } else if first_segment.ident == "super" {
        parents
            .iter()
            .map(|parent| collect_parents(scope_graph, *parent))
            .flatten()
            .collect()
    } else if first_segment.ident == "self" {
        if path.segments.len() > 1 {
            todo!("in must be an ancestor scope");
        }
        return Ok(());
    } else {
        todo!("error if not crate/super/self")
    };
    for (prev_segment, segment) in path.segments.iter().zip(path.segments.iter().skip(1)) {
        if segment.ident == "crate"
            || segment.ident == "self"
            || (prev_segment.ident != "super" && segment.ident == "super")
        {
            return Err(SpecialIdentNotAtStartOfPathError {
                file: file.clone(),
                path_ident: segment.ident.clone(),
            }
            .into());
        }
        let mut next_export_entries = Vec::with_capacity(export_entries.len());

        if segment.ident == "super" {
            for entry in &export_entries {
                let mut parents = collect_parents(scope_graph, *entry);
                if parents.is_empty() {
                    return Err(TooManySupersError {
                        file: file.clone(),
                        ident: segment.ident.clone(),
                    }
                    .into());
                }
                next_export_entries.append(&mut parents);
            }
        } else {
            let segment_ident_string = segment.ident.to_string();
            for entry in &export_entries {
                let mut next = scope_graph
                    .neighbors(*entry)
                    .filter(|child| match &scope_graph[*child] {
                        Node::Mod { item_mod, .. } => item_mod.ident == segment_ident_string,
                        Node::Root {
                            name: Some(name), ..
                        } => *name == segment_ident_string,
                        _ => false,
                    })
                    .collect::<Vec<NodeIndex>>();
                if next.is_empty() {
                    return Err(UnresolvedItemError {
                        file: file.clone(),
                        previous_idents: path
                            .segments
                            .iter()
                            .map(|segment| segment.ident.clone())
                            .collect(),
                        unresolved_ident: segment.ident.clone(),
                        has_leading_colon: false,
                    }
                    .into());
                }
                next_export_entries.append(&mut next);
            }
        }
        export_entries = next_export_entries;
    }

    // TODO: check ancestry of the exports & that it is not violating publicity (its containers are visible where it is exported)
    for parent in parents {
        match &mut scope_graph[parent] {
            // export node to grandparents
            Node::Mod { exports, .. } => exports.entry(node).or_default().extend(&export_entries),
            // export node to root
            Node::Root { exports, .. } => exports.push(node),
            other => {
                error!("parent is not a mod or root {:?}", other);
            }
        }
    }

    Ok(())
}

fn apply_visibility_pub<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
    let parents = collect_parents(scope_graph, node);
    let grandparents: Vec<NodeIndex> = parents
        .iter()
        .map(|parent| collect_parents(scope_graph, *parent))
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

fn collect_parents<'ast>(scope_graph: &mut ScopeGraph<'ast>, node: NodeIndex) -> Vec<NodeIndex> {
    scope_graph
        .neighbors_directed(node, Direction::Incoming)
        .collect()
}

fn collect_roots<'ast>(scope_graph: &mut ScopeGraph<'ast>, node: NodeIndex) -> Vec<NodeIndex> {
    let mut roots: Vec<NodeIndex> = vec![];
    let mut level: Vec<NodeIndex> = vec![];
    let mut next: Vec<NodeIndex> = vec![node];
    while !next.is_empty() {
        level.append(&mut next);
        level.iter().for_each(|n| {
            if let Node::Root { .. } = scope_graph[*n] {
                roots.push(*n);
            } else {
                next.append(&mut collect_parents(scope_graph, *n));
            }
        });
    }
    roots
}

/// https://github.com/rust-lang/rust/issues/53120
/// TODO: check validity of a crate-level pub if this isn't a crate
/// Bottom-up BFS
fn apply_visibility_crate<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
    let parents: Vec<NodeIndex> = collect_parents(scope_graph, node);
    let roots = collect_roots(scope_graph, node);
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
