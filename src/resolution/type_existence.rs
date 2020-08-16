use std::collections::HashSet;
use std::rc::Rc;

use petgraph::{graph::NodeIndex, Direction, Graph};
use syn::{
    visit::Visit, Fields, FieldsNamed, FieldsUnnamed, Generics, Item, ItemConst, ItemEnum,
    ItemImpl, ItemMod, ItemStruct, ItemTrait, ItemType, TraitBound, TypeParamBound,
};

use crate::error::ResolutionError;
use crate::find_file::File;
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
    impl_generics: Option<&'c Generics>,
    generics: Option<&'c Generics>,
}

impl<'a, 'ast> TypeExistenceChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for node in self.scope_graph.node_indices() {
            let mut ctx_checker = TypeExistenceCheckerVisitor {
                scope_graph: self.scope_graph,
                errors: self.errors,
                visited_uses: self.visited_uses,
                scope: node,
                impl_generics: None,
                generics: None,
            };
            self.scope_graph[node].visit(&mut ctx_checker);
        }
    }
}
impl<'a, 'c, 'ast> Visit<'c> for TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn visit_item_mod(&mut self, item_mod: &'c ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_impl(&mut self, item_impl: &'c ItemImpl) {}

    /// Grab the current generics
    /// Check that any bounds exist
    fn visit_generics(&mut self, generics: &'c Generics) {
        self.generics = Some(generics);

        for type_param in generics.type_params() {
            for bound in &type_param.bounds {
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
                    // if let Some(PathSegment{arguments: PathArguments::}) = path.segments.last() {

                    // }
                }
            }
        }
    }

    fn visit_fields(&mut self, fields: &'c Fields) {
        // Check generics, then check items in scope
    }
}
