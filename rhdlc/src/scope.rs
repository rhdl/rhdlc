use log::{debug, error, warn};
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
use petgraph::{graph::NodeIndex, visit::EdgeRef, Direction, Graph};
use std::collections::HashMap;
use std::fmt::Display;
use syn::{File, Ident, ImplItem, Item, ItemImpl, ItemMod, ItemUse, Visibility};

use crate::error::{MultipleDefinitionError, ScopeError};

#[derive(Debug, PartialEq, Eq, Clone)]
enum Name<'ast> {
    Function(&'ast Ident),
    Variable(&'ast Ident),
    Macro(&'ast Ident),
    Type(&'ast Ident),
    Mod(&'ast Ident),
    Crate(&'ast Ident),
    Other,
}

impl<'ast> Name<'ast> {
    /// The names in a name class must be unique
    /// * mods can conflict with types, other mods, and crates
    /// * functions & variables can conflict with types
    /// * types can conflict with anything
    /// * macros only conflict with macros
    fn in_same_name_class(&self, other: &Name<'ast>) -> bool {
        use Name::*;
        match self {
            Mod(_) | Crate(_) => match other {
                Type(_) | Mod(_) | Crate(_) => true,
                _ => false,
            },
            Function(_) | Variable(_) => match other {
                Function(_) | Variable(_) | Type(_) => true,
                _ => false,
            },
            Type(_) => match other {
                Function(_) | Variable(_) | Type(_) | Mod(_) | Crate(_) => true,
                _ => false,
            },
            Macro(_) => match other {
                Macro(_) => true,
                _ => false,
            },
            Other => match other {
                Other => true,
                _ => false,
            },
        }
    }

    /// Check ident
    fn has_same_ident(&self, other: &Name<'ast>) -> bool {
        use Name::*;
        match self {
            Function(ident) | Variable(ident) | Macro(ident) | Type(ident) | Mod(ident)
            | Crate(ident) => match other {
                Function(other_ident)
                | Variable(other_ident)
                | Macro(other_ident)
                | Type(other_ident)
                | Mod(other_ident)
                | Crate(other_ident) => ident == other_ident,
                _ => false,
            },
            Other => match other {
                Other => false,
                _ => false,
            },
        }
    }

    /// Two names in the same name class with the same identifier are conflicting
    fn conflicts_with(&self, other: &Name<'ast>) -> bool {
        self.in_same_name_class(other) && self.has_same_ident(other)
    }
}

impl<'ast> From<&'ast Item> for Name<'ast> {
    fn from(item: &'ast Item) -> Self {
        use Item::*;
        use Name::Other;
        match item {
            ExternCrate(syn::ItemExternCrate { ident, .. }) => Self::Crate(ident),
            Mod(syn::ItemMod { ident, .. }) => Self::Mod(ident),
            Verbatim(_) | ForeignMod(_) => {
                warn!("Cannot handle {:?}", item);
                Other
            }
            Struct(syn::ItemStruct { ident, .. })
            | Enum(syn::ItemEnum { ident, .. })
            | Trait(syn::ItemTrait { ident, .. })
            | TraitAlias(syn::ItemTraitAlias { ident, .. })
            | Type(syn::ItemType { ident, .. })
            | Union(syn::ItemUnion { ident, .. }) => Self::Type(ident),
            Const(syn::ItemConst { ident, .. }) | Static(syn::ItemStatic { ident, .. }) => {
                Self::Variable(ident)
            }
            Fn(syn::ItemFn {
                sig: syn::Signature { ident, .. },
                ..
            }) => Self::Function(ident),
            Macro(syn::ItemMacro {
                ident: Some(ident), ..
            })
            | Macro2(syn::ItemMacro2 { ident, .. }) => Self::Macro(ident),
            Impl(_) => {
                debug!("Skipping impl, tie this to struct in next scope stage");
                Other
            }
            Use(_) => {
                debug!("Skipping use");
                Other
            }
            unknown => {
                // syn is implemented so that any additions to the items in Rust syntax will fall into this arm
                error!("Not handling {:?}", unknown);
                Other
            }
        }
    }
}

#[derive(Debug)]
pub struct ScopeBuilder<'ast> {
    pub file_graph: &'ast crate::resolve::FileGraph,
    pub scope_graph: Graph<Node<'ast>, String>,
    pub errors: Vec<ScopeError>,
    scope_ancestry: Vec<NodeIndex>,
    file_ancestry: Vec<NodeIndex>,
}

impl<'ast> From<&'ast crate::resolve::FileGraph> for ScopeBuilder<'ast> {
    fn from(file_graph: &'ast crate::resolve::FileGraph) -> Self {
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
        for file in files {
            if self.scope_ancestry.len() > 0 {
                warn!(
                    "scope_ancestry was not empty, clearing: {:?}",
                    self.scope_ancestry
                );
                self.scope_ancestry.clear();
            }
            let idx = self.scope_graph.add_node(Node::Root {
                file_index: file,
                exports: vec![],
            });
            self.scope_ancestry.push(idx);
            self.file_ancestry.push(file);
            self.file_graph[file]
                .syn
                .items
                .iter()
                .for_each(|i| self.add_mod(i));
            self.file_ancestry.pop();
            self.scope_ancestry.pop();
        }

        // Stage two: apply visibility
        for node in self.scope_graph.node_indices() {
            self.apply_visibility(node);
        }

        // Stage three: delete use nodes

        // Stage four: tie impls
    }

    fn find_name_conflicts(&mut self) {
        for node in self.scope_graph.node_indices() {
            match self.scope_graph[node] {
                Node::Root { .. } | Node::Mod { .. } => {
                    // Check the scopes for conflicts
                    let mut names: Vec<Name> = vec![];
                    let mut errors: Vec<ScopeError> = vec![];
                    for child in self.scope_graph.neighbors(node) {
                        match self.scope_graph[child] {
                            Node::Item { ident, item, .. } => {
                                names.push(Name::from(item));
                            }
                            _ => {}
                        }
                    }
                    self.errors.append(&mut errors);
                }
                _ => {}
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

    /// If a node overrides its visibility, apply it
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
                Node::Mod { exports, .. } => exports
                    .entry(node)
                    .or_insert_with(Vec::default)
                    .extend(&grandparents),
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
                Node::Mod { exports, .. } => exports
                    .entry(node)
                    .or_insert_with(Vec::default)
                    .extend(&roots),
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
                        file_index: None,
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

                    let edge = self.file_ancestry.last().and_then(|parent| {
                        self.file_graph
                            .edges(*parent)
                            .filter(|edge| full_ident_path.ends_with(edge.weight()))
                            .max_by_key(|edge| edge.weight().len())
                    });

                    if let Some(edge) = edge {
                        let mod_idx = self.scope_graph.add_node(Node::Mod {
                            item_mod,
                            exports: HashMap::default(),
                            file_index: Some(edge.target()),
                        });
                        if let Some(parent) = self.scope_ancestry.last() {
                            self.scope_graph
                                .add_edge(*parent, mod_idx, "mod".to_string());
                        }
                        self.scope_ancestry.push(mod_idx);
                        self.file_ancestry.push(edge.target());
                        self.file_graph[edge.target()]
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
        file_index: NodeIndex,
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
        /// Whether this mod is backed by a file
        file_index: Option<NodeIndex>,
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
