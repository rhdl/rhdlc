//! This is ONLY meant to be used for use-tracing, which is a niche case

use fxhash::FxHashSet as HashSet;

use rhdl::{
    ast::{
        Ident, SimplePath as Path, UseTree, UseTreeGlob, UseTreeName, UseTreePath, UseTreeRename,
    },
    visit::Visit,
};

use super::super::{
    r#use::UseResolver, Branch, Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode,
};
use super::TracingContext;
use crate::error::*;

pub struct PathFinder<'a, 'ast> {
    pub resolution_graph: &'a mut ResolutionGraph<'ast>,
    pub visited_glob_scopes: HashSet<ResolutionIndex>,
    pub errors: &'a mut Vec<Diagnostic>,
    pub resolved_uses: &'a mut HashSet<ResolutionIndex>,
}

impl<'a, 'ast> PathFinder<'a, 'ast> {
    pub fn find_at_path(
        &mut self,
        dest: ResolutionIndex,
        path: &'ast Path,
    ) -> Result<Vec<ResolutionIndex>, Diagnostic> {
        self.visited_glob_scopes.clear();
        let mut ctx = TracingContext::new(self.resolution_graph, dest, path.leading_sep.as_ref());

        let mut scopes = {
            let mut dest_scope = dest;
            while !self.resolution_graph.inner[dest_scope].is_valid_use_path_segment() {
                dest_scope = self.resolution_graph.inner[dest_scope].parent().unwrap();
            }

            // Also seed this scope
            if let ResolutionNode::Branch {
                branch: Branch::Fn(_),
                ..
            } = &self.resolution_graph.inner[ctx.dest]
            {
                vec![dest, dest_scope]
            } else {
                vec![dest_scope]
            }
        };
        for (i, segment) in path.segments.iter().enumerate() {
            // TODO: resolve hint precision regression in test/compile-fail/resolution/use/no-path
            let mut results: Vec<Result<Vec<ResolutionIndex>, Diagnostic>> = scopes
                .iter()
                .map(|scope| {
                    self.find_children(&ctx, *scope, segment, i + 1 != path.segments.len())
                })
                .collect();
            if results.iter().all(|res| res.is_err()) {
                return results.drain(..).next().unwrap();
            }
            scopes = results
                .drain(..)
                .filter_map(|res| res.ok())
                .flatten()
                .collect();
            ctx.previous_idents.push(&segment);
        }
        Ok(scopes)
    }

