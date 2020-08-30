use fnv::FnvHashSet as HashSet;
use std::rc::Rc;

use syn::{Ident, Path};

use super::{ResolutionGraph, ResolutionIndex};
use crate::error::*;
use crate::find_file::File;

pub mod r#mut;

pub struct TracingContext<'ast> {
    pub file: Rc<File>,
    pub root: ResolutionIndex,
    pub dest: ResolutionIndex,
    pub previous_idents: Vec<&'ast Ident>,
    pub has_leading_colon: bool,
}

impl<'ast> TracingContext<'ast> {
    pub fn new(
        resolution_graph: &ResolutionGraph,
        dest: ResolutionIndex,
        has_leading_colon: bool,
    ) -> Self {
        let mut root = dest;
        while let Some(parent) = resolution_graph.inner[root].parent() {
            root = parent;
        }
        Self {
            file: resolution_graph.inner[dest].file(resolution_graph),
            dest,
            root,
            previous_idents: vec![],
            has_leading_colon,
        }
    }
}

pub struct PathFinder<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub visited_glob_scopes: HashSet<ResolutionIndex>,
}

impl<'a, 'ast> PathFinder<'a, 'ast> {
    pub fn find_at_path(
        &mut self,
        dest: ResolutionIndex,
        path: &'a Path,
    ) -> Result<Vec<ResolutionIndex>, ResolutionError> {
        self.visited_glob_scopes.clear();
        let mut ctx =
            TracingContext::new(self.resolution_graph, dest, path.leading_colon.is_some());
        let mut dest_scope = dest;
        while !self.resolution_graph.inner[dest_scope].is_valid_use_path_segment() {
            dest_scope = self.resolution_graph.inner[dest_scope].parent().unwrap();
        }
        let mut scopes = vec![dest_scope];
        for (i, segment) in path.segments.iter().enumerate() {
            let ident = &segment.ident;
            let mut results: Vec<Result<Vec<ResolutionIndex>, ResolutionError>> = scopes
                .iter()
                .map(|scope| self.find_children(&ctx, *scope, ident, i + 1 != path.segments.len()))
                .collect();
            if results.iter().all(|res| res.is_err()) {
                return results.drain(..).next().unwrap();
            }
            scopes = results
                .drain(..)
                .filter_map(|res| res.ok())
                .flatten()
                .collect();
            ctx.previous_idents.push(&segment.ident);
        }
        Ok(scopes)
    }

