use fxhash::FxHashMap as HashMap;
use rhdl::{
    ast::{
        Ident, ItemArch, ItemConst, ItemEntity, ItemEnum, ItemFn, ItemImpl, ItemMod, ItemStruct,
        ItemTrait, ItemType, ItemUse, NamedField, UnnamedField, UseTreeGlob, UseTreeName,
        UseTreeRename, Variant, Vis,
    },
    visit::Visit,
};

use std::fmt::Debug;

use crate::error::ItemHint;
use crate::find_file::FileId;

#[derive(Default, Debug)]
pub struct ResolutionGraph<'ast> {
    pub inner: Vec<ResolutionNode<'ast>>,
    pub roots: Vec<ResolutionIndex>,
    /// key is exported to value
    /// if value is None, it is visible from anywhere
    pub exports: HashMap<ResolutionIndex, Option<ResolutionIndex>>,
    pub content_files: HashMap<ResolutionIndex, FileId>,
}

impl<'ast> ResolutionGraph<'ast> {
    pub fn add_node(&mut self, node: ResolutionNode<'ast>) -> ResolutionIndex {
        let idx = self.inner.len();
        if let ResolutionNode::Root { .. } = &node {
            self.roots.push(idx);
        }
        self.inner.push(node);

        idx
    }

    pub fn add_child(&mut self, parent: ResolutionIndex, child: ResolutionIndex) {
        let name = self.inner[child].name();
        if let Some(children) = self.inner[parent].children_mut() {
            children.entry(name).or_default().push(child)
        }
    }

    pub fn node_indices(&self) -> impl Iterator<Item = ResolutionIndex> {
        0..self.inner.len()
    }

    pub fn file(&self, node: ResolutionIndex) -> FileId {
        let mut next_parent = match &self.inner[node] {
            ResolutionNode::Root { .. } => node,
            ResolutionNode::Leaf { parent, .. } | ResolutionNode::Branch { parent, .. } => *parent,
        };
        loop {
            next_parent = match &self.inner[next_parent] {
                ResolutionNode::Root { .. } => return self.content_files[&next_parent],
                ResolutionNode::Branch {
                    branch: Branch::Mod(_),
                    parent,
                    ..
                } => {
                    if let Some(content_file) = self.content_files.get(&next_parent) {
                        return *content_file;
                    }
                    *parent
                }
                ResolutionNode::Leaf { parent, .. } | ResolutionNode::Branch { parent, .. } => {
                    *parent
                }
            }
        }
    }
}

pub type ResolutionIndex = usize;

#[derive(Debug)]
pub enum ResolutionNode<'ast> {
    Root {
        /// This information comes from an external source
        name: String,
        children: HashMap<Option<&'ast Ident>, Vec<ResolutionIndex>>,
    },
    Branch {
        parent: ResolutionIndex,
        branch: Branch<'ast>,
        /// Child branches/leaves, whether named (structs/enums/etc.) or not (impls)
        children: HashMap<Option<&'ast Ident>, Vec<ResolutionIndex>>,
    },
    Leaf {
        parent: ResolutionIndex,
        leaf: Leaf<'ast>,
    },
}

