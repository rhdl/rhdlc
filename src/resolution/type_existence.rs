use fnv::FnvHashMap as HashMap;

use petgraph::graph::NodeIndex;
use syn::{
    visit::Visit, File, Generics, ImplItemMethod, ImplItemType, Item, ItemFn, ItemImpl, ItemMod,
    Path, PathSegment, Receiver, TraitBound, TraitItemMethod, TraitItemType, TypeParam,
    TypeParamBound, TypePath,
};

use crate::error::{
    AmbiguitySource, DisambiguationError, ItemHint, ResolutionError, UnresolvedItemError,
};
use crate::resolution::{path::PathFinder, Node, ScopeGraph};

pub struct TypeExistenceChecker<'a, 'ast> {
    pub scope_graph: &'a ScopeGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
}

struct TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    scope_graph: &'a ScopeGraph<'ast>,
    errors: &'a mut Vec<ResolutionError>,
    scope: NodeIndex,
    generics: Vec<&'c Generics>,
    found_type_paths: HashMap<&'c Path, Vec<NodeIndex>>,
}

impl<'a, 'ast> TypeExistenceChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for scope in self.scope_graph.node_indices() {
            if !self.scope_graph[scope].is_nameless_scope() {
                let mut ctx_checker = TypeExistenceCheckerVisitor {
                    scope_graph: self.scope_graph,
                    errors: self.errors,
                    scope: scope,
                    generics: Default::default(),
                    found_type_paths: Default::default(),
                };
                for node in self.scope_graph.neighbors(scope) {
                    ctx_checker.scope = node;
                    self.scope_graph[node].visit(&mut ctx_checker);
                }
                ctx_checker.generics.clear();
            }
        }
    }
}

impl<'a, 'c, 'ast> TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn find_trait(&mut self, path: &Path) -> Result<NodeIndex, ResolutionError> {
        let res = {
            let mut path_finder = PathFinder {
                scope_graph: &self.scope_graph,
                visited_glob_scopes: Default::default(),
            };
            path_finder.find_at_path(self.scope, &path)
        };
        res.and_then(|matching| {
            // Check that there is a single trait match
            let num_matching = matching
                .iter()
                .filter(|i| self.scope_graph[**i].is_trait())
                .count();
            if num_matching != 1 {
                let file = Node::file(self.scope_graph, self.scope).clone();
                let previous_ident = path
                    .segments
                    .iter()
                    .rev()
                    .skip(1)
                    .next()
                    .map(|seg| seg.ident.clone());
                let ident = path.segments.iter().last().unwrap().ident.clone();
                if num_matching == 0 {
                    Err(UnresolvedItemError {
                        file,
                        previous_ident,
                        unresolved_ident: ident,
                        hint: ItemHint::Trait,
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
                    .filter(|i| self.scope_graph[**i].is_trait())
                    .next()
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
        for item in item_impl.items.iter() {
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

        if !self.found_type_paths.contains_key(&type_path.path) {
            let mut path_finder = PathFinder {
                scope_graph: &self.scope_graph,
                visited_glob_scopes: Default::default(),
            };
            match path_finder.find_at_path(self.scope, &type_path.path) {
                Ok(matching) => {
                    self.found_type_paths.insert(&type_path.path, matching);
                }
                Err(err) => return self.errors.push(err),
            }
            // TODO: path tracing can be expensive in modules that have a lot of items
            // so instead, it might be better to have the path finder return a common
            // visibility issue (privacy or unresolved or disambiguation)
            // which can then be converted into the appropriate error to display to the user
            // this doesn't matter short-term because it only affects crates with many many items in a scope
            // i.e. winapi
        }
        let matching = self.found_type_paths.get(&type_path.path).unwrap();
        let num_matching = matching
            .iter()
            .filter(|i| self.scope_graph[**i].is_type())
            .count();
        if num_matching != 1 {
            let file = Node::file(self.scope_graph, self.scope).clone();
            let previous_ident = type_path
                .path
                .segments
                .iter()
                .rev()
                .skip(1)
                .next()
                .map(|seg| seg.ident.clone());
            let ident = type_path.path.segments.iter().last().unwrap().ident.clone();
            if num_matching == 0 {
                self.errors.push(
                    UnresolvedItemError {
                        file,
                        previous_ident,
                        unresolved_ident: ident,
                        hint: ItemHint::Trait,
                    }
                    .into(),
                );
            } else if num_matching > 1 {
                self.errors.push(
                    DisambiguationError {
                        file,
                        ident,
                        src: AmbiguitySource::Item(ItemHint::Item),
                    }
                    .into(),
                );
            }
        }
    }
}
