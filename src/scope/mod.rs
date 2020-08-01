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
use std::collections::HashMap;
use std::fmt::Display;
use std::rc::Rc;

use log::error;
use petgraph::{graph::NodeIndex, visit::EdgeRef, Direction, Graph};
use syn::{spanned::Spanned, Ident, Item, ItemImpl, ItemMod, ItemUse};

use crate::error::{InvalidRawIdentifierError, MultipleDefinitionError, ScopeError};
use crate::resolve::{File, FileGraph};

mod name;
use name::Name;

mod r#use;
use r#use::UseType;

mod visibility;

pub type ScopeGraph<'ast> = Graph<Node<'ast>, String>;

#[derive(Debug)]
pub struct ScopeBuilder<'ast> {
    pub file_graph: &'ast FileGraph,
    pub scope_graph: ScopeGraph<'ast>,
    pub errors: Vec<ScopeError>,
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
                name: None,
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
        self.scope_graph
            .node_indices()
            .for_each(|i| visibility::apply_visibility(&mut self.scope_graph, i));

        // Stage three: trace use nodes
        self.scope_graph.node_indices().for_each(|i| {
            r#use::trace_use_entry(&mut self.scope_graph, &mut self.errors, i);
        });

        // Stage four: tie impls
    }

    pub fn check_graph(&mut self) {
        self.find_invalid_names();
        self.find_name_conflicts();
    }

    pub fn find_invalid_names(&mut self) {
        use crate::ident::*;
        for node in self.scope_graph.node_indices() {
            match &self.scope_graph[node] {
                Node::Root { name, file, .. } => {}
                Node::Item { ident, file, .. } => {
                    if !can_be_raw(ident) {
                        self.errors.push(
                            InvalidRawIdentifierError {
                                file: file.clone(),
                                ident: (*ident).clone(),
                            }
                            .into(),
                        );
                    }
                }
                Node::Mod {
                    item_mod: ItemMod { ident, .. },
                    file,
                    ..
                } => {
                    if !can_be_raw(ident) {
                        self.errors.push(
                            InvalidRawIdentifierError {
                                file: file.clone(),
                                ident: ident.clone(),
                            }
                            .into(),
                        );
                    }
                }
                Node::Use { imports, file, .. } => {
                    for (_, uses) in imports.iter() {
                        for r#use in uses.iter() {
                            use r#use::UseType::*;
                            use syn::{UseName, UseRename};
                            match r#use {
                                Name {
                                    name: UseName { ident, .. },
                                    ..
                                } => {
                                    if !can_be_raw(ident) {
                                        self.errors.push(
                                            InvalidRawIdentifierError {
                                                file: file.clone(),
                                                ident: (*ident).clone(),
                                            }
                                            .into(),
                                        );
                                    }
                                }
                                Rename {
                                    rename: UseRename { ident, rename, .. },
                                    ..
                                } => {
                                    if !can_be_raw(ident) {
                                        self.errors.push(
                                            InvalidRawIdentifierError {
                                                file: file.clone(),
                                                ident: (*ident).clone(),
                                            }
                                            .into(),
                                        );
                                    }
                                    if !can_be_raw(rename) {
                                        self.errors.push(
                                            InvalidRawIdentifierError {
                                                file: file.clone(),
                                                ident: (*rename).clone(),
                                            }
                                            .into(),
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    /// TODO: check use items for conflicts, and remember that globs are optional and don't conflict
    fn find_name_conflicts(&mut self) {
        for node in self.scope_graph.node_indices() {
            let file = match &self.scope_graph[node] {
                Node::Root { file, .. } | Node::Mod { file, .. } => file,
                _ => continue,
            };

            // Check the scopes for conflicts
            let mut ident_map: HashMap<String, Vec<NodeIndex>> = HashMap::default();
            for child in self.scope_graph.neighbors(node) {
                if let Node::Item { ident, .. }
                | Node::Mod {
                    item_mod: ItemMod { ident, .. },
                    ..
                } = self.scope_graph[child]
                {
                    ident_map.entry(ident.to_string()).or_default().push(child)
                }
            }
            for (ident, indices) in ident_map.iter() {
                let mut claimed = vec![false; indices.len()];
                // Unfortunately, need an O(n^2) check here on items with the same name
                // As per petgraph docs, this is ordered most recent to least recent, so need to iterate in reverse
                for i in (0..indices.len()).rev() {
                    let (i_name, i_span) = match &self.scope_graph[indices[i]] {
                        Node::Item {
                            item: i_item,
                            ident: i_ident,
                            ..
                        } => (Name::from(*i_item), i_ident.span()),
                        Node::Mod {
                            item_mod: i_item_mod,
                            ..
                        } => (Name::from(*i_item_mod), i_item_mod.ident.span()),
                        _ => continue,
                    };
                    for j in (0..i).rev() {
                        // Don't create repetitive errors by "claiming" duplicates for errors
                        if claimed[j] {
                            continue;
                        }
                        let (j_name, j_span) = match &self.scope_graph[indices[j]] {
                            Node::Item {
                                item: j_item,
                                ident: j_ident,
                                ..
                            } => (Name::from(*j_item), j_ident.span()),
                            Node::Mod {
                                item_mod: j_item_mod,
                                ..
                            } => (Name::from(*j_item_mod), j_item_mod.ident.span()),
                            _ => continue,
                        };
                        if i_name.conflicts_with(&j_name) {
                            self.errors.push(
                                MultipleDefinitionError {
                                    file: file.clone(),
                                    name: ident.clone(),
                                    original: i_span,
                                    duplicate: j_span,
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
    }

    /// Stage four
    fn tie_impl(&mut self, item: &'ast Item) {
        if let Item::Impl(ItemImpl { .. }) = item {}
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
                        .filter_map(|scope_ancestor| match self.scope_graph[*scope_ancestor] {
                            Node::Mod { item_mod, .. } => Some(item_mod.ident.clone()),
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
            Macro(ItemMacro {
                ident: Some(ident), ..
            })
            | ExternCrate(ItemExternCrate { ident, .. })
            | Type(ItemType { ident, .. })
            | Static(ItemStatic { ident, .. })
            | Const(ItemConst { ident, .. })
            | Fn(ItemFn {
                sig: Signature { ident, .. },
                ..
            })
            | Macro2(ItemMacro2 { ident, .. })
            | Struct(ItemStruct { ident, .. })
            | Enum(ItemEnum { ident, .. })
            | Trait(ItemTrait { ident, .. })
            | TraitAlias(ItemTraitAlias { ident, .. })
            | Union(ItemUnion { ident, .. }) => {
                let item_idx = self.scope_graph.add_node(Node::Item {
                    item,
                    ident,
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "item".to_string());
            }
            Use(item_use) => {
                let use_idx = self.scope_graph.add_node(Node::Use {
                    item_use,
                    imports: HashMap::default(),
                    file: self.file_graph[*self.file_ancestry.last().unwrap()].clone(),
                });
                self.scope_graph.add_edge(
                    *self.scope_ancestry.last().unwrap(),
                    use_idx,
                    "use".to_string(),
                );
            }
            _other => {}
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
        name: Option<String>,
        file: Rc<File>,
        exports: Vec<NodeIndex>,
    },
    Item {
        item: &'ast Item,
        ident: &'ast Ident,
        file: Rc<File>,
    },
    Mod {
        item_mod: &'ast ItemMod,
        /// Exports: (from item, to list of roots/mods) aka pubs
        exports: HashMap<NodeIndex, Vec<NodeIndex>>,
        file: Rc<File>,
        /// The file backing the content of this mod when content = None, if available
        content_file: Option<Rc<File>>,
    },
    Use {
        item_use: &'ast ItemUse,
        /// Imports: (from root/mod to list of items)
        imports: HashMap<NodeIndex, Vec<UseType<'ast>>>,
        file: Rc<File>,
    },
}

impl<'ast> Display for Node<'ast> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        match self {
            Self::Root { .. } => write!(f, "root"),
            Self::Item { ident, .. } => write!(f, "{}", ident),
            Self::Mod { item_mod, .. } => write!(f, "mod {}", item_mod.ident),
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
                            UseType::Rename { rename, .. } => write!(f, " {}", rename.rename)?,
                        }
                    }
                }
                Ok(())
            }
        }
    }
}