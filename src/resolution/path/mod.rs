use fxhash::FxHashSet as HashSet;
use std::rc::Rc;

use syn::{Ident, Path};

use super::{Branch, Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode};
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
            file: resolution_graph.file(dest),
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

        let mut scopes = if path
            .segments
            .first()
            .map(|seg| seg.ident == "Self")
            .unwrap_or_default()
        {
            // Seed with applicable traits/impls
            let mut dest_scope = dest;
            if let Some((parent, true)) =
                self.resolution_graph.inner[dest_scope]
                    .parent()
                    .map(|parent| {
                        (
                            parent,
                            self.resolution_graph.inner[parent].is_trait_or_impl(),
                        )
                    })
            {
                dest_scope = parent;
            }
            vec![dest_scope]
        } else {
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
            let ident = &segment.ident;
            if ident == "Self" {
                continue;
            }
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

        let is_special_ident = ident == "super" || ident == "crate" || ident == "self";
        let is_chained_supers = ctx
            .previous_idents
            .last()
            .map(|ident| *ident == "super")
            .unwrap_or(true)
            && ident == "super";
        if !is_entry && is_special_ident && !is_chained_supers {
            Err(SpecialIdentNotAtStartOfPathError {
                file: ctx.file.clone(),
                path_ident: ident.clone(),
            }
            .into())
        } else if ctx.has_leading_colon && is_special_ident {
            Err(GlobalPathCannotHaveSpecialIdentError {
                file: ctx.file.clone(),
                path_ident: ident.clone(),
            }
            .into())
        } else if ident == "self" {
            Ok(vec![scope])
        } else if ident == "super" {
            let mut use_grandparent = self.resolution_graph.inner[scope].parent();
            while use_grandparent
                .map(|i| !self.resolution_graph.inner[i].is_valid_use_path_segment())
                .unwrap_or_default()
            {
                use_grandparent = self.resolution_graph.inner[use_grandparent.unwrap()].parent();
            }
            if let Some(use_grandparent) = use_grandparent {
                Ok(vec![use_grandparent])
            } else {
                Err(TooManySupersError {
                    file: ctx.file.clone(),
                    ident: ident.clone(),
                }
                .into())
            }
        } else if ident == "crate" {
            let mut root = scope;
            while let Some(next_parent) = self.resolution_graph.inner[root].parent() {
                root = next_parent;
            }
            Ok(vec![root])
        } else {
            let mut local = if !is_entry || !ctx.has_leading_colon {
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
                            if self.resolution_graph.inner[*child].is_use() {
                                local.append(
                                    &mut self
                                        .matching_from_use(ctx, *child, ident, paths_only, false),
                                );
                            }
                        })
                    });
                    local
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
            let mut global = if is_entry {
                self.resolution_graph
                    .roots
                    .iter()
                    .filter(|child| **child != ctx.root)
                    .filter(|child| {
                        !paths_only
                            || self.resolution_graph.inner[**child].is_valid_use_path_segment()
                    })
                    .cloned()
                    .collect()
            } else {
                vec![]
            };
            let local_is_empty = local.is_empty();
            local.retain(|i| super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, *i));
            if global.is_empty() && !local_is_empty && local.is_empty() {
                return Err(ItemVisibilityError {
                    file: ctx.file.clone(),
                    ident: ident.clone(),
                    hint,
                }
                .into());
            }
            let global_is_empty = global.is_empty();
            global.retain(|i| super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, *i));
            if local.is_empty() && !global_is_empty && global.is_empty() {
                return Err(ItemVisibilityError {
                    file: ctx.file.clone(),
                    ident: ident.clone(),
                    hint,
                }
                .into());
            }
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
                        let mut local_from_globs = self.resolution_graph.inner[scope]
                            .children()
                            .and_then(|children| children.get(&None))
                            .map(|children_unnamed| {
                                let mut local_from_globs = vec![];
                                children_unnamed.iter().for_each(|child| {
                                    if self.resolution_graph.inner[*child].is_use() {
                                        local_from_globs.append(&mut self.matching_from_use(
                                            ctx, *child, ident, paths_only, true,
                                        ));
                                    }
                                });
                                local_from_globs
                            })
                            .unwrap_or_default();
                        let local_from_globs_is_empty = local_from_globs.is_empty();
                        local_from_globs.retain(|i| {
                            super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, *i)
                        });
                        if !local_from_globs_is_empty && local_from_globs.is_empty() {
                            Err(ItemVisibilityError {
                                file: ctx.file.clone(),
                                ident: ident.clone(),
                                hint,
                            }
                            .into())
                        } else if local_from_globs.is_empty() {
                            Err(UnresolvedItemError {
                                file: ctx.file.clone(),
                                previous_ident: ctx.previous_idents.last().cloned().cloned(),
                                unresolved_ident: ident.clone(),
                                hint,
                            }
                            .into())
                        } else {
                            Ok(local_from_globs)
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
    }

    fn matching_from_use(
        &mut self,
        ctx: &TracingContext,
        use_index: ResolutionIndex,
        ident_to_look_for: &Ident,
        paths_only: bool,
        glob_only: bool,
    ) -> Vec<ResolutionIndex> {
        if !super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, use_index) {
            vec![]
        } else {
            let use_children = self.resolution_graph.inner[use_index].children().unwrap();
            let matches: Vec<ResolutionIndex> = if glob_only {
                let mut matches = vec![];
                use_children
                    .get(&None)
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
                            glob_src_children
                                .get(&None)
                                .map(|glob_src_children_unnamed| {
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
                                });
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
