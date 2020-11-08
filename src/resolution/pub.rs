use rhdl::ast::{Spanned, Vis, VisRestricted};

use super::{ResolutionGraph, ResolutionIndex};
use crate::error::*;
use crate::find_file::FileId;

/// If a node overrides its own visibility, make a note of it in the parent node(s) as an "export".
/// TODO: pub in enum: "not allowed because it is implied"
/// claim: parent scopes are always already visited so no need for recursive behavior
pub fn apply_visibility<'ast>(
    resolution_graph: &mut ResolutionGraph<'ast>,
    node: ResolutionIndex,
) -> Result<(), Diagnostic> {
    let export_dest = if let Some(vis) = resolution_graph.inner[node].visibility() {
        use Vis::*;
        let file = resolution_graph.file(node);
        match vis {
            Pub(_) => apply_visibility_pub(resolution_graph, node),
            Crate(_) => apply_visibility_crate(resolution_graph, node),
            Super(_) => apply_visibility_pub(resolution_graph, node),
            Restricted(r) => apply_visibility_in(resolution_graph, node, file, r),
            ExplicitInherited(_) => Ok(Some(resolution_graph.inner[node].parent().unwrap())),
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
    file: FileId,
    r: &'ast VisRestricted,
) -> Result<Option<ResolutionIndex>, Diagnostic> {
    if let Some(leading_sep) = &r.path.leading_sep {
        return Err(incorrect_visibility_restriction(file, leading_sep.span()));
    }
    let node_parent = resolution_graph.inner[node].parent().unwrap();
    let ancestry = build_ancestry(resolution_graph, node);

    let first_segment = r
        .path
        .segments
        .first()
        .expect("error if no first segment, this should never happen");
    let mut export_dest = if first_segment == "crate" {
        *ancestry.last().unwrap()
    } else if first_segment == "super" {
        if let Some(grandparent) = resolution_graph.inner[node_parent].parent() {
            grandparent
        } else {
            return Err(too_many_supers(file, first_segment));
        }
    } else if first_segment == "self" {
        if r.path.segments.len() > 1 {
            return Err(non_ancestral_visibility(file, &first_segment, None));
        }
        return Ok(Some(node_parent));
    } else {
        return Err(incorrect_visibility_restriction(file, first_segment.span()));
    };

    for (i, (prev_segment, segment)) in r
        .path
        .segments
        .iter()
        .zip(r.path.segments.iter().skip(1))
        .enumerate()
    {
        if segment == "crate"
            || segment == "self"
            || (prev_segment != "super" && segment == "super")
        {
            return Err(special_ident_not_at_start_of_path(file, &segment));
        } else if prev_segment == "super" && segment != "super" {
            return Err(non_ancestral_visibility(
                file,
                &segment,
                Some(&prev_segment),
            ));
        }

        export_dest = if segment == "super" {
            if let Some(export_dest_parent) = resolution_graph.inner[export_dest].parent() {
                if !is_target_visible(resolution_graph, export_dest_parent, node_parent) {
                    return Err(scope_visibility(
                        file,
                        &segment,
                        if resolution_graph.inner[export_dest_parent]
                            .parent()
                            .is_none()
                        {
                            ItemHint::InternalNamedRootScope
                        } else {
                            ItemHint::InternalNamedChildScope
                        },
                    ));
                }
                export_dest_parent
            } else {
                return Err(too_many_supers(file, &segment));
            }
        } else {
            let export_dest_children: Vec<ResolutionIndex> = resolution_graph.inner[export_dest]
                .children()
                .and_then(|children| children.get(&Some(&segment)))
                .map(|named_children| {
                    named_children
                        .iter()
                        .filter(|child| resolution_graph.inner[**child].is_valid_use_path_segment())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if export_dest_children.is_empty() {
                return Err(unresolved_item(
                    file,
                    r.path.segments.iter().nth(i),
                    &segment,
                    ItemHint::InternalNamedChildScope,
                    vec![],
                ));
            } else if let Some(export_dest_child) = export_dest_children
                .iter()
                .find(|child| ancestry.contains(child))
            {
                if !is_target_visible(resolution_graph, *export_dest_child, node_parent) {
                    return Err(scope_visibility(
                        file,
                        &segment,
                        ItemHint::InternalNamedChildScope,
                    ));
                }
                *export_dest_child
            } else {
                return Err(non_ancestral_visibility(
                    file,
                    &segment,
                    Some(&prev_segment),
                ));
            }
        };
    }
    // TODO: are beyond root exports for a given path possible?
    Ok(Some(export_dest))
}

fn apply_visibility_pub<'ast>(
    resolution_graph: &ResolutionGraph<'ast>,
    node: ResolutionIndex,
) -> Result<Option<ResolutionIndex>, Diagnostic> {
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
) -> Result<Option<ResolutionIndex>, Diagnostic> {
    let root = *build_ancestry(resolution_graph, node).last().unwrap();
    Ok(Some(root))
}

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
    if target_parent == target
        || resolution_graph.inner[target_parent]
            .parent()
            .map(|g| g == target)
            .unwrap_or_default()
        || dest == target_parent
    {
        // self
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
