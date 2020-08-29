use fnv::FnvHashMap as HashMap;
use log::error;
use petgraph::{graph::NodeIndex, visit::EdgeRef};
use syn::{
    spanned::Spanned, visit::Visit, File, Ident, ItemConst, ItemEnum, ItemExternCrate, ItemFn,
    ItemImpl, ItemMacro, ItemMacro2, ItemMod, ItemStatic, ItemStruct, ItemTrait, ItemTraitAlias,
    ItemType, ItemUnion, ItemUse,
};

use super::{FileGraph, Node, ScopeGraph};
use crate::error::{ResolutionError, UnsupportedError};

pub struct ScopeBuilder<'a, 'ast> {
    pub file_graph: &'ast FileGraph,
    pub scope_graph: &'a mut ScopeGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
    pub file_ancestry: Vec<NodeIndex>,
    pub scope_ancestry: Vec<NodeIndex>,
}

impl<'a, 'ast> Visit<'ast> for ScopeBuilder<'a, 'ast> {
    fn visit_file(&mut self, file: &'ast File) {
        file.items.iter().for_each(|item| self.visit_item(item));
    }

    fn visit_item_mod(&mut self, item_mod: &'ast ItemMod) {
        if let Some((_, items)) = &item_mod.content {
            let mod_idx = self.scope_graph.add_node(Node::Mod {
                item_mod,
                exports: HashMap::default(),
                file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                content_file: None,
            });
            let parent = self.scope_ancestry.last().unwrap();
            self.scope_graph
                .add_edge(*parent, mod_idx, "mod".to_string());
            self.scope_ancestry.push(mod_idx);
            items.iter().for_each(|i| self.visit_item(i));
            self.scope_ancestry.pop();
        } else {
            let is_fn = self.scope_ancestry.iter().any(|ancestor| {
                if let Node::Fn { .. } = self.scope_graph[*ancestor] {
                    true
                } else {
                    false
                }
            });
            if is_fn {
                self.errors.push(
                    UnsupportedError {
                        file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                        span: item_mod.ident.span(),
                        reason: "RHDL does not support modules without content inside functions",
                    }
                    .into(),
                );
            }
            let mut full_ident_path: Vec<Ident> = self
                .scope_ancestry
                .iter()
                .filter_map(|scope_ancestor| match &self.scope_graph[*scope_ancestor] {
                    Node::Mod { item_mod, .. } => Some(item_mod.ident.clone()),
                    Node::Root { name, .. } => {
                        error!("this needs to be an ident: {}", name);
                        None
                    }
                    _ => None,
                })
                .collect();
            full_ident_path.push(item_mod.ident.clone());

            let file_index = self.file_ancestry.last().and_then(|parent| {
                self.file_graph
                    .edges(*parent)
                    .filter(|edge| full_ident_path.ends_with(edge.weight()))
                    .max_by_key(|edge| edge.weight().len())
                    .map(|edge| edge.target())
            });

            if let Some(file_index) = file_index {
                let content_file = self.file_graph[file_index].clone();
                let mod_idx = self.scope_graph.add_node(Node::Mod {
                    item_mod,
                    exports: HashMap::default(),
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                    content_file: Some(content_file),
                });
                if let Some(parent) = self.scope_ancestry.last() {
                    self.scope_graph
                        .add_edge(*parent, mod_idx, "mod".to_string());
                }
                self.scope_ancestry.push(mod_idx);
                self.file_ancestry.push(file_index);
                self.visit_file(&self.file_graph[file_index].syn);
                self.file_ancestry.pop();
                self.scope_ancestry.pop();
            } else {
                let mod_idx = self.scope_graph.add_node(Node::Mod {
                    item_mod,
                    exports: HashMap::default(),
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                    content_file: None,
                });
                if let Some(parent) = self.scope_ancestry.last() {
                    self.scope_graph
                        .add_edge(*parent, mod_idx, "mod".to_string());
                }
            }
        }
    }

    fn visit_item_use(&mut self, item_use: &'ast ItemUse) {
        let use_idx = self.scope_graph.add_node(Node::Use {
            item_use,
            imports: HashMap::default(),
        });
        self.scope_graph.add_edge(
            *self.scope_ancestry.last().unwrap(),
            use_idx,
            "use".to_string(),
        );
    }

    fn visit_item_fn(&mut self, item_fn: &'ast ItemFn) {
        let item_idx = self.scope_graph.add_node(Node::Fn { item_fn });
        let parent = self.scope_ancestry.last().unwrap();
        self.scope_graph
            .add_edge(*parent, item_idx, "fn".to_string());
        self.scope_ancestry.push(item_idx);
        self.visit_block(item_fn.block.as_ref());
        self.scope_ancestry.pop();
    }

    fn visit_item_const(&mut self, item_const: &'ast ItemConst) {
        let item_idx = self.scope_graph.add_node(Node::Const { item_const });
        let parent = self.scope_ancestry.last().unwrap();
        self.scope_graph
            .add_edge(*parent, item_idx, "const".to_string());
    }

    fn visit_item_type(&mut self, item_type: &'ast ItemType) {
        let item_idx = self.scope_graph.add_node(Node::Type { item_type });
        let parent = self.scope_ancestry.last().unwrap();
        self.scope_graph
            .add_edge(*parent, item_idx, "type alias".to_string());
    }

    fn visit_item_trait(&mut self, item_trait: &'ast ItemTrait) {
        let item_idx = self.scope_graph.add_node(Node::Trait {
            item_trait,
            impls: HashMap::default(),
        });
        let parent = self.scope_ancestry.last().unwrap();
        self.scope_graph
            .add_edge(*parent, item_idx, "trait".to_string());
    }

    fn visit_item_struct(&mut self, item_struct: &'ast ItemStruct) {
        let item_idx = self.scope_graph.add_node(Node::Struct {
            item_struct,
            impls: HashMap::default(),
        });
        let parent = self.scope_ancestry.last().unwrap();
        self.scope_graph
            .add_edge(*parent, item_idx, "struct".to_string());
    }

    fn visit_item_enum(&mut self, item_enum: &'ast ItemEnum) {
        let item_idx = self.scope_graph.add_node(Node::Enum {
            item_enum,
            impls: HashMap::default(),
        });
        let parent = self.scope_ancestry.last().unwrap();
        self.scope_graph
            .add_edge(*parent, item_idx, "enum".to_string());
    }

    fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {
        let impl_idx = self.scope_graph.add_node(Node::Impl { item_impl });
        let parent = self.scope_ancestry.last().unwrap();
        self.scope_graph
            .add_edge(*parent, impl_idx, "impl".to_string());
    }

    fn visit_item_macro(&mut self, item_macro: &'ast ItemMacro) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                span: item_macro.ident.span(),
                reason: "RHDL does not support macros (yet)",
            }
            .into(),
        );
    }

    fn visit_item_macro2(&mut self, item_macro2: &'ast ItemMacro2) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                span: item_macro2.ident.span(),
                reason:
                    "RHDL does not support declarative macros, as they are not stabilized in Rust",
            }
            .into(),
        );
    }

    fn visit_item_static(&mut self, item_static: &'ast ItemStatic) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                span: item_static.ident.span(),
                reason:
                    "RHDL does not support declarative macros, as they are not stabilized in Rust",
            }
            .into(),
        );
    }

    fn visit_item_union(&mut self, item_union: &'ast ItemUnion) {
        self.errors.push(UnsupportedError {
            file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
            span: item_union.ident.span(),
            reason: "RHDL cannot support unions and other unsafe code: safety is not yet formally defined"
        }.into());
    }

    fn visit_item_trait_alias(&mut self, item_trait_alias: &'ast ItemTraitAlias) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                span: item_trait_alias.ident.span(),
                reason:
                    "RHDL does not support trait aliases as they are still experimental in Rust",
            }
            .into(),
        );
    }

    fn visit_item_extern_crate(&mut self, item_extern_crate: &'ast ItemExternCrate) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                span: item_extern_crate.ident.span(),
                reason: "RHDL does not support Rust 2015 syntax, you can safely remove this. :)",
            }
            .into(),
        );
    }
}
