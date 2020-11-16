//! TODO: error for items that could do visibility hole punching:
//! * function with params that are not as visible as the function
//! * structs with members that are not as visible as their types (?)

use rhdl::{
    ast::{
        File, GenericParam, GenericParamType, Generics, Item, ItemFn, ItemImpl, ItemMod, ItemType,
        PathSegment, TypePath,
    },
    visit::Visit,
};

use crate::error::*;
use crate::resolution::{path::r#type::PathFinder, ResolutionGraph, ResolutionIndex};

pub struct TypeExistenceChecker<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<Diagnostic>,
}

struct TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    resolution_graph: &'a ResolutionGraph<'ast>,
    errors: &'a mut Vec<Diagnostic>,
    scope: ResolutionIndex,
    generics: Vec<&'c Generics>,
}

impl<'a, 'ast> TypeExistenceChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for scope in self.resolution_graph.node_indices() {
            if self.resolution_graph[scope].is_type_existence_checking_candidate() {
                // Cannot directly visit trait/impl content because RHDL needs the generics from the parent scope
                if self.resolution_graph[scope]
                    .parent()
                    .map(|parent| self.resolution_graph[parent].is_trait_or_impl())
                    .unwrap_or_default()
                {
                    continue;
                }
                let mut ctx_checker = TypeExistenceCheckerVisitor {
                    resolution_graph: self.resolution_graph,
                    errors: self.errors,
                    scope,
                    generics: Default::default(),
                };
                self.resolution_graph[scope].visit(&mut ctx_checker);
            }
        }
    }
}

