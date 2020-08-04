//! Note that the parent(s) iteration is overkill so no unwrap()s are done.
//! Ideally, the scope graph is a tree and there cannot be multiple parents.

use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, Direction};
use syn::{spanned::Spanned, Visibility};

use super::{Node, ScopeGraph};
use crate::error::{
    IncorrectVisibilityError, NonAncestralError, ResolutionError,
    SpecialIdentNotAtStartOfPathError, TooManySupersError, UnresolvedItemError, UnsupportedError,
    VisibilityError,
};
use crate::find_file::File;

/// If a node overrides its own visibility, make a note of it in the parent node(s) as an "export".
/// TODO: pub in enum: "not allowed because it is implied"
/// claim: parent scopes are always already visited so no need for recursive behavior
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
        return Err(UnsupportedError {
            file: file.clone(),
            span: path.span(),
            reason: "RHDL does not recognize this path, it should be pub(in path)",
        }
        .into());
    }
    if path.leading_colon.is_some() {
        return Err(UnsupportedError {
            file: file.clone(),
            span: path.leading_colon.span(),
            reason: "Beginning with the 2018 edition of Rust, paths for pub(in path) must start with crate, self, or super."
        }.into());
    }
    let node_parent = first_parent(scope_graph, node).unwrap();
    let ancestry = build_ancestry(scope_graph, node);

    let first_segment = path
        .segments
        .first()
        .expect("error if no first segment, this should never happen");
    let mut export_dest = if first_segment.ident == "crate" {
        *build_ancestry(scope_graph, node).last().unwrap()
    } else if first_segment.ident == "super" {
        if let Some(grandparent) = first_parent(scope_graph, node_parent) {
            grandparent
        } else {
            return Err(TooManySupersError {
                file: file.clone(),
                ident: first_segment.ident.clone(),
            }
            .into());
        }
    } else if first_segment.ident == "self" {
        if path.segments.len() > 1 {
            return Err(NonAncestralError {
                file: file.clone(),
                segment_ident: first_segment.ident.clone(),
            }
            .into());
        }
        return Ok(());
    } else {
        return Err(IncorrectVisibilityError {
            file: file.clone(),
            vis_span: first_segment.ident.span(),
        }
        .into());
    };

    for (i, (prev_segment, segment)) in path
        .segments
        .iter()
        .zip(path.segments.iter().skip(1))
        .enumerate()
    {
        if segment.ident == "crate"
            || segment.ident == "self"
            || (prev_segment.ident != "super" && segment.ident == "super")
        {
            return Err(SpecialIdentNotAtStartOfPathError {
                file: file.clone(),
                path_ident: segment.ident.clone(),
            }
            .into());
        } else if prev_segment.ident == "super" && segment.ident != "super" {
            return Err(NonAncestralError {
                file: file.clone(),
                segment_ident: first_segment.ident.clone(),
            }
            .into());
        }

        export_dest = if segment.ident == "super" {
            if let Some(export_dest_parent) = first_parent(scope_graph, export_dest) {
                if !is_target_visible(scope_graph, export_dest_parent, node_parent).unwrap() {
                    return Err(VisibilityError {
                        name_file: file.clone(),
                        name_ident: segment.ident.clone(),
                    }
                    .into());
                }
                export_dest_parent
            } else {
                return Err(TooManySupersError {
                    file: file.clone(),
                    ident: segment.ident.clone(),
                }
                .into());
            }
        } else {
            let segment_ident_string = segment.ident.to_string();
            let export_dest_children: Vec<NodeIndex> = scope_graph
                .neighbors(export_dest)
                .filter(|child| match &scope_graph[*child] {
                    Node::Mod { item_mod, .. } => item_mod.ident == segment_ident_string,
                    Node::Root {
                        name: Some(name), ..
                    } => *name == segment_ident_string,
                    _ => false,
                })
                .collect();
            if export_dest_children.is_empty() {
                return Err(UnresolvedItemError {
                    file: file.clone(),
                    previous_idents: path
                        .segments
                        .iter()
                        .take(i + 1)
                        .map(|s| s.ident.clone())
                        .collect(),
                    unresolved_ident: segment.ident.clone(),
                    has_leading_colon: false,
                }
                .into());
            } else if let Some(export_dest_child) = export_dest_children
                .iter()
                .find(|child| ancestry.contains(child))
            {
                if !is_target_visible(scope_graph, *export_dest_child, node_parent).unwrap() {
                    return Err(VisibilityError {
                        name_file: file.clone(),
                        name_ident: segment.ident.clone(),
                    }
                    .into());
                }
                *export_dest_child
            } else {
                return Err(NonAncestralError {
                    file: file.clone(),
                    segment_ident: first_segment.ident.clone(),
                }
                .into());
            }
        };
    }

    match &mut scope_graph[node_parent] {
        // export node to grandparents
        Node::Mod { exports, .. } => {
            exports.insert(node, export_dest);
        }
        // export node to root
        Node::Root { exports, .. } => exports.push(node),
        other => {
            error!("parent is not a mod or root {:?}", other);
        }
    }

    Ok(())
}

