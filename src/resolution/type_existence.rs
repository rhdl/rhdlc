//! TODO: error for items that could do visibility hole punching:
//! * function with params that are not as visible as the function
//! * structs with members that are not as visible as their types (?)

use rhdl::{
    ast::{
        Block, File, GenericParam, GenericParamType, Generics, Item, ItemArch, ItemImpl, ItemMod,
        ItemTrait, Qualifier, TypePath,
    },
    visit::Visit,
};

use crate::error::*;
use crate::resolution::{
    path::r#type::PathFinder, Branch, ResolutionGraph, ResolutionIndex, ResolutionNode,
};

pub struct TypeExistenceChecker<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<Diagnostic>,
}

struct TypeExistenceCheckerVisitor<'a, 'ast> {
    resolution_graph: &'a ResolutionGraph<'ast>,
    errors: &'a mut Vec<Diagnostic>,
    scope: ResolutionIndex,
    block_visited: bool,
}

impl<'a, 'ast> TypeExistenceChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for scope in self.resolution_graph.node_indices() {
            if self.resolution_graph[scope].is_type_existence_checking_candidate() {
                let mut ctx_checker = TypeExistenceCheckerVisitor {
                    resolution_graph: self.resolution_graph,
                    errors: self.errors,
                    scope,
                    block_visited: !matches!(self.resolution_graph[scope], ResolutionNode::Branch{branch: Branch::Block(_), ..}),
                };
                self.resolution_graph[scope].visit(&mut ctx_checker);
            }
        }
    }
}

impl<'a, 'ast> TypeExistenceCheckerVisitor<'a, 'ast> {
    fn find_in_scope<F>(
        &self,
        path: &TypePath,
        filter: F,
        hint: ItemHint,
    ) -> Result<ResolutionIndex, Diagnostic>
    where
        F: Fn(ResolutionIndex) -> bool,
    {
        // TODO: private trait in public trait declaration
        let found = {
            let mut path_finder = PathFinder {
                resolution_graph: &self.resolution_graph,
                visited_glob_scopes: Default::default(),
            };
            path_finder.find_at_path(self.scope, &path)
        }?;
        // Check that there is a single match
        let matching = found
            .iter()
            .copied()
            .filter(|i| filter(*i))
            .collect::<Vec<ResolutionIndex>>();
        if matching.len() != 1 {
            let file = self.resolution_graph.file(self.scope);
            let ident = &path.segments.last().as_ref().unwrap().ident;
            if matching.is_empty() {
                Err(unexpected_item(
                    file,
                    ident,
                    hint,
                    found
                        .first()
                        .and_then(|x| self.resolution_graph[*x].item_hint())
                        .unwrap(),
                ))
            } else {
                Err(disambiguation_needed(
                    file,
                    ident,
                    AmbiguitySource::Item(hint),
                ))
            }
        } else {
            Ok(*matching.first().unwrap())
        }
    }
}

impl<'a, 'ast> Visit<'ast> for TypeExistenceCheckerVisitor<'a, 'ast> {
    fn visit_file(&mut self, _file: &'ast File) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_mod(&mut self, _item_mod: &'ast ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item(&mut self, _item: &'ast Item) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {
        if let Some(generics) = &item_impl.generics {
            self.visit_generics(generics);
        }
        if let Some((of_ty, _for)) = &item_impl.of {
            if let Err(err) = self.find_in_scope(
                of_ty,
                |i| self.resolution_graph[i].is_trait(),
                ItemHint::Trait,
            ) {
                self.errors.push(err)
            }
        }
        self.visit_type(&item_impl.ty);
    }

    fn visit_item_arch(&mut self, item_arch: &'ast ItemArch) {
        if let Some(generics) = &item_arch.generics {
            self.visit_generics(generics);
        }
        self.visit_type_path(&item_arch.entity);
    }

    fn visit_item_trait(&mut self, item_trait: &'ast ItemTrait) {
        if let Some(generics) = &item_trait.generics {
            self.visit_generics(generics);
        }
        if let Some((_, super_traits)) = &item_trait.super_traits {
            for super_trait in super_traits.iter() {
                if let Err(err) = self.find_in_scope(
                    super_trait,
                    |i| self.resolution_graph[i].is_trait(),
                    ItemHint::Trait,
                ) {
                    self.errors.push(err)
                }
            }
        }
    }

    fn visit_qualifier(&mut self, qualifier: &'ast Qualifier) {
        self.visit_type(&qualifier.ty);
        if let Some((_, trait_path)) = &qualifier.cast {
            if let Err(err) = self.find_in_scope(
                trait_path,
                |i| self.resolution_graph[i].is_trait(),
                ItemHint::Trait,
            ) {
                self.errors.push(err)
            }
        }
    }

    fn visit_block(&mut self, block: &'ast Block) {
        if !self.block_visited {
            self.block_visited = true;
            block
                .statements
                .iter()
                .for_each(|stmt| self.visit_stmt(stmt));
        }
    }

    fn visit_generics(&mut self, generics: &'ast Generics) {
        for generic_param in generics.params.iter() {
            self.visit_generic_param(generic_param);
        }
    }

    fn visit_generic_param_type(&mut self, generic_type_param: &'ast GenericParamType) {
        if let Some((_, bounds)) = &generic_type_param.bounds {
            for type_path in bounds.iter() {
                if let Err(err) = self.find_in_scope(
                    type_path,
                    |i| self.resolution_graph[i].is_trait(),
                    ItemHint::Trait,
                ) {
                    self.errors.push(err)
                }
                for seg in type_path.segments.iter() {
                    self.visit_path_segment(seg);
                }
            }
        }
    }

    fn visit_type_path(&mut self, type_path: &'ast TypePath) {
        if let Err(err) = self.find_in_scope(
            &type_path,
            |i| {
                self.resolution_graph[i].is_type()
                    || (type_path.segments.len() == 1
                        && type_path
                            .segments
                            .first()
                            .map(|seg| seg.ident == "Self")
                            .unwrap_or_default()
                        && self.resolution_graph[i].is_trait_or_impl_or_arch())
            },
            ItemHint::Type,
        ) {
            // Find a generic, if there is one
            if type_path.segments.len() == 1 {
                let first = &type_path.segments.first().unwrap();
                if first.generic_args.is_none() {
                    let mut current = self.scope;
                    loop {
                        if let Some(param) =
                            self.resolution_graph[current]
                                .generics()
                                .and_then(|generics| {
                                    generics
                                        .params
                                        .iter()
                                        .filter(|g| matches!(g, GenericParam::Type(_)))
                                        .find(|g| *g.ident() == first.ident)
                                })
                        {
                            return;
                        }
                        current = self.resolution_graph[current].parent().unwrap();
                        if self.resolution_graph[current].is_valid_pub_path_segment() {
                            break;
                        }
                    }
                }
            }
            self.errors.push(err);
        }
    }
}
