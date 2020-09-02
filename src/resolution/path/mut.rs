//! This is ONLY meant to be used for use-tracing, which is a niche case

use fxhash::FxHashSet as HashSet;

use syn::{visit::Visit, Ident, UseGlob, UseName, UsePath, UseRename, UseTree};

use super::super::{
    r#use::UseResolver, Branch, Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode,
};
use super::TracingContext;
use crate::error::*;

pub struct PathFinder<'a, 'ast> {
    pub resolution_graph: &'a mut ResolutionGraph<'ast>,
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

        let is_special_ident =
            ident == "super" || ident == "crate" || ident == "self" || ident == "Self";
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
                    children.get(&None).cloned().map(|children_unnamed| {
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
            local.retain(|i| {
                super::super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, *i)
            });
            if global.is_empty() && !local_is_empty && local.is_empty() {
                return Err(ItemVisibilityError {
                    file: ctx.file.clone(),
                    ident: ident.clone(),
                    hint,
                }
                .into());
            }
            let global_is_empty = global.is_empty();
            global.retain(|i| {
                super::super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, *i)
            });
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
                            .cloned()
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
                            super::super::r#pub::is_target_visible(
                                self.resolution_graph,
                                ctx.dest,
                                *i,
                            )
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
        if !super::super::r#pub::is_target_visible(self.resolution_graph, ctx.dest, use_index) {
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
                let mut rebuilt_ctx = TracingContext::new(
                    self.resolution_graph,
                    use_index,
                    match self.resolution_graph.inner[use_index] {
                        ResolutionNode::Branch {
                            branch: Branch::Use(u),
                            ..
                        } => u.leading_colon.is_some(),
                        _ => false,
                    },
                );
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
                            glob_src_children.get(&None).cloned().map(
                                |glob_src_children_unnamed| {
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
                                },
                            );
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