    /// Ok is guaranteed to have >= 1 node, else an unresolved error will be returned
    pub fn find_children(
        &mut self,
        ctx: &TracingContext<'ast>,
        scope: ResolutionIndex,
        ident: &Ident,
        paths_only: bool,
    ) -> Result<Vec<ResolutionIndex>, Diagnostic> {
        let is_entry = ctx.previous_idents.is_empty();

        if let Some(child) = super::handle_special_ident(self.resolution_graph, ctx, scope, ident)?
        {
            Ok(vec![child])
        } else {
            let local = if !is_entry || ctx.leading_sep.is_none() {
                if let Some(children) = self.resolution_graph.inner[scope].children() {
                    let mut local = children
                        .get(&Some(ident))
                        .map(|children_with_name| {
                            children_with_name
                                .iter()
                                .filter(|child| {
                                    !paths_only
                                        || self.resolution_graph.inner[**child]
                                            .is_valid_use_path_segment()
                                })
                                .cloned()
                                .collect::<Vec<ResolutionIndex>>()
                        })
                        .unwrap_or_default();
                    if let Some(children_unnamed) = children.get(&None).cloned() {
                        children_unnamed.iter().for_each(|child| {
                            if self.resolution_graph.inner[*child].is_use() {
                                local.append(
                                    &mut self
                                        .matching_from_use(ctx, *child, ident, paths_only, false),
                                );
                            }
                        })
                    }
                    local
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
            let global = if is_entry {
                self.resolution_graph
                    .roots
                    .iter()
                    .filter(|child| **child != ctx.root)
                    .filter(|child| {
                        !paths_only
                            || self.resolution_graph.inner[**child].is_valid_use_path_segment()
                    })
                    .copied()
                    .collect()
            } else {
                vec![]
            };
            if let Some(children) = super::find_children_from_local_and_global(
                self.resolution_graph,
                ctx,
                ident,
                paths_only,
                local,
                global,
            )? {
                Ok(children)
            } else if !(ctx.leading_sep.is_some() && is_entry) {
                let local_from_globs = self.resolution_graph.inner[scope]
                    .children()
                    .and_then(|children| children.get(&None))
                    .cloned()
                    .map(|children_unnamed| {
                        let mut local_from_globs = vec![];
                        children_unnamed.iter().for_each(|child| {
                            if self.resolution_graph.inner[*child].is_use() {
                                local_from_globs.append(
                                    &mut self
                                        .matching_from_use(ctx, *child, ident, paths_only, true),
                                );
                            }
                        });
                        local_from_globs
                    })
                    .unwrap_or_default();
                super::find_children_from_globs(
                    self.resolution_graph,
                    ctx,
                    ident,
                    paths_only,
                    local_from_globs,
                )
            } else {
                Err(unresolved_item(
                    ctx.file,
                    ctx.previous_idents.last().copied(),
                    &ident,
                    ItemHint::ExternalNamedScope,
                    vec![],
                ))
            }
        }
    }

    fn matching_from_use(
        &mut self,
        ctx: &TracingContext<'ast>,
        use_index: ResolutionIndex,
        ident_to_look_for: &Ident,
        paths_only: bool,
        glob_only: bool,
    ) -> Vec<ResolutionIndex> {
        if !crate::resolution::r#pub::is_target_visible(self.resolution_graph, ctx.dest, use_index)
        {
            vec![]
        } else {
            if !{
                let mut checker = UseMightMatchChecker {
                    ident_to_look_for,
                    might_match: false,
                };
                self.resolution_graph.inner[use_index].visit(&mut checker);
                checker.might_match
            } {
                return vec![];
            } else if !self.resolved_uses.contains(&use_index) {
                let mut rebuilt_ctx = TracingContext::new(self.resolution_graph, use_index, None);
                let mut use_resolver = UseResolver {
                    resolution_graph: self.resolution_graph,
                    errors: self.errors,
                    resolved_uses: self.resolved_uses,
                };
                use_resolver.trace_use_entry_reenterable(&mut rebuilt_ctx);
            }
            let use_children = self.resolution_graph.inner[use_index].children().unwrap();
            let matches: Vec<ResolutionIndex> = if glob_only {
                let mut matches = vec![];
                use_children
                    .get(&None)
                    .cloned()
                    .map(|globs| {
                        globs.iter().for_each(|glob| {
                            let glob = match self.resolution_graph.inner[*glob] {
                                ResolutionNode::Leaf {
                                    leaf: Leaf::UseGlob(_, glob),
                                    ..
                                } => glob,
                                _ => return,
                            };
                            if self.visited_glob_scopes.contains(&glob) {
                                return;
                            }
                            self.visited_glob_scopes.insert(glob);
                            let glob_src_children =
                                self.resolution_graph.inner[glob].children().unwrap();
                            matches.append(
                                &mut glob_src_children
                                    .get(&Some(ident_to_look_for))
                                    .map(|glob_src_children_with_name| {
                                        glob_src_children_with_name
                                            .iter()
                                            .filter(|child| {
                                                !paths_only
                                                    || self.resolution_graph.inner[**child]
                                                        .is_valid_use_path_segment()
                                            })
                                            .cloned()
                                            .collect::<Vec<ResolutionIndex>>()
                                    })
                                    .unwrap_or_default(),
                            );
                            if let Some(glob_src_children_unnamed) =
                                glob_src_children.get(&None).cloned()
                            {
                                glob_src_children_unnamed.iter().for_each(|child| {
                                    if self.resolution_graph.inner[*child].is_use() {
                                        matches.append(&mut self.matching_from_use(
                                            ctx,
                                            *child,
                                            ident_to_look_for,
                                            paths_only,
                                            true,
                                        ));
                                        matches.append(&mut self.matching_from_use(
                                            ctx,
                                            *child,
                                            ident_to_look_for,
                                            paths_only,
                                            false,
                                        ));
                                    }
                                });
                            }
                        });
                    })
                    .unwrap_or_default();
                matches
            } else {
                use_children
                    .get(&Some(ident_to_look_for))
                    .map(|named| {
                        named
                            .iter()
                            .filter_map(|child| match &self.resolution_graph.inner[*child] {
                                ResolutionNode::Leaf {
                                    leaf: Leaf::UseName(_, imports),
                                    ..
                                }
                                | ResolutionNode::Leaf {
                                    leaf: Leaf::UseRename(_, imports),
                                    ..
                                } => Some(imports),
                                _ => None,
                            })
                            .flatten()
                            .filter(|child| {
                                !paths_only
                                    || self.resolution_graph.inner[**child]
                                        .is_valid_use_path_segment()
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            };
            matches
        }
    }
}

struct UseMightMatchChecker<'a> {
    ident_to_look_for: &'a Ident,
    might_match: bool,
}

impl<'a, 'ast> Visit<'ast> for UseMightMatchChecker<'a> {
    fn visit_use_tree_path(&mut self, tree_path: &'ast UseTreePath) {
        self.visit_use_tree(tree_path.tree.as_ref());
        // Does the end of this path match and is there a self after it?
        self.might_match |= tree_path
            .path
            .segments
            .last()
            .map(|seg| seg == self.ident_to_look_for)
            .unwrap_or(false)
            && match tree_path.tree.as_ref() {
                UseTree::Group(group) => group.trees.iter().any(|tree| match tree {
                    UseTree::Rename(rename) => rename.name == "self",
                    UseTree::Name(name) => name == "self",
                    _ => false,
                }),
                UseTree::Rename(rename) => rename.name == "self",
                UseTree::Name(name) => name == "self",
                _ => false,
            }
    }

    fn visit_use_tree_name(&mut self, name: &'ast UseTreeName) {
        self.might_match |= name == self.ident_to_look_for
    }

    fn visit_use_tree_rename(&mut self, rename: &'ast UseTreeRename) {
        self.might_match |= rename.rename == *self.ident_to_look_for
    }

    fn visit_use_tree_glob(&mut self, _: &'ast UseTreeGlob) {
        self.might_match |= true
    }
}
