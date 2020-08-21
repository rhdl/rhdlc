use std::collections::HashSet;

use petgraph::graph::NodeIndex;
use syn::{
    visit::Visit, Generics, ImplItemMethod, ImplItemType, Item, ItemFn, ItemImpl, ItemMod, Path,
    PathSegment, TraitBound, TraitItemMethod, TraitItemType, TypeParam, TypeParamBound, TypePath,
};

use crate::error::ResolutionError;
use crate::resolution::{path::PathFinder, Node, ScopeGraph};

pub struct TypeExistenceChecker<'a, 'ast> {
    pub scope_graph: &'a ScopeGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
    pub visited_uses: &'a mut HashSet<NodeIndex>,
}

struct TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    pub scope_graph: &'a ScopeGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
    pub visited_uses: &'a mut HashSet<NodeIndex>,
    scope: NodeIndex,
    generics: Vec<&'c Generics>,
}

impl<'a, 'ast> TypeExistenceChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for node in self.scope_graph.node_indices() {
            let mut ctx_checker = TypeExistenceCheckerVisitor {
                scope_graph: self.scope_graph,
                errors: self.errors,
                visited_uses: self.visited_uses,
                scope: node,
                generics: vec![],
            };
            self.scope_graph[node].visit(&mut ctx_checker);
        }
    }
}
impl<'a, 'c, 'ast> TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn find_trait(&self, path: &Path) -> Result<NodeIndex, ResolutionError> {
        let res = {
            let path_finder = PathFinder {
                scope_graph: &self.scope_graph,
            };
            path_finder.find_at_path(self.scope, &path)
        };
        res.and_then(|matching| {
            // Check that there is a single trait match
            let num_matching = matching
                .iter()
                .filter(|i| self.scope_graph[**i].is_trait())
                .count();
            if num_matching == 0 {
                todo!("no such trait");
            } else if num_matching > 1 {
                todo!("ambiguous trait name");
            }
            Ok(*matching.first().unwrap())
        })
    }
}
impl<'a, 'c, 'ast> Visit<'c> for TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
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
        for item in item_impl.items.iter() {
            // TODO: pull out the individual visits so the generics can be popped off
            self.visit_impl_item(item);
        }
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
        if let Some(ident) = type_path.path.get_ident() {
            if ident == "Self" {
                let is_impl_or_trait = match self.scope_graph[self.scope] {
                    Node::Impl { .. } | Node::Trait { .. } => true,
                    _ => false,
                };
                if is_impl_or_trait {
                    return;
                }
            }
            let is_type_param = self.generics.iter().rev().any(|generic| {
                generic
                    .type_params()
                    .any(|type_param| type_param.ident == *ident)
            });
            if is_type_param {
                return;
            }
        }
        let res = {
            let path_finder = PathFinder {
                scope_graph: &self.scope_graph,
            };
            path_finder.find_at_path(self.scope, &type_path.path)
        };
        match res {
            Ok(matching) => {
                // Check that there is a single type match
                // TODO: need *concrete* types + generics here.
                // * is_type includes type aliases which could actually point to trait
                // * also need to skip self so the type alias doesn't point to itself
                // * also avoid T that uses T in its type param bound
                // TODO: I'd like a way to aggressively gate duplicate ident errors early on
                // so they aren't being seen here
                let num_matching = matching
                    .iter()
                    .filter(|i| self.scope_graph[**i].is_type())
                    .count();
                if num_matching == 0 {
                    todo!("no such type");
                } else if num_matching > 1 {
                    todo!("ambiguous type");
                }
            }
            Err(err) => self.errors.push(err),
        }
    }
}
