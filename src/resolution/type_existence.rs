use std::collections::HashSet;

use petgraph::graph::NodeIndex;
use syn::{
    visit::Visit, AngleBracketedGenericArguments, Fields, GenericArgument, Generics, Item,
    ItemConst, ItemEnum, ItemFn, ItemImpl, ItemMod, ItemStruct, ItemTrait, ItemType, PathArguments,
    PathSegment, TraitBound, TypeParam, TypeParamBound, TypePath,
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
impl<'a, 'c, 'ast> Visit<'c> for TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn visit_item_mod(&mut self, item_mod: &'c ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_impl(&mut self, item_impl: &'c ItemImpl) {
        self.visit_generics(&item_impl.generics);
        // TODO: visit items inside the item impl
    }

    fn visit_item_fn(&mut self, item_fn: &'c ItemFn) {
        self.visit_signature(&item_fn.sig);
        // TODO: does this need some special handling for body?
        // also: can inferrability be handled now?, that would be cool
    }

    /// Grab the current generics
    /// Check that all references to traits, etc. exist
    fn visit_generics(&mut self, generics: &'c Generics) {
        self.generics.push(generics);
        for type_param in generics.type_params() {
            self.visit_type_param(type_param);
            if let Some(default) = &type_param.default {
                self.visit_type(default);
            }
        }
    }

    fn visit_type_param(&mut self, type_param: &'c TypeParam) {
        for bound in &type_param.bounds {
            self.visit_type_param_bound(bound);
        }
    }

    fn visit_type_param_bound(&mut self, bound: &'c TypeParamBound) {
        if let TypeParamBound::Trait(TraitBound { path, .. }) = bound {
            let res = {
                let path_finder = PathFinder {
                    scope_graph: &self.scope_graph,
                };
                path_finder.find_at_path(self.scope, &path)
            };
            match res {
                Ok(matching) => {
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
                }
                Err(err) => self.errors.push(err),
            }
            if let Some(PathSegment {
                arguments: PathArguments::AngleBracketed(bracketed),
                ..
            }) = path.segments.last()
            {
                for arg in &bracketed.args {
                    match arg {
                        GenericArgument::Type(t) => self.visit_type(t),
                        GenericArgument::Binding(b) => self.visit_type(&b.ty),
                        GenericArgument::Constraint(c) => {
                            for bound in &c.bounds {
                                self.visit_type_param_bound(bound);
                            }
                        }
                        GenericArgument::Const(c) => {
                            todo!("const params not yet supported: {:?}", c);
                        }
                        GenericArgument::Lifetime(_) => {}
                    }
                }
            }
        }
    }

    fn visit_type_path(&mut self, type_path: &'c TypePath) {
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

    fn visit_fields(&mut self, fields: &'c Fields) {
        // Check generics, then check items in scope
        for field in fields.iter() {
            self.visit_type(&field.ty);
        }
    }
}