impl<'ast> ResolutionNode<'ast> {
    pub fn visibility(&self) -> Option<&Vis> {
        match self {
            ResolutionNode::Leaf {
                leaf: Leaf::Const(ItemConst { vis, .. }),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: Leaf::Type(ItemType { vis, .. }),
                ..
            }
            | ResolutionNode::Branch {
                branch: Branch::Fn(ItemFn { vis, .. }, ..),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: Leaf::NamedField(NamedField { vis, .. }),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: Leaf::UnnamedField(UnnamedField { vis, .. }),
                ..
            }
            | ResolutionNode::Branch {
                branch: Branch::Struct(ItemStruct { vis, .. }, ..),
                ..
            }
            | ResolutionNode::Branch {
                branch: Branch::Trait(ItemTrait { vis, .. }, ..),
                ..
            }
            | ResolutionNode::Branch {
                branch: Branch::Enum(ItemEnum { vis, .. }, ..),
                ..
            }
            | ResolutionNode::Branch {
                branch: Branch::Use(ItemUse { vis, .. }, ..),
                ..
            }
            | ResolutionNode::Branch {
                branch: Branch::Mod(ItemMod { vis, .. }, ..),
                ..
            } => vis.as_ref(),
            ResolutionNode::Branch {
                branch: Branch::Variant(_),
                ..
            }
            | ResolutionNode::Branch {
                branch: Branch::Arch(..),
                ..
            }
            | ResolutionNode::Root { .. }
            | ResolutionNode::Branch {
                branch: Branch::Impl(_),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: Leaf::UseName(..),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: Leaf::UseRename(..),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: Leaf::UseGlob(..),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: Leaf::Entity(..),
                ..
            } => None,
        }
    }

    pub fn is_valid_use_path_segment(&self) -> bool {
        matches!(self,
        ResolutionNode::Branch {
            branch: Branch::Mod { .. },
            ..
        }
        | ResolutionNode::Root { .. })
    }

    pub fn is_type_existence_checking_candidate(&self) -> bool {
        matches!(self,
        ResolutionNode::Branch {
            branch: Branch::Impl { .. },
            ..
        }
        | ResolutionNode::Branch {
            branch: Branch::Trait { .. },
            ..
        }
        | ResolutionNode::Branch {
            branch: Branch::Fn { .. },
            ..
        }
        | ResolutionNode::Branch {
            branch: Branch::Struct { .. },
            ..
        }
        | ResolutionNode::Branch {
            branch: Branch::Enum { .. },
            ..
        }
        | ResolutionNode::Leaf {
            leaf: Leaf::Const { .. },
            ..
        }
        | ResolutionNode::Leaf {
            leaf: Leaf::Type { .. },
            ..
        })
    }

    /// The names in a name class must be unique
    /// * mods can conflict with types, other mods, and crates
    /// * Fns & Vars can conflict with types
    /// * types can conflict with anything
    /// * macros only conflict with macros
    pub fn in_same_name_class(&self, other: &ResolutionNode) -> bool {
        use Branch::*;
        use Leaf::*;
        match self {
            ResolutionNode::Root { .. } => match other {
                ResolutionNode::Root { .. } => true,
                _ => false,
            },
            ResolutionNode::Branch {
                branch: Impl(_), ..
            }
            | ResolutionNode::Branch {
                branch: Arch(_), ..
            } => false,
            ResolutionNode::Leaf {
                leaf: NamedField(_),
                ..
            } => match other {
                ResolutionNode::Leaf {
                    leaf: NamedField(_),
                    ..
                } => true,
                _ => false,
            },
            ResolutionNode::Leaf {
                leaf: UnnamedField(_),
                ..
            } => false,
            ResolutionNode::Branch {
                branch: Variant(_), ..
            } => match other {
                ResolutionNode::Branch {
                    branch: Variant(_), ..
                } => true,
                _ => false,
            },
            ResolutionNode::Branch { branch: Mod(_), .. } => match other {
                ResolutionNode::Branch { branch: Mod(_), .. }
                | ResolutionNode::Branch {
                    branch: Struct(_), ..
                }
                | ResolutionNode::Branch {
                    branch: Enum(_), ..
                }
                | ResolutionNode::Branch {
                    branch: Trait(_), ..
                }
                | ResolutionNode::Leaf { leaf: Type(_), .. }
                | ResolutionNode::Leaf {
                    leaf: UseName(..), ..
                }
                | ResolutionNode::Leaf {
                    leaf: UseRename(..),
                    ..
                } => true,
                _ => false,
            },
            ResolutionNode::Leaf { leaf: Const(_), .. }
            | ResolutionNode::Branch { branch: Fn(_), .. } => match other {
                ResolutionNode::Leaf { leaf: Const(_), .. }
                | ResolutionNode::Branch { branch: Fn(_), .. }
                | ResolutionNode::Branch {
                    branch: Struct(_), ..
                }
                | ResolutionNode::Branch {
                    branch: Enum(_), ..
                }
                | ResolutionNode::Branch {
                    branch: Trait(_), ..
                }
                | ResolutionNode::Leaf { leaf: Type(_), .. }
                | ResolutionNode::Leaf {
                    leaf: UseName(..), ..
                }
                | ResolutionNode::Leaf {
                    leaf: UseRename(..),
                    ..
                } => true,
                _ => false,
            },
            ResolutionNode::Branch {
                branch: Struct(_), ..
            }
            | ResolutionNode::Branch {
                branch: Enum(_), ..
            }
            | ResolutionNode::Leaf {
                leaf: Entity(_), ..
            }
            | ResolutionNode::Branch {
                branch: Trait(_), ..
            }
            | ResolutionNode::Leaf { leaf: Type(_), .. }
            | ResolutionNode::Leaf {
                leaf: UseName(..), ..
            }
            | ResolutionNode::Leaf {
                leaf: UseRename(..),
                ..
            } => true,
            ResolutionNode::Branch { branch: Use(_), .. }
            | ResolutionNode::Leaf {
                leaf: UseGlob(..), ..
            } => false,
        }
    }

