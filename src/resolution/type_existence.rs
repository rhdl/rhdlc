//! TODO: error for items that could do visibilty hole punching:
//! * function with params that are not as visible as the function
//! * structs with members that are not as visible as their types (?)

use syn::{
    visit::Visit, File, Generics, ImplItemMethod, ImplItemType, Item, ItemFn, ItemImpl, ItemMod,
    ItemTrait, Path, PathArguments, PathSegment, TraitBound, TraitItemMethod, TraitItemType,
    TypeParam, TypeParamBound, TypePath,
};

use crate::error::{
    AmbiguitySource, DisambiguationError, ItemHint, Diagnostic, UnexpectedItemError,
};
use crate::resolution::{path::PathFinder, ResolutionGraph, ResolutionIndex};

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
            if self.resolution_graph.inner[scope].is_type_existence_checking_candidate() {
                // Cannot directly visit methods, functions in traits because RHDL need to have the generics from the impl/trait on the generics stack
                if self.resolution_graph.inner[scope]
                    .parent()
                    .map(|parent| self.resolution_graph.inner[parent].is_trait_or_impl())
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
                self.resolution_graph.inner[scope].visit(&mut ctx_checker);
            }
        }
    }
}

impl<'a, 'c, 'ast> TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn find_trait(&mut self, path: &Path) -> Result<ResolutionIndex, Diagnostic> {
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
                .filter(|i| self.resolution_graph.inner[**i].is_trait())
                .count();
            if num_matching != 1 {
                let file = self.resolution_graph.file(self.scope);
                let ident = path.segments.iter().last().unwrap().ident.clone();
                if num_matching == 0 {
                    Err(UnexpectedItemError {
                        file,
                        ident,
                        expected_hint: ItemHint::Trait,
                        actual_hint: matching
                            .first()
                            .and_then(|x| self.resolution_graph.inner[*x].item_hint())
                            .unwrap_or(ItemHint::Item),
                    }
                    .into())
                } else {
                    Err(DisambiguationError {
                        file,
                        ident,
                        src: AmbiguitySource::Item(ItemHint::Trait),
                    }
                    .into())
                }
            } else {
                Ok(*matching
                    .iter()
                    .find(|i| self.resolution_graph.inner[**i].is_trait())
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
        self.visit_generics(&item_impl.generics);
        if let Some((_, path, _)) = &item_impl.trait_ {
            match self.find_trait(path) {
                Ok(_) => {}
                Err(err) => self.errors.push(err),
            }
            if let Some(PathSegment { arguments, .. }) = path.segments.last() {
                self.visit_path_arguments(arguments);
            }
        }
        self.visit_type(item_impl.self_ty.as_ref());
        item_impl
            .items
            .iter()
            .for_each(|item| self.visit_impl_item(item));
        self.generics.pop();
    }

    fn visit_item_trait(&mut self, item_trait: &'c ItemTrait) {
        self.visit_generics(&item_trait.generics);
        item_trait
            .supertraits
            .iter()
            .for_each(|supertrait| self.visit_type_param_bound(supertrait));
        item_trait
            .items
            .iter()
            .for_each(|item| self.visit_trait_item(item));
        self.generics.pop();
    }

    fn visit_impl_item_type(&mut self, impl_item_type: &'c ImplItemType) {
        self.visit_generics(&impl_item_type.generics);
        self.visit_type(&impl_item_type.ty);
        self.generics.pop();
    }

    fn visit_trait_item_type(&mut self, trait_item_type: &'c TraitItemType) {
        self.visit_generics(&trait_item_type.generics);
        for type_param_bound in trait_item_type.bounds.iter() {
            self.visit_type_param_bound(type_param_bound);
        }
        if let Some((_, ty)) = &trait_item_type.default {
            self.visit_type(ty);
        }
        self.generics.pop();
    }

    fn visit_item_fn(&mut self, item_fn: &'c ItemFn) {
        self.visit_signature(&item_fn.sig);
        // TODO: special handling is needed for body, to avoid recursing into local items like structs
        // this can be done in a way that would also work for impl methods
        self.visit_block(item_fn.block.as_ref());
        // also: can inferrability be handled now?, that would be cool
        // pop off signature generics
        self.generics.pop();
    }

    fn visit_impl_item_method(&mut self, impl_item_method: &'c ImplItemMethod) {
        self.visit_signature(&impl_item_method.sig);
        self.visit_block(&impl_item_method.block);
        self.generics.pop();
    }

    fn visit_trait_item_method(&mut self, trait_item_method: &'c TraitItemMethod) {
        self.visit_signature(&trait_item_method.sig);
        if let Some(block) = &trait_item_method.default {
            self.visit_block(block);
        }
        self.generics.pop();
    }

    fn visit_generics(&mut self, generics: &'c Generics) {
        self.generics.push(generics);
        for type_param in generics.type_params() {
            self.visit_type_param(type_param);
            if let Some(default) = &type_param.default {
                self.visit_type(default);
            }
        }
        if let Some(where_clause) = &generics.where_clause {
            self.visit_where_clause(where_clause);
        }
    }

    fn visit_type_param(&mut self, type_param: &'c TypeParam) {
        for bound in &type_param.bounds {
            self.visit_type_param_bound(bound);
        }
    }

    fn visit_type_param_bound(&mut self, bound: &'c TypeParamBound) {
        if let TypeParamBound::Trait(TraitBound { path, .. }) = bound {
            match self.find_trait(path) {
                Ok(_) => {}
                Err(err) => self.errors.push(err),
            }
            if let Some(PathSegment { arguments, .. }) = path.segments.last() {
                self.visit_path_arguments(arguments);
            }
        }
    }

    fn visit_type_path(&mut self, type_path: &'c TypePath) {
        if let Some(qself) = &type_path.qself {
            todo!("support self qualifiers: {:?}", qself);
        }
        type_path
            .path
            .segments
            .iter()
            .rev()
            .enumerate()
            .filter(|(_, seg)| {
                if let PathArguments::None = &seg.arguments {
                    false
                } else {
                    true
                }
            })
            .for_each(|(i, seg)| {
                if i != 0 {
                    todo!("error for path arguments not at the end of a path");
                }
                self.visit_path_arguments(&seg.arguments);
            });

        if let Some(ident) = type_path.path.get_ident() {
            if ident == "Self" && self.resolution_graph.inner[self.scope].is_trait_or_impl() {
                return;
            }
            // Check that there is a single type match
            // TODO: need *concrete* types + generics here.
            // * is_type includes type aliases which could actually point to trait
            // * also need to skip self so the type alias doesn't point to itself
            // * also avoid T that uses T in its type param bound
            let is_type_param = self.generics.iter().rev().any(|generic| {
                generic
                    .type_params()
                    .any(|type_param| type_param.ident == *ident)
            });
            if is_type_param {
                return;
            }
        }

        let mut path_finder = PathFinder {
            resolution_graph: &self.resolution_graph,
            visited_glob_scopes: Default::default(),
        };
        let matching = match path_finder.find_at_path(self.scope, &type_path.path) {
            Ok(matching) => matching,
            Err(err) => return self.errors.push(err),
        };
        let num_matching = matching
            .iter()
            .filter(|i| self.resolution_graph.inner[**i].is_type())
            .count();
        if num_matching != 1 {
            let file = self.resolution_graph.file(self.scope);
            let ident = type_path.path.segments.iter().last().unwrap().ident.clone();
            if num_matching == 0 {
                self.errors.push(
                    UnexpectedItemError {
                        file,
                        ident,
                        expected_hint: ItemHint::Type,
                        actual_hint: matching
                            .first()
                            .and_then(|x| self.resolution_graph.inner[*x].item_hint())
                            .unwrap_or(ItemHint::Item),
                    }
                    .into(),
                );
            } else if num_matching > 1 {
                self.errors.push(
                    DisambiguationError {
                        file,
                        ident,
                        src: AmbiguitySource::Item(ItemHint::Type),
                    }
                    .into(),
                );
            }
        }
    }
}
