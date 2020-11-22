use fxhash::FxHashSet as HashSet;

use rhdl::ast::{GenericParam, Ident, TypePath};

use super::TracingContext;
use crate::error::*;
use crate::resolution::{Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode};

pub struct PathFinder<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub visited_glob_scopes: HashSet<ResolutionIndex>,
}

impl<'a, 'ast> PathFinder<'a, 'ast> {
    pub fn find_at_path(
        &mut self,
        dest: ResolutionIndex,
        path: &'a TypePath,
    ) -> Result<Vec<ResolutionIndex>, Diagnostic> {
        self.visited_glob_scopes.clear();
        let mut ctx = TracingContext::new(self.resolution_graph, dest, path.leading_sep.as_ref());

        let scopes = if path
            .segments
            .first()
            .map(|seg| seg.ident == "Self")
            .unwrap_or_default()
        {
            // TODO: also allow type/trait aliases to use Self
            if let Some((parent, true)) = self.resolution_graph[dest].parent().map(|parent| {
                (
                    parent,
                    self.resolution_graph[parent].is_trait_or_impl_or_arch(),
                )
            }) {
                vec![parent]
            } else {
                return Err(unresolved_item(
                    self.resolution_graph.file(dest),
                    None,
                    &path.segments.first().unwrap().ident,
                    ItemHint::Type,
                    vec![],
                ));
            }
        } else {
            let mut scopes = vec![dest];
            while !self.resolution_graph[*scopes.last().unwrap()].is_valid_pub_path_segment() {
                scopes.push(
                    self.resolution_graph[*scopes.last().unwrap()]
                        .parent()
                        .unwrap(),
                );
            }
            scopes
        };
        let first = path.segments.first().unwrap();
        // DFS from each scope, followed by a check on that scope's generics
        for scope in scopes.iter().rev().copied() {
            let mut dfs_state = vec![scope];
            for (i, segment) in path.segments.iter().enumerate() {
                // already seeded earlier
                if i == 0 && segment.ident == "Self" {
                    continue;
                }
                let mut results: Vec<Result<Vec<ResolutionIndex>, Diagnostic>> = dfs_state
                    .iter()
                    .map(|scope| {
                        self.find_children(
                            &ctx,
                            *scope,
                            &segment.ident,
                            i + 1 != path.segments.len(),
                        )
                    })
                    .collect();

                if i == 0 {
                    // first segment found
                    if let Some(err) =
                        results
                            .iter()
                            .filter_map(|res| res.as_ref().err())
                            .find(|err| {
                                if let Some(code) = &err.code {
                                    code != UNRESOLVED_ITEM_CODE
                                } else {
                                    true
                                }
                            })
                    {
                        return Err(err.clone());
                    }
                } else if results.iter().all(|res| res.is_err()) {
                    return results.first().unwrap().clone();
                }
                dfs_state = results
                    .drain(..)
                    .filter_map(|res| res.ok())
                    .flatten()
                    .collect();
                if dfs_state.is_empty() {
                    break;
                }
                ctx.previous_idents.push(&segment.ident);
            }
            if !dfs_state.is_empty() {
                return Ok(dfs_state);
            } else if let Some(matching_generic) =
                self.resolution_graph[scope]
                    .generics()
                    .and_then(|generics| {
                        generics
                            .params
                            .iter()
                            .filter(|g| matches!(g, GenericParam::Type(_)))
                            .find(|g| *g.ident() == first.ident)
                    })
            {
                if path.segments.len() != 1 {
                    todo!("check generic children: {:#?}", matching_generic);
                }
            }
        }
        return Err(unresolved_item(
            ctx.file,
            None,
            &first.ident,
            ItemHint::Item,
            vec![],
        ));
    }

    pub fn find_children(
        &mut self,
        ctx: &TracingContext,
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
                if let Some(children) = self.resolution_graph[scope].children() {
                    let mut local = children
                        .get(&Some(ident))
                        .map(|children_with_name| {
                            children_with_name
                                .iter()
                                .copied()
                                .filter(|child| {
                                    !paths_only
                                        || self.resolution_graph[*child]
                                            .is_valid_type_path_segment()
                                })
                                .collect::<Vec<ResolutionIndex>>()
                        })
                        .unwrap_or_default();
                    if let Some(children_unnamed) = children.get(&None) {
                        children_unnamed.iter().for_each(|child| {
                            if self.resolution_graph[*child].is_use() {
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
                        !paths_only || self.resolution_graph[**child].is_valid_type_path_segment()
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
                let local_from_globs = self.resolution_graph[scope]
                    .children()
                    .and_then(|children| children.get(&None))
                    .map(|children_unnamed| {
                        let mut local_from_globs = vec![];
                        children_unnamed.iter().for_each(|child| {
                            if self.resolution_graph[*child].is_use() {
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
        ctx: &TracingContext,
        use_index: ResolutionIndex,
        ident_to_look_for: &Ident,
        paths_only: bool,
        glob_only: bool,
    ) -> Vec<ResolutionIndex> {
        if !crate::resolution::r#pub::is_target_visible(self.resolution_graph, ctx.dest, use_index)
        {
            vec![]
        } else {
            let use_children = self.resolution_graph[use_index].children().unwrap();
            let matches: Vec<ResolutionIndex> = if glob_only {
                let mut matches = vec![];
                use_children
                    .get(&None)
                    .map(|globs| {
                        globs.iter().for_each(|glob| {
                            let glob = match self.resolution_graph[*glob] {
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
                            let glob_src_children = self.resolution_graph[glob].children().unwrap();
                            matches.append(
                                &mut glob_src_children
                                    .get(&Some(ident_to_look_for))
                                    .map(|glob_src_children_with_name| {
                                        glob_src_children_with_name
                                            .iter()
                                            .copied()
                                            .filter(|child| {
                                                !paths_only
                                                    || self.resolution_graph[*child]
                                                        .is_valid_type_path_segment()
                                            })
                                            .collect::<Vec<ResolutionIndex>>()
                                    })
                                    .unwrap_or_default(),
                            );
                            if let Some(glob_src_children_unnamed) = glob_src_children.get(&None) {
                                glob_src_children_unnamed.iter().for_each(|child| {
                                    if self.resolution_graph[*child].is_use() {
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
                            .filter_map(|child| match &self.resolution_graph[*child] {
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
                            .copied()
                            .filter(|child| {
                                !paths_only
                                    || self.resolution_graph[*child].is_valid_type_path_segment()
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            matches
        }
    }
}
