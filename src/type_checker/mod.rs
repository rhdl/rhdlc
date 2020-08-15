use std::rc::Rc;

use petgraph::{graph::NodeIndex, Direction, Graph};
use syn::{
    visit::Visit, Fields, FieldsNamed, FieldsUnnamed, ItemConst, ItemEnum, ItemImpl, ItemMod,
    ItemStruct, ItemTrait, ItemType,
};

use crate::error::TypeError;
use crate::find_file::File;
use crate::resolution::{Node, ScopeGraph};

struct TypeExistenceChecker<'a, 'ast> {
    scope_graph: &'a ScopeGraph<'ast>,
    scope: NodeIndex,
    file: Rc<File>,
    errors: Vec<TypeError>,
}

impl<'a, 'ast> TypeExistenceChecker<'a, 'ast> {
    fn visit_all(scope_graph: &'a ScopeGraph<'ast>, errors: &'a mut Vec<TypeError>) {
        for scope in scope_graph.node_indices() {
            let (mut checker, items) = match &scope_graph[scope] {
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
                    Self {
                        scope_graph,
                        scope,
                        file: content_file
                            .as_ref()
                            .map(Clone::clone)
                            .unwrap_or_else(|| file.clone()),
                        errors: vec![],
                    },
                    items,
                ),
                Node::Root { file, .. } => (
                    Self {
                        scope_graph,
                        scope,
                        file: file.clone(),
                        errors: vec![],
                    },
                    &file.syn.items,
                ),
                _ => continue,
            };

            items.iter().for_each(|item| checker.visit_item(item));
            errors.append(&mut checker.errors);
        }
    }
}

impl<'a, 'ast> Visit<'ast> for TypeExistenceChecker<'a, 'ast> {
    fn visit_item_mod(&mut self, item_mod: &ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    /// * Resolve generics
    /// * For each field
    ///     * get the type
    ///     * recurse over the type and resolve the paths in it
    ///         * see if the type points to generics
    fn visit_item_struct(&mut self, item_struct: &ItemStruct) {
        // Steps:
        // 1. get field iterator
        // 2. for each field
        //  2.1. for each
        // check struct fields & type parameters
    }

    fn visit_item_enum(&mut self, item_enum: &ItemEnum) {
        // check enum fields & type parameters
    }

    fn visit_item_trait(&mut self, item_trait: &ItemTrait) {
        // check trait fields & type parameters
        // traits have non-function members
    }

    fn visit_item_const(&mut self, item_const: &'ast ItemConst) {}

    fn visit_item_type(&mut self, item_type: &'ast ItemType) {}

    fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {}
}
