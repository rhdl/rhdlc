//! TODO: error for items that could do visibility hole punching:
//! * function with params that are not as visible as the function
//! * structs with members that are not as visible as their types (?)

use rhdl::{
    ast::{
        Block, File, GenericParam, GenericParamType, Generics, Item, ItemArch, ItemEntity, ItemFn,
        ItemImpl, ItemMod, ItemTrait, ItemType, PathSegment, TypePath,
    },
    visit::Visit,
};

use crate::error::*;
use crate::resolution::{path::r#type::PathFinder, ResolutionGraph, ResolutionIndex};

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
            if self.resolution_graph[scope].is_type_existence_checking_candidate() {
                // Cannot directly visit these because RHDL needs the generics from the parent scope
                if self.resolution_graph[scope]
                    .parent()
                    .map(|parent| self.resolution_graph[parent].is_trait_or_impl_or_arch())
                    .unwrap_or_default()
                {
                    continue;
                }
                let mut ctx_checker = TypeExistenceCheckerVisitor {
                    resolution_graph: self.resolution_graph,
                    errors: self.errors,
                    scope,
                    generics: vec![],
                };
                self.resolution_graph[scope].visit(&mut ctx_checker);
            }
        }
    }
}

impl<'a, 'c, 'ast> TypeExistenceCheckerVisitor<'a, 'c, 'ast> {
    fn find_in_scope<F>(
        &self,
        path: &TypePath,
        filter: F,
        hint: ItemHint,
    ) -> Result<ResolutionIndex, Diagnostic>
    where
        F: Fn(ResolutionIndex) -> bool,
    {
        // TODO: private trait in public trait declaration
        let found = {
            let mut path_finder = PathFinder {
                resolution_graph: &self.resolution_graph,
                visited_glob_scopes: Default::default(),
            };
            path_finder.find_at_path(self.scope, &path, &self.generics)
        }?;
        // Check that there is a single match
        let matching = found
            .iter()
            .copied()
            .filter(|i| filter(*i))
            .collect::<Vec<ResolutionIndex>>();
        if matching.len() != 1 {
            let file = self.resolution_graph.file(self.scope);
            let ident = &path.segments.last().as_ref().unwrap().ident;
            if matching.is_empty() {
                Err(unexpected_item(
                    file,
                    ident,
                    hint,
                    found
                        .first()
                        .and_then(|x| self.resolution_graph[*x].item_hint())
                        .unwrap(),
                ))
            } else {
                Err(disambiguation_needed(
                    file,
                    ident,
                    AmbiguitySource::Item(hint),
                ))
            }
        } else {
            Ok(*matching.first().unwrap())
        }
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
        if let Some(generics) = &item_impl.generics {
            self.visit_generics(generics);
        }
        if let Some((of_ty, _for)) = &item_impl.of {
            if let Err(err) = self.find_in_scope(
                of_ty,
                |i| self.resolution_graph[i].is_trait(),
                ItemHint::Trait,
            ) {
                self.errors.push(err)
            }
            self.visit_type_path(of_ty)
        }
        self.visit_type(&item_impl.ty);
        item_impl
            .items
            .iter()
            .for_each(|item| self.visit_impl_item(item));
        if item_impl.generics.is_some() {
            self.generics.pop();
        }
    }

    fn visit_item_arch(&mut self, item_arch: &'c ItemArch) {
        if let Some(generics) = &item_arch.generics {
            self.visit_generics(generics);
        }
        self.visit_type_path(&item_arch.entity);
        item_arch
            .items
            .iter()
            .for_each(|item| self.visit_arch_item(item));
        if item_arch.generics.is_some() {
            self.generics.pop();
        }
    }

    fn visit_item_entity(&mut self, item_entity: &'c ItemEntity) {
        if let Some(generics) = &item_entity.generics {
            self.visit_generics(generics);
        }
        item_entity
            .ports
            .iter()
            .for_each(|port| self.visit_port(port));
        if item_entity.generics.is_some() {
            self.generics.pop();
        }
    }

    fn visit_item_trait(&mut self, item_trait: &'c ItemTrait) {
        if let Some(generics) = &item_trait.generics {
            self.visit_generics(generics);
        }
        if let Some((_, super_traits)) = &item_trait.super_traits {
            for super_trait in super_traits.iter() {
                if let Err(err) = self.find_in_scope(
                    super_trait,
                    |i| self.resolution_graph[i].is_trait(),
                    ItemHint::Trait,
                ) {
                    self.errors.push(err)
                }
            }
        }
        item_trait
            .items
            .iter()
            .for_each(|item| self.visit_trait_item(item));
        self.generics.pop();
    }

    fn visit_item_type(&mut self, item_type: &'c ItemType) {
        if let Some(ref generics) = item_type.generics {
            self.visit_generics(generics);
        }
        self.visit_type(&item_type.ty);
        if item_type.generics.is_some() {
            self.generics.pop();
        }
    }

    fn visit_item_fn(&mut self, item_fn: &'c ItemFn) {
        self.visit_sig(&item_fn.sig);
        self.visit_block(&item_fn.block);
        // TODO: can inferrability be handled now?, that would be cool
        if item_fn.sig.generics.is_some() {
            self.generics.pop();
        }
    }

    fn visit_block(&mut self, block: &'c Block) {
        for stmt in block.statements.iter() {
            use rhdl::ast::Stmt::*;
            match stmt {
                Local(l) => self.visit_stmt_local(l),
                Expr(e) => self.visit_stmt_expr(e),
                // Skip local items
                Item(_) => continue,
            }
        }
    }

    fn visit_generics(&mut self, generics: &'c Generics) {
        self.generics.push(generics);
        for generic_param in generics.params.iter() {
            self.visit_generic_param(generic_param);
        }
    }

    fn visit_generic_param_type(&mut self, generic_type_param: &'c GenericParamType) {
        if let Some((_, bounds)) = &generic_type_param.bounds {
            for type_path in bounds.iter() {
                if let Err(err) = self.find_in_scope(
                    type_path,
                    |i| self.resolution_graph[i].is_trait(),
                    ItemHint::Trait,
                ) {
                    self.errors.push(err)
                }
                for seg in type_path.segments.iter() {
                    self.visit_path_segment(seg);
                }
            }
        }
    }

    fn visit_type_path(&mut self, type_path: &'c TypePath) {
        if let Err(err) = self.find_in_scope(
            &type_path,
            |i| self.resolution_graph[i].is_type(),
            ItemHint::Type,
        ) {
            self.errors.push(err);
        }
    }
}