fn apply_visibility_pub<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
    let parent = first_parent(scope_graph, node).unwrap();
    let grandparent = first_parent(scope_graph, parent);
    match &mut scope_graph[parent] {
        // export node to grandparent (guaranteed that there will be one)
        Node::Mod { exports, .. } => {
            exports.insert(node, grandparent.unwrap());
        }
        // export node beyond root
        Node::Root { exports, .. } => exports.push(node),
        other => error!("parent is not a mod or root {:?}", other),
    }
    Ok(())
}

fn first_parent<'ast>(scope_graph: &mut ScopeGraph<'ast>, node: NodeIndex) -> Option<NodeIndex> {
    scope_graph
        .neighbors_directed(node, Direction::Incoming)
        .next()
}

fn build_ancestry<'ast>(scope_graph: &mut ScopeGraph<'ast>, node: NodeIndex) -> Vec<NodeIndex> {
    let mut prev_parent = node;
    let mut ancestry = vec![];
    while let Some(parent) = first_parent(scope_graph, prev_parent) {
        ancestry.push(parent);
        prev_parent = parent;
    }
    ancestry
}

/// TODO: https://github.com/rust-lang/rust/issues/53120
fn apply_visibility_crate<'ast>(
    scope_graph: &mut ScopeGraph<'ast>,
    node: NodeIndex,
) -> Result<(), ResolutionError> {
    let parent = first_parent(scope_graph, node).unwrap();
    let root = *build_ancestry(scope_graph, node).last().unwrap();
    match &mut scope_graph[parent] {
        Node::Mod { exports, .. } => {
            exports.insert(node, root);
        }
        Node::Root { exports, .. } => {
            // NOP
        }
        other => error!("parent is not a mod or root {:?}", other),
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
    target: NodeIndex,
) -> Option<bool> {
    let target_parent = if let Some(target_parent) = first_parent(scope_graph, target) {
        target_parent
    } else {
        // this is necessarily a root
        return Some(true);
    };
    // self
    if target_parent == target {
        return Some(true);
    }
    // super
    if first_parent(scope_graph, target_parent)
        .map(|g| g == target)
        .unwrap_or_default()
    {
        return Some(true);
    }
    let dest_ancestry = build_ancestry(scope_graph, dest);
    // targets in an ancestor of the use are always visible
    if dest_ancestry.contains(&target_parent) {
        return Some(true);
    }

    let target_parent_ancestry = build_ancestry(scope_graph, target_parent);

    match &scope_graph[target_parent] {
        // same root || target explicitly exported outside of crate
        Node::Root { exports, .. } => {
            Some(dest_ancestry.contains(&target_parent) || exports.contains(&target))
        }
        // explicitly visible to any dest ancestor or target parent ancestor
        Node::Mod { exports, .. } => Some(
            exports
                .get(&target)
                .map(|export_dest| {
                    dest_ancestry.contains(export_dest)
                        || target_parent_ancestry.contains(export_dest)
                })
                .unwrap_or_default(),
        ),
        _ => None,
    }
}
