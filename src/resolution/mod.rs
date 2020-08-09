/// Build a scope digraph.
/// Nodes are items with visibility
/// Directional edges connect nodes to the places where they are visible, i.e.:
/// * fn in mod
/// * struct in mod
/// * struct fields
///     * fields do have visibility
///     * they aren't items, but...
///     * struct effectively acts as a node containing field nodes & conditional edges to them
/// * pub fn in mod directly visible in the parent scope
///     * if parent scope is fully visible to another scope, it is recursively traced
/// * special pub types (pub(crate), etc.)
/// * type aliases
/// * `use super::ABC as XYZ;`
///
/// Possible scope violations, in order of precedence:
/// * In scope, but not allowed to be
///     * Not public
///         * if it's owned by the user, suggest that they should pub it
///         * crates are not owned by user, but any source in the local tree is
///     * Name conflict
///         * can't have two structs in scope with the same name
/// * Out of scope
///     * Exists, but not in scope
///         * fix by adding a use
///             * find disconnected nodes with the same name (expensive?)
///             * see if it's possible to create an edge (reachable)
///                 * don't offer this if it isn't. if it's owned by user it's private and you can could pub it.
///     * Not Found
///         * look for similarly named disconnected nodes and offer a "did you mean"
///             * use [strsim](https://docs.rs/strsim/0.10.0/strsim/) for Ident similarity
///             * heuristic guess by type (fn, struct, var, mod, etc.)
///         * fall back all the way to "not found" if nothing is similar
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::fmt::Display;
use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, visit::EdgeRef, Direction, Graph};
use syn::{
    spanned::Spanned, visit::Visit, Ident, Item, ItemConst, ItemEnum, ItemFn, ItemImpl, ItemMod,
    ItemStruct, ItemTrait, ItemType, ItemUse, Type, UseName, UseRename,
};

use crate::error::{
    InvalidRawIdentifierError, MultipleDefinitionError, ResolutionError, UnsupportedError,
};
use crate::find_file::{File, FileGraph};

mod name;
use name::Name;

mod r#use;
use r#use::UseType;

mod path;
use path::PathFinder;
mod r#pub;

pub type ScopeGraph<'ast> = Graph<Node<'ast>, String>;

#[derive(Debug)]
pub struct ScopeBuilder<'ast> {
    pub file_graph: &'ast FileGraph,
    pub scope_graph: ScopeGraph<'ast>,
    pub errors: Vec<ResolutionError>,
    scope_ancestry: Vec<NodeIndex>,
    file_ancestry: Vec<NodeIndex>,
}

impl<'ast> From<&'ast FileGraph> for ScopeBuilder<'ast> {
    fn from(file_graph: &'ast FileGraph) -> Self {
        Self {
            file_graph,
            scope_graph: Graph::default(),
            errors: vec![],
            scope_ancestry: vec![],
            file_ancestry: vec![],
        }
    }
}

impl<'ast> ScopeBuilder<'ast> {
    /// Find all names given a source forest
    /// Externals are paths to standalone source code: a top + lib.rs of each crate
    /// Doesn't care about errors, for now
    pub fn build_graph(&mut self) {
        // Stage one: add nodes
        let files: Vec<NodeIndex> = self.file_graph.externals(Direction::Incoming).collect();
        for file_index in files {
            let file = self.file_graph[file_index].clone();
            let scope_index = self.scope_graph.add_node(Node::Root {
                // TODO: attach a real name
                name: String::default(),
                file,
                exports: vec![],
            });
            self.scope_ancestry.push(scope_index);
            self.file_ancestry.push(file_index);
            self.file_graph[file_index]
                .syn
                .items
                .iter()
                .for_each(|i| self.add_mod(i));
            self.file_ancestry.pop();
            self.scope_ancestry.pop();
        }

        // Stage two: apply visibility
        let mut visibility_errors = self
            .scope_graph
            .node_indices()
            .filter_map(|i| r#pub::apply_visibility(&mut self.scope_graph, i).err())
            .collect::<Vec<ResolutionError>>();
        self.errors.append(&mut visibility_errors);

        // Stage three: trace use nodes
        let use_indices: Vec<NodeIndex> = self
            .scope_graph
            .node_indices()
            .filter(|i| match self.scope_graph[*i] {
                Node::Use { .. } => true,
                _ => false,
            })
            .collect();
        let mut visited = HashSet::default();
        let mut use_resolver = r#use::UseResolver {
            visited: &mut visited,
            scope_graph: &mut self.scope_graph,
            errors: &mut self.errors,
        };
        for use_index in use_indices {
            use_resolver.resolve_use(use_index);
        }

        // Stage four: tie impls
        self.tie_impls(&mut visited);
    }