    pub fn is_use(&self) -> bool {
        matches!(self, ResolutionNode::Branch {
            branch: Branch::Use(_),
            ..
        })
    }

    pub fn is_trait(&self) -> bool {
        matches!(self, ResolutionNode::Branch {
            branch: Branch::Trait(_),
            ..
        })
    }

    pub fn is_impl(&self) -> bool {
        matches!(self, ResolutionNode::Branch {
            branch: Branch::Impl(_),
            ..
        })
    }

    pub fn is_trait_or_impl(&self) -> bool {
        self.is_trait() || self.is_impl()
    }

    pub fn is_type(&self) -> bool {
        matches!(self,
        ResolutionNode::Branch {
            branch: Branch::Struct(_),
            ..
        }
        | ResolutionNode::Branch {
            branch: Branch::Enum(_),
            ..
        }
        | ResolutionNode::Leaf {
            leaf: Leaf::Type(_),
            ..
        })
    }

    pub fn children(&self) -> Option<&HashMap<Option<&'ast Ident>, Vec<ResolutionIndex>>> {
        if let ResolutionNode::Root { children, .. } | ResolutionNode::Branch { children, .. } =
            self
        {
            Some(children)
        } else {
            None
        }
    }

    pub fn children_mut(
        &mut self,
    ) -> Option<&mut HashMap<Option<&'ast Ident>, Vec<ResolutionIndex>>> {
        if let ResolutionNode::Root { children, .. } | ResolutionNode::Branch { children, .. } =
            self
        {
            Some(children)
        } else {
            None
        }
    }

    pub fn parent(&self) -> Option<ResolutionIndex> {
        if let ResolutionNode::Leaf { parent, .. } | ResolutionNode::Branch { parent, .. } = self {
            Some(*parent)
        } else {
            None
        }
    }

    pub fn name(&self) -> Option<&'ast Ident> {
        match self {
            ResolutionNode::Root { .. } => None,
            ResolutionNode::Branch { branch, .. } => match branch {
                Branch::Mod(m) => Some(&m.ident),
                Branch::Impl(_) => None,
                Branch::Trait(t) => Some(&t.ident),
                Branch::Fn(f) => Some(&f.sig.ident),
                Branch::Struct(s) => Some(&s.ident),
                Branch::Enum(e) => Some(&e.ident),
                Branch::Variant(v) => Some(&v.ident),
                Branch::Use(_) => None,
                Branch::Arch(_) => None,
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::NamedField(f) => Some(&f.ident),
                Leaf::Const(c) => Some(&c.ident),
                Leaf::Type(t) => Some(&t.ident),
                Leaf::UseRename(r, _) => Some(&r.rename),
                Leaf::UseName(n, _) => Some(*n),
                Leaf::UseGlob(_, _) | Leaf::UnnamedField(_) => None,
                Leaf::Entity(e) => Some(&e.ident),
            },
        }
    }

    pub fn visit<V>(&self, v: &mut V)
    where
        V: Visit<'ast>,
    {
        match self {
            ResolutionNode::Root { .. } => {}
            ResolutionNode::Branch { branch, .. } => match branch {
                Branch::Mod(m) => v.visit_item_mod(m),
                Branch::Impl(i) => v.visit_item_impl(i),
                Branch::Trait(t) => v.visit_item_trait(t),
                Branch::Fn(f) => v.visit_item_fn(f),
                Branch::Struct(s) => v.visit_item_struct(s),
                Branch::Enum(e) => v.visit_item_enum(e),
                Branch::Variant(var) => v.visit_variant(var),
                Branch::Use(u) => v.visit_item_use(u),
                Branch::Arch(a) => v.visit_item_arch(a),
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::NamedField(f) => v.visit_named_field(f),
                Leaf::UnnamedField(f) => v.visit_unnamed_field(f),
                Leaf::Const(c) => v.visit_item_const(c),
                Leaf::Type(t) => v.visit_item_type(t),
                Leaf::UseName(n, _) => v.visit_use_tree_name(n),
                Leaf::UseRename(r, _) => v.visit_use_tree_rename(r),
                Leaf::UseGlob(g, _) => v.visit_use_tree_glob(g),
                Leaf::Entity(e) => v.visit_item_entity(e),
            },
        }
    }

    pub fn item_hint(&self) -> Option<ItemHint> {
        match self {
            ResolutionNode::Root { .. } => Some(ItemHint::InternalNamedRootScope),
            ResolutionNode::Branch { branch, .. } => match branch {
                Branch::Mod(..) => Some(ItemHint::InternalNamedChildScope),
                Branch::Impl(..) => Some(ItemHint::Item),
                Branch::Trait(..) => Some(ItemHint::Trait),
                Branch::Fn(..) => Some(ItemHint::Fn),
                Branch::Struct(..) => Some(ItemHint::Type),
                Branch::Enum(..) => Some(ItemHint::Type),
                Branch::Variant(..) => None,
                Branch::Use(..) => None,
                Branch::Arch(..) => Some(ItemHint::Item),
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::NamedField(..) => None,
                Leaf::UnnamedField(..) => None,
                Leaf::Const(..) => Some(ItemHint::Var),
                Leaf::Type(..) => Some(ItemHint::Type),
                Leaf::UseName(..) => Some(ItemHint::Item),
                Leaf::UseRename(..) => Some(ItemHint::Item),
                Leaf::UseGlob(..) => None,
                Leaf::Entity(..) => Some(ItemHint::Type),
            },
        }
    }
}

#[derive(Debug)]
pub enum Branch<'ast> {
    Mod(&'ast ItemMod),
    /// Imports are treated as a special "children" case
    /// globs have no "ident"
    /// renames use the renamed ident
    /// names use the original ident
    Use(&'ast ItemUse),
    // UsePath(&'ast UsePath),
    // UseGroup(&'ast UseGroup),
    Fn(&'ast ItemFn),
    Struct(&'ast ItemStruct),
    Enum(&'ast ItemEnum),
    Variant(&'ast Variant),
    Impl(&'ast ItemImpl),
    Arch(&'ast ItemArch),
    // TODO: split trait children down into leaves
    Trait(&'ast ItemTrait),
}

#[derive(Debug)]
pub enum Leaf<'ast> {
    UseName(&'ast UseTreeName, Vec<ResolutionIndex>),
    UseRename(&'ast UseTreeRename, Vec<ResolutionIndex>),
    UseGlob(&'ast UseTreeGlob, ResolutionIndex),
    Const(&'ast ItemConst),
    Type(&'ast ItemType),
    NamedField(&'ast NamedField),
    UnnamedField(&'ast UnnamedField),
    Entity(&'ast ItemEntity),
}
