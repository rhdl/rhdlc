use fxhash::FxHashMap as HashMap;
use rhdl::ast::ItemEntity;

use rhdl::{
    ast::{
        Fields, File as RhdlFile, GenericParam, Generics, Ident, Item, ItemEnum, ItemMod, PatIdent,
        Sig,
    },
    visit::Visit,
};

use super::{Branch, Leaf, ResolutionGraph, ResolutionIndex, ResolutionNode};
use crate::error::{reimport, Diagnostic, DuplicateHint};
use crate::find_file::FileId;

pub struct ConflictChecker<'a, 'ast> {
    pub resolution_graph: &'a ResolutionGraph<'ast>,
    pub errors: &'a mut Vec<Diagnostic>,
}

struct ConflictCheckerVisitor<'a> {
    errors: &'a mut Vec<Diagnostic>,
    file: FileId,
}

impl<'a, 'ast> ConflictChecker<'a, 'ast> {
    pub fn visit_all(&mut self) {
        for node in self.resolution_graph.node_indices() {
            let mut visitor = ConflictCheckerVisitor {
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
                | ResolutionNode::Branch {
                    branch: Branch::Trait(_),
                    ..
                }
                | ResolutionNode::Branch {
                    branch: Branch::Arch(_),
                    ..
                }
                | ResolutionNode::Branch {
                    branch: Branch::Fn(_),
                    ..
                } => self.resolution_graph.file(node),
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
            self.find_use_conflicts_in(node, file);
        }
    }

    fn find_name_conflicts_in(&mut self, node: ResolutionIndex, file_id: FileId) {
        // Check the scope for conflicts
        for (ident, indices) in self.resolution_graph.inner[node].children().unwrap().iter() {
            let ident = if let Some(ident) = ident {
                ident
            } else {
                continue;
            };
            let mut names_and_indices: Vec<(ResolutionIndex, &'ast Ident)> = indices
                .iter()
                .copied()
                .filter_map(|i| Some(i).zip(self.resolution_graph.inner[i].name()))
                .collect();
            if let Some(unnamed_children) = self.resolution_graph.inner[node]
                .children()
                .and_then(|children| children.get(&None))
            {
                unnamed_children
                    .iter()
                    .copied()
                    .filter(|child| self.resolution_graph.inner[*child].is_use())
                    .for_each(|child| {
                        if let Some(with_name) = self.resolution_graph.inner[child]
                            .children()
                            .and_then(|children| children.get(&Some(ident)))
                        {
                            // The child index is used here because we want to
                            // respect the position of the use in the file
                            names_and_indices.extend(with_name.iter().copied().filter_map(|i| {
                                Some(child).zip(self.resolution_graph.inner[i].name())
                            }));
                        }
                    })
            }
            // Enforce precedence
            names_and_indices.sort_by_key(|x| x.0);
            let mut claimed = vec![false; names_and_indices.len()];
            // Unfortunately, need an O(n^2) check here on items with the same name
            for (i_pos, (i, i_name)) in names_and_indices.iter().enumerate() {
                for (j_pos, (j, j_name)) in names_and_indices.iter().enumerate().skip(i_pos + 1) {
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
                        // claimed[i_pos] = true;
                        claimed[j_pos] = true;
                        // Stop at the first conflict seen for `i`, since `j` will necessarily become `i` in the future and handle any further conflicts.
                        break;
                    }
                }
            }
        }
    }

    fn find_use_conflicts_in(&mut self, node: ResolutionIndex, file: FileId) {
        let mut imported: HashMap<ResolutionIndex, (ResolutionIndex, &'ast Ident)> =
            HashMap::default();
        let unnamed_children = if let Some(unnamed_children) = self.resolution_graph.inner[node]
            .children()
            .unwrap()
            .get(&None)
        {
            unnamed_children
        } else {
            return;
        };
        for unnamed_child in unnamed_children.iter().copied() {
            match &self.resolution_graph.inner[unnamed_child] {
                ResolutionNode::Branch {
                    branch: Branch::Use(_),
                    ..
                } => {
                    for (name_opt, use_leaf_indices) in self.resolution_graph.inner[unnamed_child]
                        .children()
                        .unwrap()
                    {
                        if name_opt.is_none() {
                            continue;
                        }
                        for named_child_idx in use_leaf_indices {
                            let ident = self.resolution_graph.inner[*named_child_idx]
                                .name()
                                .unwrap();
                            let imports = match &self.resolution_graph.inner[*named_child_idx] {
                                ResolutionNode::Leaf {
                                    leaf: Leaf::UseName(.., imports),
                                    ..
                                }
                                | ResolutionNode::Leaf {
                                    leaf: Leaf::UseRename(.., imports),
                                    ..
                                } => imports,
                                _ => unreachable!(),
                            };
                            for import in imports {
                                if let Some((_previous_idx, previous_ident)) =
                                    imported.insert(*import, (*named_child_idx, ident))
                                {
                                    self.errors.push(reimport(
                                        file,
                                        previous_ident,
                                        ident,
                                        self.resolution_graph.file(*import),
                                        self.resolution_graph.inner[*import].name().unwrap(),
                                        self.resolution_graph.inner[*import].item_hint(),
                                    ));
                                }
                            }
                        }
                    }
                }
                _ => continue,
            }
        }
        // also handle name conflicts unique to imports
        let mut name_conflicts: HashMap<&'ast Ident, Vec<(ResolutionIndex, &'ast Ident)>> =
            HashMap::default();
        for import_loc in imported.values() {
            let conflicts = name_conflicts.entry(&import_loc.1).or_default();
            if !conflicts.contains(import_loc) {
                conflicts.push(*import_loc);
            }
        }
        for (_, conflicts) in name_conflicts.iter_mut() {
            conflicts.sort_by_key(|x| x.0);
        }
        for (name, conflicts) in name_conflicts.iter() {
            if self.resolution_graph.inner[node]
                .children()
                .map(|children| children.get(&Some(name)).is_some())
                .unwrap_or_default()
            {
                continue;
            }
            for (original, duplicate) in conflicts
                .iter()
                .zip(conflicts.iter().skip(1))
            {
                self.errors.push(crate::error::multiple_definition(
                    file,
                    original.1,
                    duplicate.1,
                    DuplicateHint::Name,
                ));
            }
        }
    }
}

impl<'a, 'ast> Visit<'ast> for ConflictCheckerVisitor<'a> {
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
                    if let Some(previous_ident) =
                        seen_idents.insert(&named_field.ident.inner, &named_field.ident)
                    {
                        self.errors.push(crate::error::multiple_definition(
                            self.file,
                            previous_ident,
                            &named_field.ident,
                            DuplicateHint::Field,
                        ));
                    }
                }
            }
            Fields::Unnamed(_) => {}
        }
    }

    fn visit_item_entity(&mut self, item_entity: &'ast ItemEntity) {
        if let Some(ref generics) = item_entity.generics {
            self.visit_generics(generics);
        }
        let mut seen_idents: HashMap<&str, &Ident> = HashMap::default();
        for port in item_entity.ports.iter() {
            if let Some(previous_ident) = seen_idents.insert(&port.ident.inner, &port.ident) {
                self.errors.push(crate::error::multiple_definition(
                    self.file,
                    previous_ident,
                    &port.ident,
                    DuplicateHint::Port,
                ))
            }
        }
    }

    fn visit_item_enum(&mut self, item_enum: &'ast ItemEnum) {
        let mut seen_idents: HashMap<&str, &Ident> = HashMap::default();
        for variant in item_enum.variants.iter() {
            self.visit_variant(&variant);
            if let Some(previous_ident) = seen_idents.insert(&variant.ident.inner, &variant.ident) {
                self.errors.push(crate::error::multiple_definition(
                    self.file,
                    previous_ident,
                    &variant.ident,
                    DuplicateHint::Variant,
                ));
            }
        }
    }

    /// Bound more than once error
    fn visit_sig(&mut self, sig: &'ast Sig) {
        struct SignatureVisitor<'a, 'ast> {
            file_id: FileId,
            errors: &'a mut Vec<Diagnostic>,
            seen_idents: HashMap<&'ast str, &'ast Ident>,
        }
        impl<'a, 'ast> Visit<'ast> for SignatureVisitor<'a, 'ast> {
            fn visit_pat_ident(&mut self, pat_ident: &'ast PatIdent) {
                if let Some(previous_ident) = self.seen_idents.insert(&pat_ident.inner, &pat_ident)
                {
                    self.errors.push(crate::error::multiple_definition(
                        self.file_id,
                        previous_ident,
                        &pat_ident,
                        DuplicateHint::NameBinding,
                    ));
                }
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
            if let Some(previous_ident) = seen_idents.insert(&ident.inner, &ident) {
                self.errors.push(crate::error::multiple_definition(
                    self.file,
                    previous_ident,
                    &ident,
                    DuplicateHint::Param,
                ));
            }
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