    /// Ok is guaranteed to have >= 1 node, else an unresolved error will be returned
    fn find_children(
        &mut self,
        ctx: &TracingContext,
        scope: ResolutionIndex,
        ident: &Ident,
        paths_only: bool,
    ) -> Result<Vec<ResolutionIndex>, ResolutionError> {
        let is_entry = ctx.previous_idents.is_empty();
        let hint = if paths_only && is_entry {
            ItemHint::InternalNamedChildOrExternalNamedScope
        } else if paths_only {
            ItemHint::InternalNamedChildScope
        } else {
            ItemHint::Item
        };
        let local = if !is_entry || !ctx.has_leading_colon {
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
                children.get(&None).map(|children_unnamed| {
                    children_unnamed.iter().for_each(|child| {
                        local
                            .extend(&self.matching_from_use(ctx, *child, ident, paths_only, false));
                    })
                });
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
                    !paths_only || self.resolution_graph.inner[**child].is_valid_use_path_segment()
                })
                .cloned()
                .collect()
        } else {
            vec![]
        };
        let visible_local: Vec<ResolutionIndex> = local
            .iter()
            .filter(|i| super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, **i))
            .cloned()
            .collect();
        if global.is_empty() && !local.is_empty() && visible_local.is_empty() {
            return Err(ItemVisibilityError {
                file: ctx.file.clone(),
                ident: ident.clone(),
                hint,
            }
            .into());
        }
        let visible_global: Vec<ResolutionIndex> = global
            .iter()
            .filter(|i| super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, **i))
            .cloned()
            .collect();
        if local.is_empty() && !global.is_empty() && visible_global.is_empty() {
            return Err(ItemVisibilityError {
                file: ctx.file.clone(),
                ident: ident.clone(),
                hint,
            }
            .into());
        }
        let local = visible_local;
        let global = visible_global;
        match (global.is_empty(), local.is_empty()) {
            (false, false) => Err(DisambiguationError {
                file: ctx.file.clone(),
                ident: ident.clone(),
                src: AmbiguitySource::Item(hint),
            }
            .into()),
            (true, false) => Ok(local),
            (false, true) => Ok(global),
            (true, true) => {
                if !(ctx.has_leading_colon && is_entry) {
                    let local_from_globs = self.resolution_graph.inner[scope]
                        .children()
                        .and_then(|children| children.get(&None))
                        .map(|children_unnamed| {
                            children_unnamed
                                .iter()
                                .map(|child| {
                                    self.matching_from_use(ctx, *child, ident, paths_only, true)
                                })
                                .flatten()
                                .collect::<Vec<ResolutionIndex>>()
                        })
                        .unwrap_or_default();
                    let visible_local_from_globs: Vec<ResolutionIndex> = local_from_globs
                        .iter()
                        .filter(|i| {
                            super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, **i)
                        })
                        .cloned()
                        .collect();
                    if !local_from_globs.is_empty() && visible_local_from_globs.is_empty() {
                        Err(ItemVisibilityError {
                            file: ctx.file.clone(),
                            ident: ident.clone(),
                            hint,
                        }
                        .into())
                    } else if visible_local_from_globs.is_empty() {
                        Err(UnresolvedItemError {
                            file: ctx.file.clone(),
                            previous_ident: ctx.previous_idents.last().cloned().cloned(),
                            unresolved_ident: ident.clone(),
                            hint,
                        }
                        .into())
                    } else {
                        Ok(visible_local_from_globs)
                    }
                } else {
                    Err(UnresolvedItemError {
                        file: ctx.file.clone(),
                        previous_ident: ctx.previous_idents.last().cloned().cloned(),
                        unresolved_ident: ident.clone(),
                        hint: ItemHint::ExternalNamedScope,
                    }
                    .into())
                }
            }
        }
    }

    fn matching_from_use(
        &self,
        ctx: &TracingContext,
        use_index: ResolutionIndex,
        ident_to_look_for: &Ident,
        paths_only: bool,
        glob_only: bool,
    ) -> Vec<ResolutionIndex> {
        if !self.resolution_graph.inner[use_index].is_use() {
            vec![]
        } else if !super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, use_index) {
            vec![]
        } else {
            let use_children = self.resolution_graph.inner[use_index].children().unwrap();
            let matches: Vec<ResolutionIndex> = if glob_only {
                use_children
                    .get(&None)
                    .map(|globs| {
                        globs
                            .iter()
                            .map(|glob| {
                                let glob_src_children =
                                    self.resolution_graph.inner[*glob].children().unwrap();
                                let mut matches = glob_src_children
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
                                    .unwrap_or_default();
                                glob_src_children
                                    .get(&None)
                                    .map(|glob_src_children_unnamed| {
                                        matches.extend(
                                            glob_src_children_unnamed
                                                .iter()
                                                .filter(|child| {
                                                    self.resolution_graph.inner[**child].is_use()
                                                })
                                                .map(|child| {
                                                    let mut matches_from_dest_uses = self
                                                        .matching_from_use(
                                                            ctx,
                                                            *child,
                                                            ident_to_look_for,
                                                            paths_only,
                                                            true,
                                                        );
                                                    matches_from_dest_uses.extend(
                                                        self.matching_from_use(
                                                            ctx,
                                                            *child,
                                                            ident_to_look_for,
                                                            paths_only,
                                                            false,
                                                        ),
                                                    );
                                                    matches_from_dest_uses
                                                })
                                                .flatten(),
                                        )
                                    });
                                matches
                            })
                            .flatten()
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                use_children
                    .get(&Some(ident_to_look_for))
                    .map(|named| {
                        named
                            .iter()
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
