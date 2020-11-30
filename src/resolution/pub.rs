use rhdl::ast::{Spanned, Vis, VisRestricted};
use z3::{ast::*, Context, Solver, Sort};

use super::{Branch, Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode};
use crate::error::*;
use crate::find_file::FileId;

#[derive(Debug)]
pub struct VisibilitySolver<'ast> {
    ctx: &'ast Context,
    solver: Solver<'ast>,
    nodes: Vec<Dynamic<'ast>>,
    root: Dynamic<'ast>,
    ancestry: Array<'ast>,
    parents: Array<'ast>,
    children: Array<'ast>,
    exports: Array<'ast>,
}

impl<'ast> VisibilitySolver<'ast> {
    /// Possibilities:
    /// 1. Target is exported to grandparent scope (implicitly assume the visibility of the grandparent scope was already checked)
    /// 2. Target is directly exported to destination
    /// 3. Target is exported to some ancestral scope of destination
    /// 4. Target lies in some ancestral scope of destination
    pub fn is_target_visible(
        &self,
        resolution_graph: &ResolutionGraph<'_>,
        dest: ResolutionIndex,
        target: ResolutionIndex,
    ) -> bool {
        let dest_node = &self.nodes[Into::<usize>::into(dest)];
        let target_node = &self.nodes[Into::<usize>::into(target)];
        self.solver.push();
        // Where the target is exported
        let target_export = self.exports.select(target_node);
        // 1. Target is exported to grandparent scope
        // 2. Target is directly exported to destination
        // 3. Target is exported to some ancestral scope of destination
        // 4. Target lies in some ancestral scope of destination
        let parent = &self.parents.select(target_node);
        self.solver.assert(&Bool::or(
            self.ctx,
            &[
                &self
                    .ancestry
                    .select(&parent)
                    .as_set()
                    .unwrap()
                    .member(&target_export),
                &dest_node._eq(&target_export),
                &target_export._eq(&self.root),
                &self
                    .ancestry
                    .select(&dest_node)
                    .as_set()
                    .unwrap()
                    .member(&target_export),
                &self
                    .ancestry
                    .select(&target_node)
                    .as_set()
                    .unwrap()
                    .set_subset(&self.ancestry.select(&dest_node).as_set().unwrap()),
            ],
        ));

        use z3::SatResult::*;
        let visible = match self.solver.check() {
            Unsat => false,
            Sat => true,
            Unknown => false,
        };
        self.solver.pop(1);
        visible
    }
}

