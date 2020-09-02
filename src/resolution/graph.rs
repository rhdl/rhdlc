use fxhash::FxHashMap as HashMap;
use std::fmt::Debug;
use std::rc::Rc;

use syn::{
    visit::Visit, Field, Ident, ImplItem, ImplItemConst, ImplItemMethod, ImplItemType, Item,
    ItemConst, ItemEnum, ItemFn, ItemImpl, ItemMacro2, ItemMod, ItemStatic, ItemStruct, ItemTrait,
    ItemTraitAlias, ItemType, ItemUnion, ItemUse, Stmt, TraitItem, TraitItemConst, TraitItemMethod,
    TraitItemType, UseGlob, UseName, UseRename, Variant, Visibility,
};

use crate::find_file::File;

#[derive(Default, Debug)]
pub struct ResolutionGraph<'ast> {
    pub inner: Vec<ResolutionNode<'ast>>,
    pub roots: Vec<ResolutionIndex>,
    /// key is exported to value
    /// if value is None, it is visible from anywhere
    pub exports: HashMap<ResolutionIndex, Option<ResolutionIndex>>,
    pub content_files: HashMap<ResolutionIndex, Rc<File>>,
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
        self.inner[parent]
            .children_mut()
            .map(|hash_map| hash_map.entry(name).or_default().push(child));
    }

    pub fn node_indices(&self) -> impl Iterator<Item = ResolutionIndex> {
        0..self.inner.len()
    }

    pub fn file(&self, node: ResolutionIndex) -> Rc<File> {
        let mut next_parent = match &self.inner[node] {
            ResolutionNode::Root { .. } => node,
            ResolutionNode::Leaf { parent, .. } | ResolutionNode::Branch { parent, .. } => *parent,
        };
        loop {
            next_parent = match &self.inner[next_parent] {
                ResolutionNode::Root { .. } => return self.content_files[&next_parent].clone(),
                ResolutionNode::Branch {
                    branch: Branch::Mod(_),
                    parent,
                    ..
                } => {
                    if let Some(content_file) = self.content_files.get(&next_parent) {
                        return content_file.clone();
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
        /// Only the top level entity is allowed to have no name
        /// TODO: figure out how to reconcile library-building behavior of rustc
        /// with the fact that there are no binaries for RHDL...
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
    pub fn visibility(&self) -> Option<&Visibility> {
        match self {
            ResolutionNode::Leaf {
                leaf: Leaf::Const(some_item_const),
                ..
            } => some_item_const.visibility(),
            ResolutionNode::Leaf {
                leaf: Leaf::Type(some_item_type),
                ..
            } => some_item_type.visibility(),

            ResolutionNode::Branch {
                branch: Branch::Fn(some_item_fn, ..),
                ..
            } => some_item_fn.visibility(),
            ResolutionNode::Leaf {
                leaf: Leaf::Field(Field { vis, .. }),
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
            } => Some(vis),
            ResolutionNode::Branch {
                branch: Branch::Variant(_),
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
            } => None,
        }
    }

    pub fn is_valid_use_path_segment(&self) -> bool {
        match self {
            ResolutionNode::Branch {
                branch: Branch::Mod { .. },
                ..
            }
            | ResolutionNode::Root { .. } => true,
            _ => false,
        }
    }

    pub fn is_type_existence_checking_candidate(&self) -> bool {
        match self {
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
            } => true,
            _ => false,
        }
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
            } => false,
            ResolutionNode::Leaf { leaf: Field(_), .. } => match other {
                ResolutionNode::Leaf { leaf: Field(_), .. } => true,
                _ => false,
            },
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
        match self {
            ResolutionNode::Branch {
                branch: Branch::Use(_),
                ..
            } => true,
            _ => false,
        }
    }

    pub fn is_trait(&self) -> bool {
        match self {
            ResolutionNode::Branch {
                branch: Branch::Trait(_),
                ..
            } => true,
            _ => false,
        }
    }

    pub fn is_impl(&self) -> bool {
        match self {
            ResolutionNode::Branch {
                branch: Branch::Impl(_),
                ..
            } => true,
            _ => false,
        }
    }

    pub fn is_trait_or_impl(&self) -> bool {
        self.is_trait() || self.is_impl()
    }

    pub fn is_type(&self) -> bool {
        match self {
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
            } => true,
            _ => false,
        }
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
                Branch::Fn(f) => f.ident(),
                Branch::Struct(s) => Some(&s.ident),
                Branch::Enum(e) => Some(&e.ident),
                Branch::Variant(v) => Some(&v.ident),
                Branch::Use(_) => None,
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::Field(f) => f.ident.as_ref(),
                Leaf::Const(c) => c.ident(),
                Leaf::Type(t) => t.ident(),
                Leaf::UseRename(r, _) => Some(&r.rename),
                Leaf::UseName(n, _) => Some(&n.ident),
                Leaf::UseGlob(_, _) => None,
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
                Branch::Fn(f) => f.visit(v),
                Branch::Struct(s) => v.visit_item_struct(s),
                Branch::Enum(e) => v.visit_item_enum(e),
                Branch::Variant(var) => v.visit_variant(var),
                Branch::Use(u) => v.visit_item_use(u),
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::Field(f) => v.visit_field(f),
                Leaf::Const(c) => c.visit(v),
                Leaf::Type(t) => t.visit(v),
                Leaf::UseName(n, _) => v.visit_use_name(n),
                Leaf::UseRename(r, _) => v.visit_use_rename(r),
                Leaf::UseGlob(g, _) => v.visit_use_glob(g),
            },
        }
    }
}

