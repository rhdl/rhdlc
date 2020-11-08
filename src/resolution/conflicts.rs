use fxhash::FxHashMap as HashMap;

use rhdl::{
    ast::{
        Fields, File as RhdlFile, GenericParam, Generics, Ident, Item, ItemEnum, ItemMod, PatIdent,
        Sig,
    },
    visit::Visit,
};

use super::{Branch, ResolutionGraph, ResolutionIndex, ResolutionNode};
use crate::error::{Diagnostic, DuplicateHint};
use crate::find_file::FileId;

pub struct ConflictChecker<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<Diagnostic>,
}

struct ConflictCheckerVisitor<'a, 'ast> {
    resolution_graph: &'a ResolutionGraph<'ast>,
    errors: &'a mut Vec<Diagnostic>,
    file: FileId,
}

impl<'a, 'ast> ConflictChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for node in self.resolution_graph.node_indices() {
            let mut visitor = ConflictCheckerVisitor {
                resolution_graph: self.resolution_graph,
                errors: self.errors,
                file: self.resolution_graph.file(node),
            };
            self.resolution_graph.inner[node].visit(&mut visitor);
            let file = match &self.resolution_graph.inner[node] {
                ResolutionNode::Root { .. }
                | ResolutionNode::Branch {
                    branch: Branch::Impl(_),
                    ..
                }
                // | ResolutionNode::Branch {
                //     branch: Branch::Trait(_),
                //     ..
                // }
                 => self.resolution_graph.file(node),
                ResolutionNode::Branch {
                    branch: Branch::Mod(_),
                    ..
                } => {
                    if let Some(content_file) = self.resolution_graph.content_files.get(&node) {
                        *content_file
                    } else {
                        self.resolution_graph.file(node)
                    }
                }
                _ => continue,
            };
            self.find_name_conflicts_in(node, file);
        }
    }

    fn find_name_conflicts_in(&mut self, node: ResolutionIndex, file_id: FileId) {
        // Check the scope for conflicts
        for (ident, indices) in self.resolution_graph.inner[node].children().unwrap().iter() {
            if let Some(ident) = ident {
                let mut names_and_indices: Vec<(ResolutionIndex, &'ast Ident)> = indices
                    .iter()
                    .filter_map(|i| {
                        self.resolution_graph.inner[*i]
                            .name()
                            .map(|name| (*i, name))
                    })
                    .collect();
                if let Some(unnamed_children) = self.resolution_graph.inner[node]
                    .children()
                    .and_then(|children| children.get(&None))
                {
                    unnamed_children
                        .iter()
                        .filter(|child| self.resolution_graph.inner[**child].is_use())
                        .for_each(|child| {
                            if let Some(with_name) = self.resolution_graph.inner[*child]
                                .children()
                                .and_then(|children| children.get(&Some(ident)))
                            {
                                with_name
                                    .iter()
                                    .filter_map(|i| {
                                        self.resolution_graph.inner[*i]
                                            .name()
                                            .map(|name| (*child, name))
                                    })
                                    .for_each(|x| names_and_indices.push(x))
                            }
                        })
                }
                names_and_indices.sort_by_key(|x| x.0);
                let mut claimed = vec![false; names_and_indices.len()];
                // Unfortunately, need an O(n^2) check here on items with the same name
                for (i_pos, (i, i_name)) in names_and_indices.iter().enumerate() {
                    for (j_pos, (j, j_name)) in names_and_indices.iter().enumerate().skip(i_pos + 1)
                    {
                        // Don't create repetitive errors by "claiming" duplicates for errors
                        if claimed[j_pos] {
                            continue;
                        }
                        // Skip names that don't conflict
                        if !self.resolution_graph.inner[*i]
                            .in_same_name_class(&self.resolution_graph.inner[*j])
                        {
                            continue;
                        }
                        if i_name == j_name {
                            self.errors.push(crate::error::multiple_definition(
                                file_id,
                                i_name,
                                j_name,
                                DuplicateHint::Name,
                            ));
                            // Optimization: don't need to claim items that won't be seen again
                            // claimed[i] = true;
                            claimed[j_pos] = true;
                            // Stop at the first conflict seen for `i`, since `j` will necessarily become `i` in the future and handle any further conflicts.
                            break;
                        }
                    }
                }
            }
        }
    }

    // TODO: didn't finish this because reimports are more of a warning than an error
    // when there's a name conflict, you could specify that it's *because* of a reimport though
    // fn find_reimports_in(&self, node: ResolutionIndex, file: &FileId) -> Vec<Diagnostic> {
    //     let mut errors = vec![];
    //     let mut imported: HashMap<ResolutionIndex, &'ast Ident> = HashMap::default();
    //     for child in self.resolution_graph.neighbors(node) {
    //         match &self.resolution_graph.inner[child] {
    //             Node::Use { imports, .. } => {
    //                 imports.values().for_each(|uses| {
    //                     uses.iter().for_each(|r#use| match r#use {
    //                         UseType::Name {
    //                             indices,
    //                             name: UseName { ident, .. },
    //                             ..
    //                         }
    //                         | UseType::Rename {
    //                             indices,
    //                             rename: UseRename { ident, .. },
    //                             ..
    //                         } => indices.iter().for_each(|i| {
    //                             use std::collections::hash_map::Entry;
    //                             if let Entry::Occupied(occupant) = imported.entry(*i) {
    //                                 todo!("reimport error: {:?} {:?}", i, occupant);
    //                             } else {
    //                                 imported.insert(*i, ident);
    //                             }
    //                         }),
    //                         _ => {}
    //                     })
    //                 });
    //             }
    //             _ => continue,
    //         }
    //     }
    //     errors
    // }
}

