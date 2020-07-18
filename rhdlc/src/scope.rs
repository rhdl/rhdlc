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
use petgraph::{graph::DefaultIx, graph::NodeIndex, Direction, Graph};
use std::fmt::Display;
use syn::{File, Ident, Item, ItemMod};

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
    fn in_same_name_class(&self, other: &Name<'ast>) -> bool {
        use Name::*;
        match self {
            Function(_) | Variable(_) => match other {
                Function(_) | Variable(_) => true,
                _ => false,
            },
            Macro(_) => match other {
                Macro(_) => true,
                _ => false,
            },
            Type(_) | Mod(_) | Crate(_) => match other {
                Type(_) | Mod(_) | Crate(_) => true,
                _ => false,
            },
            Other => match other {
                Other => true,
                _ => false,
            },
        }
    }

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
                Other => true,
                _ => false,
            },
        }
    }

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
                debug!("Skipping use",);
                Other
            }
            unknown => {
                warn!("Not handling {:?}", unknown);
                Other
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ScopeBuilder<'ast> {
    pub graph: Graph<Node<'ast>, String>,
    ancestry: Vec<NodeIndex<DefaultIx>>,
}

impl<'ast> ScopeBuilder<'ast> {
    /// Find all names given a source forest
    /// Externals are paths to standalone source code: a top + lib.rs of each crate
    /// Check for declaration name conflicts and skip those that violate this
    pub fn stage_one(&mut self, file_graph: &'ast Graph<File, ItemMod>) {
        for f in file_graph.externals(Direction::Incoming) {
            self.ancestry.clear();
            file_graph[f].items.iter().for_each(|i| self.add_item(i));
        }
    }

    fn add_mod(&mut self, item: &'ast Item) {
        if let Item::Mod(syn::ItemMod { ident, content, .. }) = &item {
            let others = self.others_declared_with_same_name_in_scope(item.into());
            if others.len() > 0 {
                // TODO: create name conflict errors for this
                warn!(
                    "duplicate mod names! {:?}",
                    others
                        .iter()
                        .map(|i| Name::from(*i))
                        .collect::<Vec<Name<'ast>>>()
                );
                return;
            }
            let mod_idx = self.graph.add_node(Node { item, ident: ident });
            if let Some(parent) = self.ancestry.last() {
                self.graph
                    .add_edge(*parent, mod_idx, "mod -> mod".to_string());
            }

            if let Some((_, items)) = content {
                self.ancestry.push(mod_idx);
                items.iter().for_each(|i| self.add_item(i));
                self.ancestry.pop();
            }
        }
    }

    fn add_item(&mut self, item: &'ast Item) {
        use Name::*;
        let name = Name::from(item);
        match name {
            Mod(_) => {
                self.add_mod(item);
            }
            Crate(ident) | Type(ident) | Variable(ident) | Macro(ident) | Function(ident) => {
                let others = self.others_declared_with_same_name_in_scope(name);
                if others.len() > 0 {
                    // TODO: create name conflict errors for this
                    warn!(
                        "duplicate item names! {:?}",
                        others
                            .iter()
                            .map(|i| Name::from(*i))
                            .collect::<Vec<Name<'ast>>>()
                    );
                    return;
                }
                let item_idx = self.graph.add_node(Node { item, ident });
                if let Some(parent) = self.ancestry.last() {
                    self.graph
                        .add_edge(*parent, item_idx, "mod -> item".to_string());
                }
            }
            Other => {}
        }
    }

    fn others_declared_with_same_name_in_scope(&self, name: Name<'ast>) -> Vec<&'ast Item> {
        if let Some(parent) = self.ancestry.last() {
            // Check parent neighbors
            return self
                .graph
                .neighbors(*parent)
                .filter_map(|neighbor| {
                    let item = self.graph[neighbor].item;
                    if name.conflicts_with(&item.into()) {
                        Some(item)
                    } else {
                        None
                    }
                })
                .collect::<Vec<&'ast Item>>();
        }
        // Check top level neighbors
        self.graph
            .externals(Direction::Incoming)
            .filter_map(|neighbor| {
                let item = self.graph[neighbor].item;
                if name.conflicts_with(&item.into()) {
                    Some(item)
                } else {
                    None
                }
            })
            .collect::<Vec<&'ast Item>>()
    }
}

#[derive(Debug)]
pub struct Node<'ast> {
    item: &'ast Item,
    ident: &'ast Ident,
}

impl<'ast> Display for Node<'ast> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "{}", self.ident)
    }
}
