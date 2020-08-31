use std::rc::Rc;

use syn::{spanned::Spanned, Visibility};

use super::{ResolutionGraph, ResolutionIndex};
use crate::error::{
    IncorrectVisibilityError, ItemHint, NonAncestralError, ResolutionError, ScopeVisibilityError,
    SpecialIdentNotAtStartOfPathError, TooManySupersError, UnresolvedItemError, UnsupportedError,
};
use crate::find_file::File;

/// If a node overrides its own visibility, make a note of it in the parent node(s) as an "export".
/// TODO: pub in enum: "not allowed because it is implied"
/// claim: parent scopes are always already visited so no need for recursive behavior
pub fn apply_visibility<'ast>(
    resolution_graph: &mut ResolutionGraph<'ast>,
    node: ResolutionIndex,
) -> Result<(), ResolutionError> {
    let export_dest = if let Some(vis) = resolution_graph.inner[node].visibility() {
        use Visibility::*;
        let file = resolution_graph.file(node);
        match vis {
            Public(_) => apply_visibility_pub(resolution_graph, node),
            Crate(_) => apply_visibility_crate(resolution_graph, node),
            Restricted(r) => {
                apply_visibility_in(resolution_graph, node, file, r.in_token.is_some(), &r.path)
            }
            Inherited => Ok(Some(resolution_graph.inner[node].parent().unwrap())),
        }?
    } else {
        return Ok(());
    };
    resolution_graph.exports.insert(node, export_dest);
    Ok(())
}

fn apply_visibility_in<'ast>(
    resolution_graph: &ResolutionGraph<'ast>,
    node: ResolutionIndex,
    file: Rc<File>,
    has_in_token: bool,
    path: &syn::Path,
) -> Result<Option<ResolutionIndex>, ResolutionError> {
    if !has_in_token && path.segments.len() > 1 {
        return Err(UnsupportedError {
            file,
            span: path.span(),
            reason: "RHDL does not recognize this path, it should be pub(in path)",
        }
        .into());
    }
    if path.leading_colon.is_some() {
        return Err(UnsupportedError {
            file,
            span: path.leading_colon.span(),
            reason: "Beginning with the 2018 edition of Rust, paths for pub(in path) must start with crate, self, or super."
        }.into());
    }
    let node_parent = resolution_graph.inner[node].parent().unwrap();
    let ancestry = build_ancestry(resolution_graph, node);

    let first_segment = path
        .segments
        .first()
        .expect("error if no first segment, this should never happen");
    let mut export_dest = if first_segment.ident == "crate" {
        *build_ancestry(resolution_graph, node).last().unwrap()
    } else if first_segment.ident == "super" {
        if let Some(grandparent) = resolution_graph.inner[node_parent].parent() {
            grandparent
        } else {
            return Err(TooManySupersError {
                file,
                ident: first_segment.ident.clone(),
            }
            .into());
        }
    } else if first_segment.ident == "self" {
        if path.segments.len() > 1 {
            return Err(NonAncestralError {
                file,
                segment_ident: first_segment.ident.clone(),
                prev_segment_ident: None,
            }
            .into());
        }
        return Ok(Some(node_parent));
    } else {
        return Err(IncorrectVisibilityError {
            file,
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
                file,
                path_ident: segment.ident.clone(),
            }
            .into());
        } else if prev_segment.ident == "super" && segment.ident != "super" {
            return Err(NonAncestralError {
                file,
                segment_ident: segment.ident.clone(),
                prev_segment_ident: Some(prev_segment.ident.clone()),
            }
            .into());
        }

        export_dest = if segment.ident == "super" {
            if let Some(export_dest_parent) = resolution_graph.inner[export_dest].parent() {
                if !is_target_visible(resolution_graph, export_dest_parent, node_parent) {
                    return Err(ScopeVisibilityError {
                        file,
                        ident: segment.ident.clone(),
                        hint: if resolution_graph.inner[export_dest_parent]
                            .parent()
                            .is_none()
                        {
                            ItemHint::InternalNamedRootScope
                        } else {
                            ItemHint::InternalNamedChildScope
                        },
                    }
                    .into());
                }
                export_dest_parent
            } else {
                return Err(TooManySupersError {
                    file,
                    ident: segment.ident.clone(),
                }
                .into());
            }
        } else {
            let export_dest_children: Vec<ResolutionIndex> = resolution_graph.inner[export_dest]
                .children()
                .and_then(|children| children.get(&Some(&segment.ident)))
                .map(|named_children| {
                    named_children
                        .iter()
                        .filter(|child| resolution_graph.inner[**child].is_valid_use_path_segment())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if export_dest_children.is_empty() {
                return Err(UnresolvedItemError {
                    file,
                    previous_ident: path
                        .segments
                        .iter()
                        .skip(i)
                        .next()
                        .map(|seg| seg.ident.clone()),
                    unresolved_ident: segment.ident.clone(),
                    hint: ItemHint::InternalNamedChildScope,
                }
                .into());
            } else if let Some(export_dest_child) = export_dest_children
                .iter()
                .find(|child| ancestry.contains(child))
            {
                if !is_target_visible(resolution_graph, *export_dest_child, node_parent) {
                    return Err(ScopeVisibilityError {
                        file,
                        ident: segment.ident.clone(),
                        hint: ItemHint::InternalNamedChildScope,
                    }
                    .into());
                }
                *export_dest_child
            } else {
                return Err(NonAncestralError {
                    file,
                    segment_ident: segment.ident.clone(),
                    prev_segment_ident: Some(prev_segment.ident.clone()),
                }
                .into());
            }
        };
    }
    // TODO: are beyond root exports for a given path possible?
    Ok(Some(export_dest))
}

fn apply_visibility_pub<'ast>(
    resolution_graph: &ResolutionGraph<'ast>,
    node: ResolutionIndex,
) -> Result<Option<ResolutionIndex>, ResolutionError> {
    let parent = resolution_graph.inner[node].parent().unwrap();
    let grandparent = resolution_graph.inner[parent].parent();
    Ok(grandparent)
}

