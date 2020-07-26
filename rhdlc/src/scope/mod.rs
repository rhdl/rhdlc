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

use log::{error, warn};
use petgraph::{graph::NodeIndex, visit::EdgeRef, Direction, Graph};
use syn::{spanned::Spanned, Ident, Item, ItemImpl, ItemMod, ItemUse, Visibility};

use crate::error::{MultipleDefinitionError, ScopeError};
use crate::resolve::{File, FileGraph};

mod name;
use name::Name;

#[derive(Debug)]
pub struct ScopeBuilder<'ast> {
    pub file_graph: &'ast FileGraph,
    pub scope_graph: Graph<Node<'ast>, String>,
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
            .for_each(|i| self.apply_visibility(i));

        // Stage three: delete use nodes

        // Stage four: tie impls
    }

    pub fn check_graph(&mut self) {
        self.find_name_conflicts();
    }

    fn find_name_conflicts(&mut self) {
        for node in self.scope_graph.node_indices() {
            let file = match &self.scope_graph[node] {
                Node::Root { file, .. } | Node::Mod { file, .. } => file,
                _ => continue,
            };

            // Check the scopes for conflicts
            let mut ident_map: HashMap<String, Vec<NodeIndex>> = HashMap::default();
            for child in self.scope_graph.neighbors(node) {
                match self.scope_graph[child] {
                    Node::Item { ident, .. } => {
                        ident_map.entry(ident.to_string()).or_default().push(child)
                    }
                    _ => {}
                }
            }
            for (ident, indices) in ident_map.iter() {
                let mut claimed = vec![false; indices.len()];
                // Unfortunately, need an O(n^2) check here on items with the same name
                // As per petgraph docs, this is ordered most recent to least recent, so need to iterate in reverse
                for i in (0..indices.len()).rev() {
                    for j in (0..i).rev() {
                        // Don't create repetitive errors by "claiming" duplicates for errors
                        // TODO: see if this can be simplified since it is a bit hard to understand
                        if claimed[j] {
                            continue;
                        }
                        match (&self.scope_graph[indices[i]], &self.scope_graph[indices[j]]) {
                            (
                                Node::Item {
                                    item: i_item,
                                    ident: i_ident,
                                    ..
                                },
                                Node::Item {
                                    item: j_item,
                                    ident: j_ident,
                                    ..
                                },
                            ) => {
                                if Name::from(*i_item).conflicts_with(&Name::from(*j_item)) {
                                    self.errors.push(
                                        MultipleDefinitionError {
                                            file: file.clone(),
                                            name: ident.clone(),
                                            original: i_ident.span(),
                                            duplicate: j_ident.span(),
                                        }
                                        .into(),
                                    );
                                    // Optimization: don't need to claim items that won't be seen again
                                    // claimed[i] = true;
                                    claimed[j] = true;
                                    // Stop at the first conflict seen for `i`, since `j` will necessarily become `i` in the future and handle any further conflicts
                                    break;
                                }
                            }
                            _ => error!("Only item nodes were added, so this shouldn't happen"),
                        }
                    }
                }
            }
        }
        // let others = self.others_declared_with_same_name_in_scope(name);
        // if others.len() > 0 {
        //     // TODO: create name conflict errors for this
        //     warn!(
        //         "duplicate item names! {:?}",
        //         others
        //             .iter()
        //             .map(|i| Name::from(*i))
        //             .collect::<Vec<Name<'ast>>>()
        //     );
        // }
    }

    /// If a node overrides its own visibility, make a note of it in the parent node(s).
    fn apply_visibility(&mut self, node: NodeIndex) {
        use syn::Item::*;
        use syn::*;
        let vis = match self.scope_graph[node] {
            Node::Item { item, .. } => match item {
                ExternCrate(ItemExternCrate { vis, .. })
                | Type(ItemType { vis, .. })
                | Static(ItemStatic { vis, .. })
                | Const(ItemConst { vis, .. })
                | Fn(ItemFn {
                    sig: Signature { .. },
                    vis,
                    ..
                })
                | Macro2(ItemMacro2 { vis, .. })
                | Struct(ItemStruct { vis, .. })
                | Enum(ItemEnum { vis, .. })
                | Trait(ItemTrait { vis, .. })
                | TraitAlias(ItemTraitAlias { vis, .. })
                | Union(ItemUnion { vis, .. }) => Some(vis),
                _ => None,
            },
            Node::Mod {
                item_mod: ItemMod { vis, .. },
                ..
            } => Some(vis),
            Node::Use {
                item_use: ItemUse { vis, .. },
                ..
            } => Some(vis),
            _ => None,
        };

        if let Some(vis) = vis {
            use Visibility::*;
            match vis {
                Public(_) => self.apply_visibility_pub(node),
                Crate(_) => self.apply_visibility_crate(node),
                Restricted(r) => {
                    if let Some(_in) = r.in_token {
                        todo!("restricted visibility in paths is not implemented yet");
                    // Edition Differences: Starting with the 2018 edition, paths for pub(in path) must start with crate, self, or super. The 2015 edition may also use paths starting with :: or modules from the crate root.
                    } else {
                        match r
                            .path
                            .get_ident()
                            .map(|ident| ident.to_string())
                            .expect("error if the path is not an ident")
                            .as_str()
                        {
                            // No-op
                            "self" => {}
                            // Same as crate pub
                            "crate" => self.apply_visibility_crate(node),
                            // Same as pub
                            "super" => self.apply_visibility_pub(node),
                            _ => todo!("error if none of the above"),
                        }
                    }
                }
                Inherited => {}
            }
        }
    }

    fn apply_visibility_pub(&mut self, node: NodeIndex) {
        let parents: Vec<NodeIndex> = self
            .scope_graph
            .neighbors_directed(node, Direction::Incoming)
            .collect();
        let grandparents: Vec<NodeIndex> = parents
            .iter()
            .map(|parent| {
                self.scope_graph
                    .neighbors_directed(*parent, Direction::Incoming)
            })
            .flatten()
            .collect();
        for parent in parents {
            match &mut self.scope_graph[parent] {
                Node::Mod { exports, .. } => exports.entry(node).or_default().extend(&grandparents),
                Node::Root { exports, .. } => exports.extend(&grandparents),
                other => {
                    error!("parent is not a mod or root {:?}", other);
                }
            }
        }
    }

    /// https://github.com/rust-lang/rust/issues/53120
    /// TODO: check validity of a crate-level pub if this isn't a crate
    /// Bottom-up BFS
    fn apply_visibility_crate(&mut self, node: NodeIndex) {
        let parents: Vec<NodeIndex> = self
            .scope_graph
            .neighbors_directed(node, Direction::Incoming)
            .collect();
        let roots = {
            let mut roots: Vec<NodeIndex> = vec![];
            let mut level: Vec<NodeIndex> = vec![];
            let mut next: Vec<NodeIndex> = vec![node];
            while !next.is_empty() {
                level.append(&mut next);
                level.iter().for_each(|n| {
                    if let Node::Root { .. } = self.scope_graph[*n] {
                        roots.push(*n);
                    } else {
                        next.extend(self.scope_graph.neighbors_directed(*n, Direction::Incoming));
                    }
                });
            }
            roots
        };
        for parent in parents {
            match &mut self.scope_graph[parent] {
                Node::Mod { exports, .. } => exports.entry(node).or_default().extend(&roots),
                Node::Root { exports, .. } => exports.extend(&roots),
                other => {
                    error!("parent is not a mod or root {:?}", other);
                }
            }
        }
    }

    /// Stage four
    fn tie_impl(&mut self, item: &'ast Item) {
        if let Item::Impl(ItemImpl { items, self_ty, .. }) = item {}
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
                        let file = self.file_graph[file_index].clone();
                        let mod_idx = self.scope_graph.add_node(Node::Mod {
                            item_mod,
                            exports: HashMap::default(),
                            file,
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
                        error!(
                            "Could not find a file for {}, this should not be possible",
                            &item_mod.ident
                        );
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
                let item_idx = self.scope_graph.add_node(Node::Item { item, ident });
                let parent = self.scope_ancestry.last().unwrap();
                self.scope_graph
                    .add_edge(*parent, item_idx, "item".to_string());
            }
            Use(item_use) => {
                let use_idx = self.scope_graph.add_node(Node::Use { item_use });
                self.scope_graph.add_edge(
                    *self.scope_ancestry.last().unwrap(),
                    use_idx,
                    "use".to_string(),
                );
            }
            _other => {}
        }
    }

    fn others_declared_with_same_name_in_scope(&self, name: Name<'ast>) -> Vec<&'ast Item> {
        let parent = self.scope_ancestry.last().unwrap();
        // Check parent neighbors
        return self
            .scope_graph
            .neighbors(*parent)
            .filter_map(|neighbor| {
                if let Node::Item { item, .. } = self.scope_graph[neighbor] {
                    if name.conflicts_with(&item.into()) {
                        Some(item)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect::<Vec<&'ast Item>>();
    }
}

#[derive(Debug)]
pub enum Node<'ast> {
    /// A dummy node for the root of a tree
    Root {
        file: Rc<File>,
        exports: Vec<NodeIndex>,
    },
    Item {
        item: &'ast Item,
        ident: &'ast Ident,
    },
    Mod {
        item_mod: &'ast ItemMod,
        /// Exports: (from item, to list of roots/mods)
        exports: HashMap<NodeIndex, Vec<NodeIndex>>,
        /// The file backing this mod, whether it's its own file or the file of an ancestor
        file: Rc<File>,
    },
    Use {
        item_use: &'ast ItemUse,
    },
}

impl<'ast> Display for Node<'ast> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        match self {
            Self::Root { .. } => write!(f, "root"),
            Self::Item { ident, .. } => write!(f, "{}", ident),
            Self::Mod { item_mod, .. } => write!(f, "mod {}", item_mod.ident),
            Self::Use { item_use, .. } => write!(f, "use"),
        }
    }
}
