use std::collections::HashSet;
use std::rc::Rc;

use petgraph::{graph::NodeIndex, Direction, Graph};
use syn::{
    visit::Visit, Fields, FieldsNamed, FieldsUnnamed, Generics, ItemConst, ItemEnum, ItemImpl,
    ItemMod, ItemStruct, ItemTrait, ItemType, TraitBound, TypeParamBound,
};

use crate::error::ResolutionError;
use crate::find_file::File;
use crate::resolution::{Node, ScopeGraph};

pub struct TypeExistenceChecker<'a, 'ast> {
    scope_graph: &'a ScopeGraph<'ast>,
    errors: Vec<ResolutionError>,
    visited_uses: HashSet<NodeIndex>,

    ctx: TypeExistenceCheckerContext<'ast>,
}

struct TypeExistenceCheckerContext<'ast> {
    scope: NodeIndex,
    file: Rc<File>,
    impl_generics: Option<&'ast Generics>,
    generics: Option<&'ast Generics>,
}

impl<'a, 'ast> TypeExistenceChecker<'a, 'ast> {
    pub fn visit_all(
        scope_graph: &'a ScopeGraph<'ast>,
        errors: &'a mut Vec<ResolutionError>,
        visited_uses: &'a mut HashSet<NodeIndex>,
    ) {
        for scope in scope_graph.node_indices() {
            let (file, items) = match &scope_graph[scope] {
                Node::Mod {
                    file,
                    content_file,
                    item_mod:
                        ItemMod {
                            content: Some((_, items)),
                            ..
                        },
                    ..
                } => (
                    content_file
                        .as_ref()
                        .map(Clone::clone)
                        .unwrap_or_else(|| Clone::clone(file)),
                    items,
                ),
                Node::Root { file, .. } => (file.clone(), &file.syn.items),
                _ => continue,
            };

            let mut checker = Self {
                scope_graph,
                errors: vec![],
                ctx: TypeExistenceCheckerContext {
                    file,
                    scope,
                    impl_generics: None,
                    generics: None,
                },
                visited_uses: Clone::clone(visited_uses),
            };

            items.iter().for_each(|item| checker.visit_item(item));
            errors.append(&mut checker.errors);
            visited_uses.union(&checker.visited_uses);
        }
    }
}

impl<'a, 'ast> Visit<'ast> for TypeExistenceChecker<'a, 'ast> {
    fn visit_item_mod(&mut self, item_mod: &'ast ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    // /// * Resolve generics
    // /// * For each field
    // ///     * get the type
    // ///     * recurse over the type and resolve the paths in it
    // ///         * see if the type points to generics
    // fn visit_item_struct(&mut self, item_struct: &'ast ItemStruct) {
    //     // Steps:
    //     // 1. get field iterator
    //     // 2. for each field
    //     //  2.1. for each
    //     // check struct fields & type parameters
    //     self.ctx.generics = Some(&item_struct.generics);
    //     self.visit_fields(&item_struct.fields);
    // }

    // fn visit_item_enum(&mut self, item_enum: &'ast ItemEnum) {
    //     // check enum fields & type parameters
    //     self.ctx.generics
    // }

    fn visit_item_trait(&mut self, item_trait: &'ast ItemTrait) {
        // check trait fields & type parameters
        // traits have non-function members
    }

    /// Catch the current generics
    /// Check that any bounds exist
    fn visit_generics(&mut self, generics: &'ast Generics) {
        self.ctx.generics = Some(generics);
        for type_param in generics.type_params() {
            for bound in &type_param.bounds {
                match bound {
                    TypeParamBound::Trait(TraitBound { path, .. }) => {
                        todo!("make sure the path resolves to a single trait: {:?}", path);
                    }
                    _ => {}
                }
            }
        }
    }

    fn visit_fields(&mut self, fields: &'ast Fields) {
        // Check generics, then check items in scope
    }

    fn visit_item_const(&mut self, item_const: &'ast ItemConst) {}

    fn visit_item_type(&mut self, item_type: &'ast ItemType) {}

    fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {}
}
