use fnv::{FnvHashMap as HashMap, FnvHashSet as HashSet};
use syn::{
    visit::Visit, Fields, File as SynFile, Generics, Ident, Item, ItemEnum, ItemMod, PatIdent,
    Signature, UseName, UseRename,
};

use std::rc::Rc;

use super::{
    r#use::UseType, Branch, File, Name, ResolutionError, ResolutionGraph, ResolutionIndex,
    ResolutionNode,
};
use crate::error::{DuplicateHint, MultipleDefinitionError};

pub struct ConflictChecker<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<ResolutionError>,
}

struct ConflictCheckerVisitor<'a, 'ast> {
    resolution_graph: &'a ResolutionGraph<'ast>,
    errors: &'a mut Vec<ResolutionError>,
    file: Rc<File>,
}

impl<'a, 'ast> ConflictChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for node in self.resolution_graph.node_indices() {
            let mut visitor = ConflictCheckerVisitor {
                resolution_graph: self.resolution_graph,
                errors: self.errors,
                file: self.resolution_graph.inner[node].file(self.resolution_graph),
            };
            self.resolution_graph.inner[node].visit(&mut visitor);
            let file = match &self.resolution_graph.inner[node] {
                ResolutionNode::Root { file, .. } => file.clone(),
                ResolutionNode::Branch {
                    branch: Branch::Mod(_, None),
                    ..
                }
                | ResolutionNode::Branch {
                    branch: Branch::Impl(_),
                    ..
                }
                | ResolutionNode::Branch {
                    branch: Branch::Trait(_),
                    ..
                } => self.resolution_graph.inner[node].file(self.resolution_graph),
                ResolutionNode::Branch {
                    branch: Branch::Mod(_, Some(content_file)),
                    ..
                } => content_file.clone(),
                _ => continue,
            };
            self.find_name_conflicts_in(node, file);
        }
    }

    fn find_name_conflicts_in(&mut self, node: ResolutionIndex, file: Rc<File>) {
        // Check the scope for conflicts
        let mut ident_map: HashMap<&'ast Ident, Vec<&'ast Ident>> = self.resolution_graph.inner
            [node]
            .children()
            .unwrap()
            .iter()
            .filter_map(|(ident_opt, with_ident)| {
                ident_opt.map(|ident| {
                    (
                        ident,
                        with_ident
                            .iter()
                            .filter_map(|child| self.resolution_graph.inner[*child].name())
                            .collect::<Vec<&'ast Ident>>(),
                    )
                })
            })
            .collect();
        for (ident, names) in ident_map.iter() {
            let mut claimed = vec![false; names.len()];
            // Unfortunately, need an O(n^2) check here on items with the same name
            // As per petgraph docs, this is ordered most recent to least recent, so need to iterate in reverse
            for i in (0..names.len()).rev() {
                let i_name = &names[i];
                for j in (0..i).rev() {
                    // Don't create repetitive errors by "claiming" duplicates for errors
                    if claimed[j] {
                        continue;
                    }
                    let j_name = &names[j];
                    // TODO: go back to conflicts with logic
                    if i_name == j_name {
                        self.errors.push(
                            MultipleDefinitionError {
                                file: file.clone(),
                                name: ident.to_string(),
                                original: i_name.span(),
                                duplicate: j_name.span(),
                                hint: DuplicateHint::Name,
                            }
                            .into(),
                        );
                        // Optimization: don't need to claim items that won't be seen again
                        // claimed[i] = true;
                        claimed[j] = true;
                        // Stop at the first conflict seen for `i`, since `j` will necessarily become `i` in the future and handle any further conflicts.
                        break;
                    }
                }
            }
        }
    }

    // TODO: didn't finish this because reimports are more of a warning than an error
    // when there's a name conflict, you could specify that it's *because* of a reimport though
    // fn find_reimports_in(&self, node: ResolutionIndex, file: &Rc<File>) -> Vec<ResolutionError> {
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
    fn visit_file(&mut self, _file: &'ast SynFile) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item_mod(&mut self, _item_mod: &'ast ItemMod) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_item(&mut self, _item: &'ast Item) {
        // purposefully do nothing so we don't recurse out of this scope
    }

    fn visit_fields(&mut self, fields: &'ast Fields) {
        let mut seen_idents: HashSet<&Ident> = HashSet::default();
        for field in fields.iter() {
            if let Some(ident) = field.ident.as_ref() {
                if let Some(previous_ident) = seen_idents.get(ident) {
                    self.errors.push(
                        MultipleDefinitionError {
                            file: self.file.clone(),
                            name: ident.to_string(),
                            original: previous_ident.span(),
                            duplicate: ident.span(),
                            hint: DuplicateHint::Field,
                        }
                        .into(),
                    );
                }
                seen_idents.insert(ident);
            }
        }
    }

    fn visit_item_enum(&mut self, item_enum: &'ast ItemEnum) {
        let mut seen_idents: HashSet<&Ident> = HashSet::default();
        for variant in item_enum.variants.iter() {
            self.visit_fields(&variant.fields);
            if let Some(previous_ident) = seen_idents.get(&variant.ident) {
                self.errors.push(
                    MultipleDefinitionError {
                        file: self.file.clone(),
                        name: variant.ident.to_string(),
                        original: previous_ident.span(),
                        duplicate: variant.ident.span(),
                        hint: DuplicateHint::Variant,
                    }
                    .into(),
                );
            }
            seen_idents.insert(&variant.ident);
        }
    }

    /// Rebound more than once error
    fn visit_signature(&mut self, sig: &'ast Signature) {
        struct SignatureVisitor<'a, 'ast> {
            file: Rc<File>,
            errors: &'a mut Vec<ResolutionError>,
            seen_idents: HashSet<&'ast Ident>,
        }
        impl<'a, 'ast> Visit<'ast> for SignatureVisitor<'a, 'ast> {
            fn visit_pat_ident(&mut self, pat_ident: &'ast PatIdent) {
                if let Some(previous_ident) = self.seen_idents.get(&pat_ident.ident) {
                    // error
                    self.errors.push(
                        MultipleDefinitionError {
                            file: self.file.clone(),
                            name: pat_ident.ident.to_string(),
                            original: previous_ident.span(),
                            duplicate: pat_ident.ident.span(),
                            hint: DuplicateHint::NameBinding,
                        }
                        .into(),
                    );
                }
                self.seen_idents.insert(&pat_ident.ident);
            }
        }
        let mut signature_visitor = SignatureVisitor {
            file: self.file.clone(),
            errors: self.errors,
            seen_idents: Default::default(),
        };
        signature_visitor.visit_signature(sig);
        self.visit_generics(&sig.generics);
    }

    /// Conflicting generics/lifetimes
    fn visit_generics(&mut self, generics: &'ast Generics) {
        let mut seen_idents: HashSet<&Ident> = HashSet::default();
        for type_param in generics.type_params() {
            if let Some(previous_ident) = seen_idents.get(&type_param.ident) {
                self.errors.push(
                    MultipleDefinitionError {
                        file: self.file.clone(),
                        name: type_param.ident.to_string(),
                        original: previous_ident.span(),
                        duplicate: type_param.ident.span(),
                        hint: DuplicateHint::TypeParam,
                    }
                    .into(),
                );
            }
            seen_idents.insert(&type_param.ident);
        }

        seen_idents.clear();
        for lifetime in generics.lifetimes() {
            if let Some(previous_ident) = seen_idents.get(&lifetime.lifetime.ident) {
                self.errors.push(
                    MultipleDefinitionError {
                        file: self.file.clone(),
                        name: lifetime.lifetime.ident.to_string(),
                        original: previous_ident.span(),
                        duplicate: lifetime.lifetime.ident.span(),
                        hint: DuplicateHint::Lifetime,
                    }
                    .into(),
                );
            }
            seen_idents.insert(&lifetime.lifetime.ident);
        }
    }
}