impl<'a, 'c, 'ast> TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn find_trait(&mut self, path: &TypePath) -> Result<ResolutionIndex, Diagnostic> {
        // TODO: private trait in public trait declaration
        let res = {
            let mut path_finder = PathFinder {
                resolution_graph: &self.resolution_graph,
                visited_glob_scopes: Default::default(),
            };
            path_finder.find_at_path(self.scope, &path)
        };
        res.and_then(|matching| {
            // Check that there is a single trait match
            let num_matching = matching
                .iter()
                .filter(|i| self.resolution_graph[**i].is_trait())
                .count();
            if num_matching != 1 {
                let file = self.resolution_graph.file(self.scope);
                let ident = &path.segments.last().as_ref().unwrap().ident;
                if num_matching == 0 {
                    Err(unexpected_item(
                        file,
                        ident,
                        ItemHint::Trait,
                        matching
                            .first()
                            .and_then(|x| self.resolution_graph[*x].item_hint())
                            .unwrap_or(ItemHint::Item),
                    ))
                } else {
                    Err(disambiguation_needed(
                        file,
                        ident,
                        AmbiguitySource::Item(ItemHint::Trait),
                    ))
                }
            } else {
                Ok(*matching
                    .iter()
                    .find(|i| self.resolution_graph[**i].is_trait())
                    .unwrap())
            }
        })
    }
}
impl<'a, 'c, 'ast> Visit<'c> for TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn visit_file(&mut self, _file: &'c File) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_mod(&mut self, _item_mod: &'c ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item(&mut self, _item: &'c Item) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_impl(&mut self, item_impl: &'c ItemImpl) {
        if let Some(generics) = &item_impl.generics {
            self.visit_generics(generics);
        }
        if let Some((of_ty, _for)) = &item_impl.of {
            if let Err(err) = self.find_trait(of_ty) {
                self.errors.push(err)
            }
            self.visit_type_path(of_ty)
        }
        self.visit_type(&item_impl.ty);
        item_impl
            .items
            .iter()
            .for_each(|item| self.visit_impl_item(item));
        if item_impl.generics.is_some() {
            self.generics.pop();
        }
    }

    // fn visit_item_trait(&mut self, item_trait: &'c ItemTrait) {
    //     self.visit_generics(&item_trait.generics);
    //     item_trait
    //         .supertraits
    //         .iter()
    //         .for_each(|supertrait| self.visit_type_param_bound(supertrait));
    //     item_trait
    //         .items
    //         .iter()
    //         .for_each(|item| self.visit_trait_item(item));
    //     self.generics.pop();
    // }

    fn visit_item_type(&mut self, item_type: &'c ItemType) {
        if let Some(ref generics) = item_type.generics {
            self.visit_generics(generics);
        }
        self.visit_type(&item_type.ty);
        if item_type.generics.is_some() {
            self.generics.pop();
        }
    }

    fn visit_item_fn(&mut self, item_fn: &'c ItemFn) {
        self.visit_sig(&item_fn.sig);
        // TODO: special handling is needed for body, to avoid recursing into local items like structs
        // this can be done in a way that would also work for impl methods
        self.visit_block(&item_fn.block);
        // also: can inferrability be handled now?, that would be cool
        // pop off signature generics
        if item_fn.sig.generics.is_some() {
            self.generics.pop();
        }
    }

    fn visit_generics(&mut self, generics: &'c Generics) {
        self.generics.push(generics);
        for generic_param in generics.params.iter() {
            self.visit_generic_param(generic_param);
        }
    }

    fn visit_generic_param_type(&mut self, generic_type_param: &'c GenericParamType) {
        if let Some((_, bounds)) = &generic_type_param.bounds {
            for type_path in bounds.iter() {
                if let Err(err) = self.find_trait(type_path) {
                    self.errors.push(err)
                }
                for seg in type_path.segments.iter() {
                    self.visit_path_segment(seg);
                }
            }
        }
    }

    fn visit_type_path(&mut self, type_path: &'c TypePath) {
        type_path
            .segments
            .iter()
            .rev()
            .enumerate()
            .filter(|(_, seg)| seg.generic_args.is_some())
            .for_each(|(i, seg)| {
                if i != 0 {
                    todo!("error for path arguments not at the end of a path");
                }
                self.visit_generic_args(seg.generic_args.as_ref().unwrap())
            });

        if let Some(PathSegment {
            ident,
            generic_args: None,
        }) = type_path.segments.last()
        {
            if ident == "Self" && self.resolution_graph[self.scope].is_trait_or_impl() {
                return;
            }
            // Check that there is a single type match
            // TODO: need *concrete* types + generics here.
            // * is_type includes type aliases which could actually point to trait
            // * also need to skip self so the type alias doesn't point to itself
            // * also avoid T that uses T in its type param bound
            let is_type_param = self.generics.iter().rev().any(|generics| {
                generics.params.iter().any(|param| {
                    if let GenericParam::Type(GenericParamType {
                        ident: param_type_ident,
                        ..
                    }) = param
                    {
                        ident == param_type_ident
                    } else {
                        false
                    }
                })
            });
            if is_type_param {
                return;
            }
        }

        let mut path_finder = PathFinder {
            resolution_graph: &self.resolution_graph,
            visited_glob_scopes: Default::default(),
        };
        let matching = match path_finder.find_at_path(self.scope, &type_path) {
            Ok(matching) => matching,
            Err(err) => return self.errors.push(err),
        };
        let num_matching = matching
            .iter()
            .filter(|i| self.resolution_graph[**i].is_type())
            .count();
        if num_matching != 1 {
            let file = self.resolution_graph.file(self.scope);
            let ident = &type_path.segments.iter().last().unwrap().ident;
            if num_matching == 0 {
                self.errors.push(unexpected_item(
                    file,
                    &ident,
                    ItemHint::Type,
                    matching
                        .first()
                        .and_then(|x| self.resolution_graph[*x].item_hint())
                        .unwrap_or(ItemHint::Item),
                ));
            } else if num_matching > 1 {
                self.errors.push(disambiguation_needed(
                    file,
                    ident,
                    AmbiguitySource::Item(ItemHint::Type),
                ));
            }
        }
    }
}