    pub fn check_graph(&mut self) {
        self.errors.append(&mut self.find_invalid_names());
        for node in self.scope_graph.node_indices() {
            let file = match &self.scope_graph[node] {
                Node::Root { file, .. } | Node::Mod { file, .. } => file,
                Node::Impl { .. } => Node::file(&self.scope_graph, node),
                _ => continue,
            };
            self.errors
                .append(&mut self.find_name_conflicts_in(node, &file));
            // self.errors.append(&mut self.find_reimports_in(node, &file));
        }
    }

    fn find_invalid_names(&self) -> Vec<ResolutionError> {
        struct IdentVisitor<'ast>(Vec<ResolutionError>, &'ast Rc<File>);
        impl<'ast> Visit<'ast> for IdentVisitor<'ast> {
            fn visit_ident(&mut self, ident: &Ident) {
                // https://github.com/rust-lang/rust/blob/5ef299eb9805b4c86b227b718b39084e8bf24454/src/librustc_span/symbol.rs#L1592
                if ident == "r#_"
                    || ident == "r#"
                    || ident == "r#super"
                    || ident == "r#self"
                    || ident == "r#Self"
                    || ident == "r#crate"
                {
                    self.0.push(
                        InvalidRawIdentifierError {
                            file: self.1.clone(),
                            ident: ident.clone(),
                        }
                        .into(),
                    );
                }
            }
        }
        let mut errors = vec![];
        for node in self.file_graph.node_indices() {
            let file = &self.file_graph[node];
            let mut visitor = IdentVisitor(vec![], file);
            visitor.visit_file(&file.syn);
            errors.append(&mut visitor.0);
        }
        errors
    }

    fn find_name_conflicts_in(&self, node: NodeIndex, file: &Rc<File>) -> Vec<ResolutionError> {
        // Check the scope for conflicts
        let mut ident_map: HashMap<String, Vec<Name<'ast>>> = HashMap::default();
        for child in self.scope_graph.neighbors(node) {
            for name in self.scope_graph[child].names() {
                ident_map.entry(name.to_string()).or_default().push(name);
            }
        }
        let mut errors = vec![];
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
                    if i_name.conflicts_with(&j_name) {
                        errors.push(
                            MultipleDefinitionError {
                                file: file.clone(),
                                name: ident.clone(),
                                original: i_name.span(),
                                duplicate: j_name.span(),
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
        errors
    }

    // TODO: didn't finish this because reimports are more of a warning than an error
    // when there's a name conflict, you could specify that it's *because* of a reimport though
    fn find_reimports_in(&self, node: NodeIndex, file: &Rc<File>) -> Vec<ResolutionError> {
        let mut errors = vec![];
        let mut imported: HashMap<NodeIndex, &'ast Ident> = HashMap::default();
        for child in self.scope_graph.neighbors(node) {
            match &self.scope_graph[child] {
                Node::Use { imports, .. } => {
                    imports.values().for_each(|uses| {
                        uses.iter().for_each(|r#use| match r#use {
                            UseType::Name {
                                indices,
                                name: UseName { ident, .. },
                                ..
                            }
                            | UseType::Rename {
                                indices,
                                rename: UseRename { ident, .. },
                                ..
                            } => indices.iter().for_each(|i| {
                                use std::collections::hash_map::Entry;
                                if let Entry::Occupied(occupant) = imported.entry(*i) {
                                    todo!("reimport error: {:?} {:?}", i, occupant);
                                } else {
                                    imported.insert(*i, ident);
                                }
                            }),
                            _ => {}
                        })
                    });
                }
                _ => continue,
            }
        }
        errors
    }

    fn tie_impls(&mut self, visited: &mut HashSet<NodeIndex>) {
        let impls: Vec<NodeIndex> = self
            .scope_graph
            .node_indices()
            .filter(|index| match &self.scope_graph[*index] {
                Node::Impl { .. } => true,
                _ => false,
            })
            .collect();
        for r#impl in impls {
            let item_impl = match &mut self.scope_graph[r#impl] {
                Node::Impl {
                    item_impl,
                    r#trait,
                    r#for,
                } => item_impl.clone(),
                _ => continue,
            };
            let mut path_finder = PathFinder {
                scope_graph: &mut self.scope_graph,
                visited,
                errors: &mut self.errors,
            };
            let r#trait = if let Some((_, trait_path, _)) = &item_impl.trait_ {
                // find the trait
                match path_finder.find_at_path(r#impl, trait_path) {
                    Ok(traits) => {
                        if traits.len() > 1 {
                            todo!("disambiguate if possible, if not push an error but run with the found trait");
                        }
                        traits.first().cloned()
                    }
                    Err(err) => {
                        self.errors.push(err);
                        continue;
                    }
                }
            } else {
                None
            };

            // TODO: this actually needs type-checking to be resolved if trait is some
            let r#for = if item_impl.trait_.is_none() {
                if let Type::Path(type_path) = item_impl.self_ty.as_ref() {
                    // find the for
                    match path_finder.find_at_path(r#impl, &type_path.path) {
                        Ok(types) => {
                            if types.len() > 1 {
                                todo!("disambiguate if possible, if not push an error but run with the found struct");
                            }
                            types.first().cloned()
                        }
                        Err(err) => {
                            self.errors.push(err);
                            continue;
                        }
                    }
                } else {
                    todo!("no base type found for inherent implementation")
                }
            } else {
                None
            };

            match &mut self.scope_graph[r#impl] {
                Node::Impl {
                    item_impl,
                    r#trait: trait_dest,
                    r#for: for_dest,
                } => {
                    *trait_dest = r#trait;
                    *for_dest = r#for;
                },
                _ => continue,
            }
        }
    }

    /// Stage one
    fn add_mod(&mut self, item: &'ast Item) {
        use syn::Item::*;
        use syn::*;
        match item {
            Mod(item_mod) => {
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
                    items.iter().for_each(|i| self.add_mod(i));
                    self.scope_ancestry.pop();
                } else {
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
                        self.file_graph[file_index]
                            .syn
                            .items
                            .iter()
                            .for_each(|item| self.add_mod(item));
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
            Fn(r#fn) => {
                let item_idx = self.scope_graph.add_node(Node::Fn { item_fn: r#fn });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "fn".to_string());
            }

            Const(item_const) => {
                let item_idx = self.scope_graph.add_node(Node::Const { item_const });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "const".to_string());
            }
            Type(item_type) => {
                let item_idx = self.scope_graph.add_node(Node::Type { item_type });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "type alias".to_string());
            }
            Trait(item_trait) => {
                let item_idx = self.scope_graph.add_node(Node::Trait {
                    item_trait,
                    impls: HashMap::default(),
                });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "trait".to_string());
            }
            Struct(item_struct) => {
                let item_idx = self.scope_graph.add_node(Node::Struct {
                    item_struct,
                    impls: HashMap::default(),
                });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "struct".to_string());
            }
            Enum(item_enum) => {
                let item_idx = self.scope_graph.add_node(Node::Enum {
                    item_enum,
                    impls: HashMap::default(),
                });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "enum".to_string());
            }
            Use(item_use) => {
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
            Impl(item_impl) => {
                let impl_idx = self.scope_graph.add_node(Node::Impl {
                    item_impl,
                    r#trait: None,
                    r#for: None,
                });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, impl_idx, "impl".to_string());
            }
            Macro(ItemMacro { ident, .. }) => {
                self.errors.push(
                    UnsupportedError {
                        file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                        span: ident.span(),
                        reason: "RHDL does not support macros (yet)",
                    }
                    .into(),
                );
            }
            Macro2(ItemMacro2 { ident, .. }) => {
                self.errors.push(UnsupportedError {
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                    span: ident.span(),
                    reason: "RHDL does not support declarative macros, as they are not stabilized in Rust"
                }.into());
            }
            Static(ItemStatic { ident, .. }) => {
                self.errors.push(UnsupportedError {
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                    span: ident.span(),
                    reason: "RHDL cannot support static variables and other unsafe code: safety is not yet formally defined"
                }.into());
            }
            Union(ItemUnion { ident, .. }) => {
                self.errors.push(UnsupportedError {
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                    span: ident.span(),
                    reason: "RHDL cannot support unions and other unsafe code: safety is not yet formally defined"
                }.into());
            }
            TraitAlias(ItemTraitAlias { ident, .. }) => {
                self.errors.push(UnsupportedError {
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                    span: ident.span(),
                    reason: "RHDL does not support trait aliases as they are still experimental in Rust"
                }.into());
            }
            ExternCrate(ItemExternCrate { ident, .. }) => {
                self.errors.push(UnsupportedError {
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                    span: ident.span(),
                    reason: "RHDL does not support Rust 2015 syntax, you can safely remove this. :)"
                }.into());
            }

            other => {
                dbg!(other);
                self.errors.push(
                    UnsupportedError {
                        file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                        span: other.span(),
                        reason: "RHDL does not know about this feature (yet!)",
                    }
                    .into(),
                );
            }
        }
    }
}