impl<'a, 'ast> Visit<'ast> for ConflictCheckerVisitor<'a, 'ast> {
    fn visit_file(&mut self, _file: &'ast RhdlFile) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_mod(&mut self, _item_mod: &'ast ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item(&mut self, _item: &'ast Item) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_fields(&mut self, fields: &'ast Fields) {
        let mut seen_idents: HashMap<&str, &Ident> = HashMap::default();
        match fields {
            Fields::Named(named) => {
                for named_field in named.inner.iter() {
                    if let Some(previous_ident) = seen_idents.get(named_field.ident.inner.as_str())
                    {
                        self.errors.push(crate::error::multiple_definition(
                            self.file,
                            previous_ident,
                            &named_field.ident,
                            DuplicateHint::Field,
                        ));
                    }
                    seen_idents.insert(&named_field.ident.inner, &named_field.ident);
                }
            }
            Fields::Unnamed(_) => {}
        }
    }

    fn visit_item_enum(&mut self, item_enum: &'ast ItemEnum) {
        let mut seen_idents: HashMap<&str, &Ident> = HashMap::default();
        for variant in item_enum.variants.iter() {
            self.visit_variant(&variant);
            if let Some(previous_ident) = seen_idents.get(variant.ident.inner.as_str()) {
                self.errors.push(crate::error::multiple_definition(
                    self.file,
                    previous_ident,
                    &variant.ident,
                    DuplicateHint::Variant,
                ));
            }
            seen_idents.insert(&variant.ident.inner, &variant.ident);
        }
    }

    /// Rebound more than once error
    fn visit_sig(&mut self, sig: &'ast Sig) {
        struct SignatureVisitor<'a, 'ast> {
            file_id: FileId,
            errors: &'a mut Vec<Diagnostic>,
            seen_idents: HashMap<&'ast str, &'ast Ident>,
        }
        impl<'a, 'ast> Visit<'ast> for SignatureVisitor<'a, 'ast> {
            fn visit_pat_ident(&mut self, pat_ident: &'ast PatIdent) {
                if let Some(previous_ident) = self.seen_idents.get(pat_ident.inner.as_str()) {
                    self.errors.push(crate::error::multiple_definition(
                        self.file_id,
                        previous_ident,
                        &pat_ident,
                        DuplicateHint::NameBinding,
                    ));
                }
                self.seen_idents.insert(&pat_ident.inner, &pat_ident);
            }
        }
        let mut signature_visitor = SignatureVisitor {
            file_id: self.file,
            errors: self.errors,
            seen_idents: Default::default(),
        };
        signature_visitor.visit_sig(sig);
        if let Some(generics) = &sig.generics {
            self.visit_generics(generics);
        }
    }

    /// Conflicting generics/lifetimes
    fn visit_generics(&mut self, generics: &'ast Generics) {
        let mut seen_idents: HashMap<&str, &Ident> = HashMap::default();
        for generic_param in generics.params.iter() {
            let ident = match generic_param {
                GenericParam::Type(ty) => &ty.ident,
                GenericParam::Const(cons) => &cons.ident,
            };
            if let Some(previous_ident) = seen_idents.get(ident.inner.as_str()) {
                self.errors.push(crate::error::multiple_definition(
                    self.file,
                    previous_ident,
                    &ident,
                    DuplicateHint::TypeParam,
                ));
            }
            seen_idents.insert(&ident.inner, &ident);
        }

        // seen_idents.clear();
        // for lifetime in generics.lifetimes() {
        //     if let Some(previous_ident) = seen_idents.get(&lifetime.lifetime.ident) {
        //         self.errors.push(
        //             MultipleDefinitionError {
        //                 file: self.file.clone(),
        //                 name: lifetime.lifetime.ident.to_string(),
        //                 original: previous_ident.span(),
        //                 duplicate: lifetime.lifetime.ident.span(),
        //                 hint: DuplicateHint::Lifetime,
        //             }
        //             .into(),
        //         );
        //     }
        //     seen_idents.insert(&lifetime.lifetime.ident);
        // }
    }
}