fn build_ancestry<'ast>(
    resolution_graph: &ResolutionGraph<'ast>,
    node: ResolutionIndex,
) -> Vec<ResolutionIndex> {
    let mut prev_parent = node;
    let mut ancestry = vec![];
    while let Some(parent) = resolution_graph.inner[prev_parent].parent() {
        ancestry.push(parent);
        prev_parent = parent;
    }
    ancestry
}

/// TODO: https://github.com/rust-lang/rust/issues/53120
fn apply_visibility_crate<'ast>(
    resolution_graph: &ResolutionGraph<'ast>,
    node: ResolutionIndex,
) -> Result<Option<ResolutionIndex>, ResolutionError> {
    let root = *build_ancestry(resolution_graph, node).last().unwrap();
    Ok(Some(root))
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
    resolution_graph: &ResolutionGraph<'ast>,
    dest: ResolutionIndex,
    target: ResolutionIndex,
) -> bool {
    let target_parent = if let Some(target_parent) = resolution_graph.inner[target].parent() {
        target_parent
    } else {
        // this is necessarily a root
        return true;
    };
    // self
    if target_parent == target {
        return true;
    }
    // super
    if resolution_graph.inner[target_parent]
        .parent()
        .map(|g| g == target)
        .unwrap_or_default()
    {
        return true;
    }
    let dest_ancestry = build_ancestry(resolution_graph, dest);
    // targets in an ancestor of the use are always visible
    if dest_ancestry.contains(&target_parent) {
        return true;
    }

    let target_parent_ancestry = build_ancestry(resolution_graph, target_parent);
    resolution_graph
        .exports
        .get(&target)
        .map(|export_dest_opt| {
            // exported to dest/dest_ancestry, out of the crate, or to target grandparent
            export_dest_opt
                .map(|export_dest| {
                    target_parent_ancestry.contains(&export_dest)
                        || dest_ancestry.contains(&export_dest)
                })
                .unwrap_or(true)
        })
        .unwrap_or_default()
}
