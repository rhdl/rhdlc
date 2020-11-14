use codespan_reporting::diagnostic::Diagnostic;
use fxhash::FxHashMap as HashMap;

use rhdl::{
    ast::{
        Ident, ItemArch, ItemConst, ItemEntity, ItemEnum, ItemFn, ItemImpl, ItemMod, ItemStruct,
        ItemTrait, ItemTraitAlias, ItemType, ItemUse, ModContent, NamedField, UnnamedField,
        Variant,
    },
    visit::Visit,
};

use super::{graph::*, FileGraph, FileId};

pub struct ScopeBuilder<'a, 'ast> {
    pub file_graph: &'ast FileGraph,
    pub resolution_graph: &'a mut ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<Diagnostic<FileId>>,
    pub file_ancestry: Vec<FileId>,
    pub scope_ancestry: Vec<ResolutionIndex>,
}

impl<'a, 'ast> Visit<'ast> for ScopeBuilder<'a, 'ast> {
    fn visit_item_mod(&mut self, item_mod: &'ast ItemMod) {
        let parent = *self.scope_ancestry.last().unwrap();
        if let ModContent::Here(here) = &item_mod.content {
            let mod_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
                branch: Branch::Mod(item_mod),
                parent,
                children: HashMap::default(),
            });
            self.resolution_graph.add_child(parent, mod_idx);
            self.scope_ancestry.push(mod_idx);
            here.items.iter().for_each(|i| self.visit_item(i));
            self.scope_ancestry.pop();
        } else {
            let is_fn = self.scope_ancestry.iter().any(|ancestor| {
                if let ResolutionNode::Branch {
                    branch: Branch::Fn { .. },
                    ..
                } = self.resolution_graph[*ancestor]
                {
                    true
                } else {
                    false
                }
            });
            if is_fn {
                self.errors
                    .push(crate::error::module_with_external_file_in_fn(
                        *self.file_ancestry.last().unwrap(),
                        &item_mod,
                    ));
            }
            let mut full_ident_path: Vec<Ident> = self
                .scope_ancestry
                .iter()
                .filter_map(
                    |scope_ancestor| match &self.resolution_graph[*scope_ancestor] {
                        ResolutionNode::Branch {
                            branch: Branch::Mod(item_mod, ..),
                            ..
                        } => Some(item_mod.ident.clone()),
                        ResolutionNode::Root { .. } => None,
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
                let mod_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
                    branch: Branch::Mod(item_mod),
                    parent,
                    children: HashMap::default(),
                });
                self.resolution_graph
                    .content_files
                    .insert(mod_idx, file_index);
                self.resolution_graph.add_child(parent, mod_idx);
                if let Some(parsed) = &self.file_graph[file_index].parsed {
                    self.scope_ancestry.push(mod_idx);
                    self.file_ancestry.push(file_index);
                    self.visit_file(parsed);
                    self.file_ancestry.pop();
                    self.scope_ancestry.pop();
                }
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

    fn visit_item_const(&mut self, item_const: &'ast ItemConst) {
        let parent = *self.scope_ancestry.last().unwrap();
        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::Const(item_const),
            parent,
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
        self.visit_block(&item_fn.block);
        self.scope_ancestry.pop();
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
        // self.scope_ancestry.push(item_idx);
        // item_trait
        //     .items
        //     .iter()
        //     .for_each(|trait_item| self.visit_trait_item(trait_item));
        // self.scope_ancestry.pop();
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
        self.visit_fields(&item_struct.fields);
        self.scope_ancestry.pop();
    }

    fn visit_named_field(&mut self, field: &'ast NamedField) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::NamedField(field),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_unnamed_field(&mut self, field: &'ast UnnamedField) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::UnnamedField(field),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
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

    fn visit_variant(&mut self, variant: &'ast Variant) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Variant(variant),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
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

    fn visit_item_entity(&mut self, item_entity: &'ast ItemEntity) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::Entity(item_entity),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx);
    }

    fn visit_item_arch(&mut self, item_arch: &'ast ItemArch) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Branch {
            branch: Branch::Arch(item_arch),
            parent,
            children: HashMap::default(),
        });
        self.resolution_graph.add_child(parent, item_idx);
        self.scope_ancestry.push(item_idx);
        item_arch
            .items
            .iter()
            .for_each(|arch_item| self.visit_arch_item(arch_item));
        self.scope_ancestry.pop();
    }

    fn visit_item_trait_alias(&mut self, item_trait_alias: &'ast ItemTraitAlias) {
        let parent = *self.scope_ancestry.last().unwrap();

        let item_idx = self.resolution_graph.add_node(ResolutionNode::Leaf {
            leaf: Leaf::TraitAlias(item_trait_alias),
            parent,
        });
        self.resolution_graph.add_child(parent, item_idx)
    }
}
