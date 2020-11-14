use rhdl::ast::{Spanned, Vis, VisRestricted};

use super::{ResolutionGraph, ResolutionIndex};
use crate::error::*;
use crate::find_file::FileId;

/// If a node overrides its own visibility, make a note of it in the parent node(s) as an "export".
/// claim: parent scopes are always already visited so no need for recursive behavior
pub fn apply_visibility<'ast>(
    resolution_graph: &mut ResolutionGraph<'ast>,
    node: ResolutionIndex,
) -> Result<(), Diagnostic> {
    if let Some(vis) = resolution_graph[node].visibility() {
        use Vis::*;
        let file = resolution_graph.file(node);
        let export_dest = match vis {
            Pub(_) => apply_visibility_pub(resolution_graph, node, file, vis),
            // Crate(_) => apply_visibility_crate(resolution_graph, node),
            // Super(_) => apply_visibility_pub(resolution_graph, node),
            Restricted(r) => apply_visibility_in(resolution_graph, node, file, r),
            // ExplicitInherited(_) => Ok(Some(resolution_graph[node].parent().unwrap())),
        }?;
        resolution_graph.exports.insert(node, export_dest);
        Ok(())
    } else {
        Ok(())
    }
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
    let node_parent = resolution_graph[node].parent().unwrap();
    let ancestry = build_ancestry(resolution_graph, node, true);

    let first_segment = r
        .path
        .segments
        .first()
        .expect("error if no first segment, this should never happen");
    let mut ancestry_position = if first_segment == "crate" {
        ancestry.len().saturating_sub(1)
    } else if first_segment == "super" {
        if ancestry.len() >= 2 {
            1
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

        ancestry_position = if segment == "super" {
            // chained supers go up towards the root
            if ancestry_position + 1 < ancestry.len() {
                if !is_target_visible(
                    resolution_graph,
                    ancestry[ancestry_position + 1],
                    node_parent,
                ) {
                    return Err(scope_visibility(
                        file,
                        segment.span(),
                        resolution_graph[node].item_hint().unwrap(),
                        if ancestry_position + 2 < ancestry.len() {
                            ItemHint::InternalNamedChildScope
                        } else {
                            ItemHint::InternalNamedRootScope
                        },
                    ));
                }
                ancestry_position + 1
            } else {
                return Err(too_many_supers(file, &segment));
            }
        } else {
            // a regular path goes down to some scope that is also an ancestor
            let has_matching_child = resolution_graph[ancestry[ancestry_position]]
                .children()
                .and_then(|children| children.get(&Some(&segment)))
                .map(|named_children| {
                    named_children
                        .iter()
                        .any(|child| resolution_graph[*child].is_valid_use_path_segment())
                })
                .unwrap_or_default();
            if !has_matching_child {
                return Err(unresolved_item(
                    file,
                    r.path.segments.iter().nth(i),
                    &segment,
                    ItemHint::InternalNamedChildScope,
                    vec![],
                ));
            } else if ancestry_position == 0 {
                return Err(non_ancestral_visibility(
                    file,
                    &segment,
                    Some(&prev_segment),
                ));
            } else if resolution_graph[ancestry[ancestry_position - 1]]
                .name()
                .unwrap()
                != segment
            {
                return Err(non_ancestral_visibility(
                    file,
                    &segment,
                    Some(&prev_segment),
                ));
            } else {
                if !is_target_visible(
                    resolution_graph,
                    ancestry[ancestry_position - 1],
                    node_parent,
                ) {
                    return Err(scope_visibility(
                        file,
                        segment.span(),
                        resolution_graph[node].item_hint().unwrap(),
                        ItemHint::InternalNamedChildScope,
                    ));
                }
                ancestry_position - 1
            }
        };
    }
    // TODO: are beyond root exports for a given path possible?
    Ok(Some(ancestry[ancestry_position]))
}

fn apply_visibility_pub(
    resolution_graph: &ResolutionGraph<'_>,
    node: ResolutionIndex,
    file: FileId,
    vis: &Vis,
) -> Result<Option<ResolutionIndex>, Diagnostic> {
    let ancestry = build_ancestry(resolution_graph, node, true);
    if let Some(grandparent) = ancestry.iter().skip(1).next().copied() {
        if !is_target_visible(
            resolution_graph,
            grandparent,
            resolution_graph[node].parent().unwrap(),
        ) {
            Err(scope_visibility(
                file,
                vis.span(),
                resolution_graph[node].item_hint().unwrap(),
                if ancestry.len() > 2 {
                    ItemHint::InternalNamedChildScope
                } else {
                    ItemHint::InternalNamedRootScope
                },
            ))
        } else {
            Ok(Some(grandparent))
        }
    } else {
        if !resolution_graph[resolution_graph[node].parent().unwrap()].is_valid_use_path_segment() {
            todo!("explicitly exporting fields beyond a root is not yet supported")
        } else {
            Ok(None)
        }
    }
}

fn build_ancestry(
    resolution_graph: &ResolutionGraph<'_>,
    node: ResolutionIndex,
    segments_only: bool,
) -> Vec<ResolutionIndex> {
    let mut prev_parent = node;
    let mut ancestry = vec![];
    while let Some(parent) = resolution_graph[prev_parent].parent() {
        if !segments_only || resolution_graph[parent].is_valid_use_path_segment() {
            ancestry.push(parent);
        }
        prev_parent = parent;
    }
    ancestry
}

/// TODO: https://github.com/rust-lang/rust/issues/53120
// fn apply_visibility_crate<'ast>(
//     resolution_graph: &ResolutionGraph<'ast>,
//     node: ResolutionIndex,
// ) -> Result<Option<ResolutionIndex>, Diagnostic> {
//     let root = *build_ancestry(resolution_graph, node).last().unwrap();
//     Ok(Some(root))
// }

/// Possibilities:
/// * dest_parent == target_parent (self, always visible)
/// * target_parent == root && dest_root == root (crate, always visible)
/// * target == target_parent (use a::{self, b}, always visible)
/// * target is actually a parent of target_parent (use super::super::b, always visible)
/// * target_parent is a parent of dest_parent (use super::a, always visible)
pub fn is_target_visible(
    resolution_graph: &ResolutionGraph<'_>,
    dest: ResolutionIndex,
    target: ResolutionIndex,
) -> bool {
    let target_parent = if let Some(target_parent) = resolution_graph[target].parent() {
        target_parent
    } else {
        // this is necessarily a root
        return true;
    };
    if target_parent == target
        || resolution_graph[target_parent]
            .parent()
            .map(|g| g == target)
            .unwrap_or_default()
        || dest == target_parent
    {
        // self
        return true;
    }
    let dest_ancestry = build_ancestry(resolution_graph, dest, false);
    // targets in an ancestor of the use are always visible
    if dest_ancestry.contains(&target_parent) {
        return true;
    }

    let target_parent_ancestry = build_ancestry(resolution_graph, target_parent, false);
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
