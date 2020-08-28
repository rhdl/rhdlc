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

use petgraph::{graph::NodeIndex, Direction, Graph};
use syn::{
    visit::Visit, Ident, ItemConst, ItemEnum, ItemFn, ItemImpl, ItemMod, ItemStruct, ItemTrait,
    ItemType, ItemUse,
};

use crate::error::{InvalidRawIdentifierError, ResolutionError};
use crate::find_file::{File, FileGraph};

mod name;
use name::Name;

mod r#use;
use r#use::UseType;

mod build;
mod conflicts;
mod path;
mod r#pub;
mod type_existence;

pub type ScopeGraph<'ast> = Graph<Node<'ast>, String>;

#[derive(Debug)]
pub struct Resolver<'ast> {
    file_graph: &'ast FileGraph,
    pub scope_graph: ScopeGraph<'ast>,
    pub errors: Vec<ResolutionError>,
    resolved_uses: HashSet<NodeIndex>,
}

impl<'ast> From<&'ast FileGraph> for Resolver<'ast> {
    fn from(file_graph: &'ast FileGraph) -> Self {
        Self {
            file_graph,
            scope_graph: Default::default(),
            errors: vec![],
            resolved_uses: Default::default(),
        }
    }
}

impl<'ast> Resolver<'ast> {
    /// Find all names given a source forest
    /// Externals are paths to standalone source code: a top + lib.rs of each crate
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
            let mut builder = build::ScopeBuilder {
                errors: &mut self.errors,
                file_graph: &mut self.file_graph,
                scope_graph: &mut self.scope_graph,
                file_ancestry: vec![file_index],
                scope_ancestry: vec![scope_index],
            };
            builder.visit_file(&self.file_graph[file_index].syn);
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
        for use_index in use_indices {
            let mut use_resolver = r#use::UseResolver {
                resolved_uses: &mut self.resolved_uses,
                scope_graph: &mut self.scope_graph,
                errors: &mut self.errors,
            };
            use_resolver.resolve_use(use_index);
        }
    }

    pub fn check_graph(&mut self) {
        self.errors.append(&mut self.find_invalid_names());
        {
            let mut conflict_checker = conflicts::ConflictChecker {
                scope_graph: &self.scope_graph,
                errors: &mut self.errors,
            };
            conflict_checker.visit_all();
        }
        {
            let mut type_existence_checker = type_existence::TypeExistenceChecker {
                scope_graph: &self.scope_graph,
                errors: &mut self.errors,
            };
            type_existence_checker.visit_all();
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
        // / impl Option<Trait> Option<for> **NodeIndex**
        // / which cannot be resolved at first
        // r#trait: Option<NodeIndex>,
        // r#for: Option<NodeIndex>,
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
    fn is_nameless_scope(&self) -> bool {
        match self {
            Self::Root { .. } | Self::Mod { .. } => false,
            _ => true,
        }
    }

    fn is_trait(&self) -> bool {
        match self {
            Self::Trait { .. } => true,
            _ => false,
        }
    }

    fn is_impl(&self) -> bool {
        match self {
            Self::Impl { .. } => true,
            _ => false,
        }
    }

    fn is_type(&self) -> bool {
        match self {
            Self::Struct { .. } | Self::Type { .. } | Self::Enum { .. } => true,
            _ => false,
        }
    }

    fn visit<V>(&self, v: &mut V)
    where
        V: Visit<'ast>,
    {
        match self {
            Self::Root { .. } => {}
            Self::Mod { .. } => {}
            Self::Struct { item_struct, .. } => v.visit_item_struct(item_struct),
            Self::Trait { item_trait, .. } => v.visit_item_trait(item_trait),
            Self::Type { item_type, .. } => v.visit_item_type(item_type),
            Self::Enum { item_enum, .. } => v.visit_item_enum(item_enum),
            Self::Fn { item_fn, .. } => v.visit_item_fn(item_fn),
            Self::Const { item_const, .. } => v.visit_item_const(item_const),
            Self::Use { item_use, .. } => v.visit_item_use(item_use),
            Self::Impl { item_impl, .. } => v.visit_item_impl(item_impl),
        }
    }

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
            Self::Struct { item_struct, .. } => vec![Name::from(*item_struct)],
            Self::Trait { item_trait, .. } => vec![Name::from(*item_trait)],
            Self::Type { item_type, .. } => vec![Name::from(*item_type)],
            Self::Enum { item_enum, .. } => vec![Name::from(*item_enum)],
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
            Self::Impl { .. } => {
                write!(f, "impl")?;
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
