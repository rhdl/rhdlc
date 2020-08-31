use fnv::FnvHashSet as HashSet;
use std::rc::Rc;

use syn::{visit::Visit, Ident, Path, UseGlob, UseName, UsePath, UseRename, UseTree};

use super::super::{Branch, ResolutionGraph, ResolutionIndex, ResolutionNode};
use super::TracingContext;
use crate::error::*;
use crate::find_file::File;

pub struct PathFinder<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub visited_glob_scopes: HashSet<ResolutionIndex>,
    pub errors: &'a mut Vec<ResolutionError>,
    pub resolved_uses: &'a mut HashSet<ResolutionIndex>,
}

impl<'a, 'ast> PathFinder<'a, 'ast> {
    /// Ok is guaranteed to have >= 1 node, else an unresolved error will be returned
    pub fn find_children(
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
            .filter(|i| {
                super::super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, **i)
            })
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
            .filter(|i| {
                super::super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, **i)
            })
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
                            super::super::r#pub::is_target_visible(
                                self.resolution_graph,
                                ctx.dest,
                                **i,
                            )
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
        &mut self,
        ctx: &TracingContext,
        use_index: ResolutionIndex,
        ident_to_look_for: &Ident,
        paths_only: bool,
        glob_only: bool,
    ) -> Vec<ResolutionIndex> {
        if !self.resolution_graph.inner[use_index].is_use() {
            vec![]
        } else if !super::super::r#pub::is_target_visible(
            self.resolution_graph,
            ctx.dest,
            use_index,
        ) {
            vec![]
        } else {
            let use_children = self.resolution_graph.inner[use_index].children().unwrap();
            let matches: Vec<ResolutionIndex> = if glob_only {
                use_children
                    .get(&None)
                    .map(|globs| {
                        globs
                            .iter()
                            .filter_map(|glob| {
                                if self.visited_glob_scopes.contains(glob) {
                                    return None;
                                }
                                self.visited_glob_scopes.insert(*glob);
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
                                                .filter_map(|child| {
                                                    if self.resolution_graph.inner[*child].is_use()
                                                    {
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
                                                        Some(matches_from_dest_uses)
                                                    } else {
                                                        None
                                                    }
                                                })
                                                .flatten(),
                                        )
                                    });
                                Some(matches)
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

struct UseMightMatchChecker<'a> {
    ident_to_look_for: &'a Ident,
    might_match: bool,
}

impl<'a, 'ast> Visit<'ast> for UseMightMatchChecker<'a> {
    fn visit_use_path(&mut self, path: &'ast UsePath) {
        // this replaces the default trait impl, need to call use_tree for use name visitation
        self.visit_use_tree(path.tree.as_ref());
        self.might_match |= path.ident == *self.ident_to_look_for
            && match path.tree.as_ref() {
                UseTree::Group(group) => group.items.iter().any(|tree| match tree {
                    UseTree::Rename(rename) => rename.ident == "self",
                    UseTree::Name(name) => name.ident == "self",
                    _ => false,
                }),
                _ => false,
            }
    }

    fn visit_use_name(&mut self, name: &'ast UseName) {
        self.might_match |= name.ident == *self.ident_to_look_for
    }

    fn visit_use_rename(&mut self, rename: &'ast UseRename) {
        self.might_match |= rename.rename == *self.ident_to_look_for
    }

    fn visit_use_glob(&mut self, _: &'ast UseGlob) {
        self.might_match |= true;
    }
}