#[derive(Debug)]
pub enum Node<'ast> {
    /// A node for the root of a tree
    /// This could be a crate, or just "top.rhdl"
    Root {
        /// This information comes from an external source
        /// Only the top level entity is allowed to have no name
        /// TODO: figure out how to reconcile library-building behavior of rustc
        /// with the fact that there are no binaries for RHDL...
        name: String,
        file: Rc<File>,
        // The list of items this root exports, visible from ANY scope
        exports: Vec<NodeIndex>,
    },
    Fn {
        /// A fn
        item_fn: &'ast ItemFn,
    },
    Const {
        /// A const, static
        item_const: &'ast ItemConst,
    },
    Struct {
        item_struct: &'ast ItemStruct,
        // impls for this: traits & self
        impls: HashMap<NodeIndex, NodeIndex>,
    },
    Enum {
        item_enum: &'ast ItemEnum,
        // impls for this: traits & self
        impls: HashMap<NodeIndex, NodeIndex>,
    },
    Type {
        item_type: &'ast ItemType,
        // TODO: since this is an alias, needs to be treated as a pointer to a real underlying type
    },
    Trait {
        item_trait: &'ast ItemTrait,
        // impls of this trait
        impls: HashMap<NodeIndex, NodeIndex>,
    },
    Mod {
        item_mod: &'ast ItemMod,
        /// Exports: (from item, to roots/mods) aka pubs
        exports: HashMap<NodeIndex, NodeIndex>,
        file: Rc<File>,
        /// The file backing the content of this mod when content = None, if available
        content_file: Option<Rc<File>>,
    },
    Impl {
        item_impl: &'ast ItemImpl,
        /// impl Option<Trait> Option<for> **NodeIndex**
        /// which cannot be resolved at first
        r#trait: Option<NodeIndex>,
        r#for: Option<NodeIndex>,
    },
    Use {
        item_use: &'ast ItemUse,
        /// Imports: (from scope to its list of use types)
        /// Note that each UseType can include ambiguous names
        /// These are NOT deduped, so that we can catch reimport errors
        imports: HashMap<NodeIndex, Vec<UseType<'ast>>>,
    },
}
impl<'ast> Node<'ast> {
    fn file(scope_graph: &'ast ScopeGraph<'ast>, index: NodeIndex) -> &'ast Rc<File> {
        let mut current_index = index;
        loop {
            match &scope_graph[current_index] {
                Self::Root { file, .. } => return &file,
                Self::Mod {
                    file, content_file, ..
                } => {
                    return if let (Some(content_file), false) =
                        (content_file.as_ref(), index == current_index)
                    {
                        content_file
                    } else {
                        &file
                    }
                }
                _ => {
                    current_index = scope_graph
                        .neighbors_directed(current_index, Direction::Incoming)
                        .next()
                        .unwrap();
                }
            }
        }
    }

    fn names(&self) -> Vec<Name<'ast>> {
        match self {
            // TODO: handle invalid name roots
            Self::Root { .. } => vec![],
            Self::Struct { item_struct, .. } => vec![Name::from(*item_struct).into()],
            Self::Trait { item_trait, .. } => vec![Name::from(*item_trait).into()],
            Self::Type { item_type, .. } => vec![Name::from(*item_type).into()],
            Self::Enum { item_enum, .. } => vec![Name::from(*item_enum).into()],
            Self::Fn { item_fn, .. } => vec![Name::from(*item_fn)],
            Self::Mod { item_mod, .. } => vec![Name::from(*item_mod)],
            Self::Const { item_const, .. } => vec![Name::from(*item_const)],
            Self::Use { imports, .. } => imports
                .values()
                .map(|uses| uses.iter().filter_map(|r#use| Name::try_from(r#use).ok()))
                .flatten()
                .collect::<Vec<Name<'ast>>>(),
            Self::Impl {
                item_impl: ItemImpl { .. },
                ..
            } => vec![],
        }
    }
}