#[derive(Debug)]
pub enum Branch<'ast> {
    Mod(&'ast ItemMod),
    Impl(&'ast ItemImpl),
    Trait(&'ast ItemTrait),
    Fn(&'ast dyn SomeFn<'ast>),
    Struct(&'ast ItemStruct),
    Enum(&'ast ItemEnum),
    Variant(&'ast Variant),
    /// Imports are treated as a special "children" case
    /// globs have no "ident"
    /// renames use the renamed ident
    /// names use the original ident
    Use(&'ast ItemUse),
    // UsePath(&'ast UsePath),
    // UseGroup(&'ast UseGroup)
}

#[derive(Debug)]
pub enum Leaf<'ast> {
    Field(&'ast Field),
    Const(&'ast dyn SomeConst<'ast>),
    Type(&'ast dyn SomeType<'ast>),
    UseName(&'ast UseName, Vec<ResolutionIndex>),
    UseRename(&'ast UseRename, Vec<ResolutionIndex>),
    UseGlob(&'ast UseGlob, ResolutionIndex),
}

pub trait SomeItem<'ast> {
    fn ident(&'ast self) -> Option<&'ast Ident>;
}

macro_rules! some_item_fn {
    ($ident: ident) => {
        impl<'ast> SomeItem<'ast> for $ident {
            fn ident(&'ast self) -> Option<&'ast Ident> {
                Some(&self.sig.ident)
            }
        }
    };
}
some_item_fn!(ItemFn);
some_item_fn!(ImplItemMethod);
some_item_fn!(TraitItemMethod);

macro_rules! some_item_type_or_const {
    ($ident: ident) => {
        impl<'ast> SomeItem<'ast> for $ident {
            fn ident(&'ast self) -> Option<&'ast Ident> {
                Some(&self.ident)
            }
        }
    };
}
some_item_type_or_const!(ItemConst);
some_item_type_or_const!(ImplItemConst);
some_item_type_or_const!(TraitItemConst);
some_item_type_or_const!(ItemType);
some_item_type_or_const!(ImplItemType);
some_item_type_or_const!(TraitItemType);

pub trait SomeFn<'ast>: Debug + SomeItem<'ast> {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        None
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>);
}
impl<'ast> SomeFn<'ast> for ItemFn {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        Some(&self.vis)
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_item_fn(self)
    }
}
impl<'ast> SomeFn<'ast> for ImplItemMethod {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        Some(&self.vis)
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_impl_item_method(self)
    }
}
impl<'ast> SomeFn<'ast> for TraitItemMethod {
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_trait_item_method(self)
    }
}

pub trait SomeConst<'ast>: Debug + SomeItem<'ast> {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        None
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>);
}
impl<'ast> SomeConst<'ast> for ItemConst {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        Some(&self.vis)
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_item_const(self)
    }
}
impl<'ast> SomeConst<'ast> for ImplItemConst {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        Some(&self.vis)
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_impl_item_const(self)
    }
}
impl<'ast> SomeConst<'ast> for TraitItemConst {
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_trait_item_const(self)
    }
}

pub trait SomeType<'ast>: Debug + SomeItem<'ast> {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        None
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>);
}
impl<'ast> SomeType<'ast> for ItemType {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        Some(&self.vis)
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_item_type(self)
    }
}
impl<'ast> SomeType<'ast> for ImplItemType {
    fn visibility(&'ast self) -> Option<&'ast Visibility> {
        Some(&self.vis)
    }
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_impl_item_type(self)
    }
}
impl<'ast> SomeType<'ast> for TraitItemType {
    fn visit(&'ast self, v: &mut dyn Visit<'ast>) {
        v.visit_trait_item_type(self)
    }
}