pub fn build_visibility_solver<'ast>(
    resolution_graph: &mut ResolutionGraph<'ast>,
    errors: &mut Vec<Diagnostic>,
    ctx: &'ast Context,
) -> VisibilitySolver<'ast> {
    let node_ty = Sort::int(ctx);
    // let ancestry_ty = Sort::array(&ctx, &idx_ty, &node_ty);
    let solver = Solver::new(&ctx);

    // Create nodes
    let root: Dynamic = Int::from_i64(ctx, -1).into();
    let nodes = resolution_graph
        .node_indices()
        .map(|i| Int::from_u64(&ctx, Into::<usize>::into(i) as u64).into())
        .collect::<Vec<Dynamic>>();

    // Store visibility state
    let mut z3_parents = Array::new_const(&ctx, "parents", &node_ty, &node_ty);
    let mut z3_ancestry = Array::new_const(&ctx, "ancestry", &node_ty, &Sort::set(&ctx, &node_ty))
        .store(&root, &Set::empty(ctx, &node_ty).into());
    let mut z3_children = Array::new_const(&ctx, "children", &node_ty, &Sort::set(&ctx, &node_ty))
        .store(
            &root,
            &{
                let mut root_children = Set::empty(ctx, &node_ty);
                for root in resolution_graph.roots.iter() {
                    root_children = root_children.add(&nodes[Into::<usize>::into(*root)]);
                }
                root_children
            }
            .into(),
        );
    let mut z3_exports = Array::new_const(&ctx, "exports", &node_ty, &node_ty);
    for node in resolution_graph.node_indices() {
        let idx = Into::<usize>::into(node);
        let ancestry = build_ancestry(resolution_graph, node, false);

        let ancestry_const = Set::new_const(&ctx, format!("x{}_ancestry", idx), &node_ty);
        solver.assert(&ancestry_const._eq(&if let Some(parent) =
            ancestry.first().map(|p| Into::<usize>::into(*p))
        {
            Set::set_union(
                ctx,
                &[
                    &z3_ancestry
                        .select(&Int::from_u64(ctx, parent as u64).into())
                        .as_set()
                        .unwrap(),
                    &Set::empty(ctx, &node_ty).add(&nodes[parent]),
                ],
            )
        } else {
            Set::empty(ctx, &node_ty).add(&root)
        }));
        z3_ancestry = z3_ancestry.store(&nodes[idx], &ancestry_const.into());
        let mut children_const = Set::empty(&ctx, &node_ty);
        if let Some(children) = resolution_graph[node].children() {
            for child in children.values().flatten() {
                children_const = children_const.add(&nodes[Into::<usize>::into(*child)]);
            }
        }
        z3_children = z3_children.store(&nodes[idx], &children_const.into());
        use Vis::*;
        let file = resolution_graph.file(node);
        let parent = if let Some(parent) = ancestry.first() {
            &nodes[Into::<usize>::into(*parent)]
        } else {
            &root
        };
        z3_parents = z3_parents.store(&nodes[idx], parent);
        let grandparent = if let Some(grandparent) = ancestry.iter().skip(1).next() {
            &nodes[Into::<usize>::into(*grandparent)]
        } else {
            // Forest "root"
            &root
        };
        // TODO: once trait items are split into leaves, assert their exports to same as trait
        if matches!(resolution_graph[node], ResolutionNode::Leaf{leaf: Leaf::NamedField(_), ..} | ResolutionNode::Leaf{leaf: Leaf::UnnamedField(_), ..} | ResolutionNode::Branch{branch: Branch::Variant(_), ..})
        {
            // bad visibility usage
            if let Some(vis) = resolution_graph[node].visibility() {
                errors.push(unnecessary_visibility(file, vis));
            }
            solver.assert(
                &z3_exports
                    .select(&nodes[idx])
                    ._eq(&z3_exports.select(parent)),
            );
        } else if let Some(vis) = resolution_graph[node].visibility() {
            match vis {
                Pub(_) | Super(_) => {
                    z3_exports = z3_exports.store(&nodes[idx], &grandparent);
                }
                Crate(_) => {
                    z3_exports = z3_exports.store(
                        &nodes[idx],
                        &nodes[Into::<usize>::into(*ancestry.last().unwrap())],
                    );
                }
                Restricted(r) => match apply_visibility_in(resolution_graph, node, file, r) {
                    Ok(dest) => {
                        z3_exports = z3_exports.store(
                            &nodes[idx],
                            if let Some(dest) = dest {
                                &nodes[Into::<usize>::into(dest)]
                            } else {
                                &root
                            },
                        );
                    }
                    Err(err) => {
                        errors.push(err);
                        z3_exports = z3_exports.store(&nodes[idx], parent);
                    }
                },
                // export to parent is an easy way of not making it visible anywhere else
                Priv(_) | LowerSelf(_) => {
                    z3_exports = z3_exports.store(&nodes[idx], parent);
                }
            }
        } else {
            // treated the same as an explicit export to self
            z3_exports = z3_exports.store(&nodes[idx], parent);
        }
    }

    VisibilitySolver {
        ctx,
        solver,
        nodes,
        root,
        ancestry: z3_ancestry,
        parents: z3_parents,
        children: z3_children,
        exports: z3_exports,
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
        0
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
                // TODO: apparently, rust is fine with this
                // if !is_target_visible(
                //     resolution_graph,
                //     ancestry[ancestry_position + 1],
                //     node_parent,
                // ) {
                //     return Err(scope_visibility(
                //         file,
                //         segment.span(),
                //         resolution_graph[node].item_hint().unwrap(),
                //         if ancestry_position + 2 < ancestry.len() {
                //             ItemHint::InternalNamedChildScope
                //         } else {
                //             ItemHint::InternalNamedRootScope
                //         },
                //     ));
                // }
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
                        .any(|child| resolution_graph[*child].is_valid_pub_path_segment())
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
                // TODO: apparently rust is fine with this
                // if !is_target_visible(
                //     resolution_graph,
                //     ancestry[ancestry_position - 1],
                //     node_parent,
                // ) {
                //     return Err(scope_visibility(
                //         file,
                //         segment.span(),
                //         resolution_graph[node].item_hint().unwrap(),
                //         ItemHint::InternalNamedChildScope,
                //     ));
                // }
                ancestry_position - 1
            }
        };
    }

    // TODO: are beyond crate-root exports for a given path possible?
    Ok(Some(ancestry[ancestry_position]))
}

fn build_ancestry(
    resolution_graph: &ResolutionGraph<'_>,
    node: ResolutionIndex,
    segments_only: bool,
) -> Vec<ResolutionIndex> {
    let mut prev_parent = node;
    let mut ancestry = vec![];
    while let Some(parent) = resolution_graph[prev_parent].parent() {
        if !segments_only || resolution_graph[parent].is_valid_pub_path_segment() {
            ancestry.push(parent);
        }
        prev_parent = parent;
    }
    ancestry
}