#[cfg(not(tarpaulin_include))]
impl<'ast> Display for Node<'ast> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        match self {
            Self::Root { .. } => write!(f, "root"),
            Self::Const { item_const, .. } => write!(f, "const {}", item_const.ident),
            Self::Fn { item_fn, .. } => write!(f, "fn {}", item_fn.sig.ident),
            Self::Struct { item_struct, .. } => write!(f, "struct {}", item_struct.ident),
            Self::Trait { item_trait, .. } => write!(f, "trait {}", item_trait.ident),
            Self::Type { item_type, .. } => write!(f, "type {}", item_type.ident),
            Self::Enum { item_enum, .. } => write!(f, "enum {}", item_enum.ident),
            Self::Mod { item_mod, .. } => write!(f, "mod {}", item_mod.ident),
            Self::Impl { r#for, .. } => {
                write!(f, "impl")?;
                if let Some(r#for) = r#for {
                    write!(f, "for {:?}", r#for);
                }
                Ok(())
            }
            Self::Use {
                item_use, imports, ..
            } => {
                if let syn::Visibility::Public(_) = item_use.vis {
                    write!(f, "pub ")?;
                }
                write!(f, "use")?;
                for (_, uses) in imports.iter() {
                    for r#use in uses.iter() {
                        match r#use {
                            UseType::Name { name, .. } => write!(f, " {}", name.ident)?,
                            UseType::Glob { .. } => write!(f, " *")?,
                            UseType::Rename { rename, .. } => {
                                write!(f, " {} as {}", rename.ident, rename.rename)?
                            }
                        }
                    }
                }
                Ok(())
            }
        }
    }
}
