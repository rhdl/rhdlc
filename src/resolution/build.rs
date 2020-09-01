use fnv::FnvHashMap as HashMap;
use log::error;
use syn::{
    spanned::Spanned, visit::Visit, Field, File, Ident, ImplItemConst, ImplItemMethod, ItemConst,
    ItemEnum, ItemExternCrate, ItemFn, ItemImpl, ItemMacro, ItemMacro2, ItemMod, ItemStatic,
    ItemStruct, ItemTrait, ItemTraitAlias, ItemType, ItemUnion, ItemUse, TraitItemConst,
    TraitItemMethod, Variant,
};

use super::{graph::*, FileGraph, FileGraphIndex};
use crate::error::{ResolutionError, UnsupportedError};

pub struct ScopeBuilder<'a, 'ast> {
    pub file_graph: &'ast FileGraph,
    pub resolution_graph: &'a mut ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
    pub file_ancestry: Vec<FileGraphIndex>,
    pub scope_ancestry: Vec<ResolutionIndex>,
}

impl<'a, 'ast> Visit<'ast> for ScopeBuilder<'a, 'ast> {
    fn visit_file(&mut self, file: &'ast File) {
        file.items.iter().for_each(|item| self.visit_item(item));
    }

    fn visit_item_mod(&mut self, item_mod: &'ast ItemMod) {
        let parent = *self.scope_ancestry.last().unwrap();
        if let Some((_, items)) = &item_mod.content {
            let mod_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
                branch: Branch::Mod(item_mod),
                parent,
                children: HashMap::default(),
            });
            self.resolution_graph.add_child(parent, mod_idx);
            self.scope_ancestry.push(mod_idx);
            items.iter().for_each(|i| self.visit_item(i));
            self.scope_ancestry.pop();
        } else {
            let is_fn = self.scope_ancestry.iter().any(|ancestor| {
                if let ResolutionNode::Branch {
                    branch: Branch::Fn { .. },
                    ..
                } = self.resolution_graph.inner[*ancestor]
                {
                    true
                } else {
                    false
                }
            });
            if is_fn {
                self.errors.push(
                    UnsupportedError {
                        file: self.file_graph.inner[*self.file_ancestry.last().unwrap()].clone(),
                        span: item_mod.ident.span(),
                        reason: "RHDL does not support modules without content inside functions",
                    }
                    .into(),
                );
            }
            let mut full_ident_path: Vec<Ident> = self
                .scope_ancestry
                .iter()
                .filter_map(
                    |scope_ancestor| match &self.resolution_graph.inner[*scope_ancestor] {
                        ResolutionNode::Branch {
                            branch: Branch::Mod(item_mod, ..),
                            ..
                        } => Some(item_mod.ident.clone()),
                        ResolutionNode::Root { name, .. } => {
                            error!("this needs to be an ident: {}", name);
                            None
                        }
                        _ => None,
                    },
                )
                .collect();
            full_ident_path.push(item_mod.ident.clone());

            let file_index = self
                .file_ancestry
                .last()
                .and_then(|parent| self.file_graph.children.get(&parent))
                .map(|children| {
                    children
                        .iter()
                        .filter(|(idents, _)| full_ident_path.ends_with(idents))
                        .max_by_key(|(idents, _)| idents.len())
                        .map(|(_, idx)| idx)
                        .cloned()
                })
                .flatten();

            if let Some(file_index) = file_index {
                let content_file = self.file_graph.inner[file_index].clone();
                let mod_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
                    branch: Branch::Mod(item_mod),
                    parent,
                    children: HashMap::default(),
                });
                self.resolution_graph
                    .content_files
                    .insert(mod_idx, content_file);
                self.resolution_graph.add_child(parent, mod_idx);
                self.scope_ancestry.push(mod_idx);
                self.file_ancestry.push(file_index);
                self.visit_file(&self.file_graph.inner[file_index].syn);
                self.file_ancestry.pop();
                self.scope_ancestry.pop();
            } else {
                let mod_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
                    branch: Branch::Mod(item_mod),
                    parent,
                    children: HashMap::default(),
                });
                self.resolution_graph.add_child(parent, mod_idx);
            }
        }
    }

    fn visit_item_use(&mut self, item_use: &'ast ItemUse) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Use(item_use),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_item_fn(&mut self, item_fn: &'ast ItemFn) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Fn(item_fn),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        self.visit_block(item_fn.block.as_ref());
        self.scope_ancestry.pop();
    }

    fn visit_impl_item_method(&mut self, impl_item_method: &'ast ImplItemMethod) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Fn(impl_item_method),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        self.visit_block(&impl_item_method.block);
        self.scope_ancestry.pop();
    }

    fn visit_trait_item_method(&mut self, trait_item_method: &'ast TraitItemMethod) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Fn(trait_item_method),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        if let Some(block) = trait_item_method.default.as_ref() {
            self.visit_block(block);
        }
        self.scope_ancestry.pop();
    }

    fn visit_item_const(&mut self, item_const: &'ast ItemConst) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::Const(item_const),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_impl_item_const(&mut self, impl_item_const: &'ast ImplItemConst) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::Const(impl_item_const),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_trait_item_const(&mut self, trait_item_const: &'ast TraitItemConst) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::Const(trait_item_const),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_item_type(&mut self, item_type: &'ast ItemType) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::Type(item_type),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_item_trait(&mut self, item_trait: &'ast ItemTrait) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Trait(item_trait),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        item_trait
            .items
            .iter()
            .for_each(|trait_item| self.visit_trait_item(trait_item));
        self.scope_ancestry.pop();
    }

    fn visit_item_struct(&mut self, item_struct: &'ast ItemStruct) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Struct(item_struct),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        item_struct
            .fields
            .iter()
            .for_each(|field| self.visit_field(field));
        self.scope_ancestry.pop();
    }

    fn visit_item_enum(&mut self, item_enum: &'ast ItemEnum) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Enum(item_enum),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        item_enum
            .variants
            .iter()
            .for_each(|variant| self.visit_variant(variant));
        self.scope_ancestry.pop();
    }

    fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Impl(item_impl),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        item_impl
            .items
            .iter()
            .for_each(|impl_item| self.visit_impl_item(impl_item));
        self.scope_ancestry.pop();
    }

    fn visit_variant(&mut self, variant: &'ast Variant) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Variant(variant),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_field(&mut self, field: &'ast Field) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::Field(field),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_item_macro(&mut self, item_macro: &'ast ItemMacro) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph.inner[*self.file_ancestry.last().unwrap()].clone(),
                span: item_macro.ident.span(),
                reason: "RHDL does not support macros (yet)",
            }
            .into(),
        );
    }

    fn visit_item_macro2(&mut self, item_macro2: &'ast ItemMacro2) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph.inner[*self.file_ancestry.last().unwrap()].clone(),
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
                file: self.file_graph.inner[*self.file_ancestry.last().unwrap()].clone(),
                span: item_static.ident.span(),
                reason:
                    "RHDL does not support declarative macros, as they are not stabilized in Rust",
            }
            .into(),
        );
    }

    fn visit_item_union(&mut self, item_union: &'ast ItemUnion) {
        self.errors.push(UnsupportedError {
            file: self.file_graph.inner[*self.file_ancestry.last().unwrap()].clone(),
            span: item_union.ident.span(),
            reason: "RHDL cannot support unions and other unsafe code: safety is not yet formally defined"
        }.into());
    }

    fn visit_item_trait_alias(&mut self, item_trait_alias: &'ast ItemTraitAlias) {
        self.errors.push(
            UnsupportedError {
                file: self.file_graph.inner[*self.file_ancestry.last().unwrap()].clone(),
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
                file: self.file_graph.inner[*self.file_ancestry.last().unwrap()].clone(),
                span: item_extern_crate.ident.span(),
                reason: "RHDL does not support Rust 2015 syntax, you can safely remove this. :)",
            }
            .into(),
        );
    }
}
