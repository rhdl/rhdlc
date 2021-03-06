use fxhash::FxHashMap as HashMap;
use rhdl::{
    ast::{
        Block, File, Generics, Ident, Item, ItemArch, ItemConst, ItemEntity, ItemEnum, ItemFn,
        ItemImpl, ItemMod, ItemStruct, ItemTrait, ItemTraitAlias, ItemType, ItemUse, NamedField,
        UnnamedField, UseTreeGlob, UseTreeName, UseTreeRename, Variant, VariantType, Vis,
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
        let idx = ResolutionIndex(self.inner.len());
        if let ResolutionNode::Root { .. } = &node {
            self.roots.push(idx);
        }
        self.inner.push(node);

        idx
    }

    pub fn add_child(&mut self, parent: ResolutionIndex, child: ResolutionIndex) {
        let name = self[child].name();
        if let Some(children) = self[parent].children_mut() {
            children.entry(name).or_default().push(child)
        }
    }

    pub fn node_indices(&self) -> impl Iterator<Item = ResolutionIndex> {
        (0..self.inner.len()).map(|x| ResolutionIndex(x))
    }

    pub fn file(&self, node: ResolutionIndex) -> FileId {
        let mut next_parent = match &self[node] {
            ResolutionNode::Root { .. } => node,
            ResolutionNode::Leaf { parent, .. } | ResolutionNode::Branch { parent, .. } => *parent,
        };
        loop {
            next_parent = match &self[next_parent] {
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

impl<'ast> std::ops::Index<ResolutionIndex> for ResolutionGraph<'ast> {
    type Output = ResolutionNode<'ast>;
    fn index(&self, index: ResolutionIndex) -> &<Self as std::ops::Index<ResolutionIndex>>::Output {
        &self.inner[index.0]
    }
}

impl<'ast> std::ops::IndexMut<ResolutionIndex> for ResolutionGraph<'ast> {
    fn index_mut(
        &mut self,
        index: ResolutionIndex,
    ) -> &mut <Self as std::ops::Index<ResolutionIndex>>::Output {
        &mut self.inner[index.0]
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolutionIndex(usize);

impl std::fmt::Display for ResolutionIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }
}

impl From<ResolutionIndex> for usize {
    fn from(idx: ResolutionIndex) -> Self {
        idx.0
    }
}

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
        let mut visitor = VisibilityVisitor {
            vis: None,
            block_visited: !matches!(self, ResolutionNode::Branch{branch: Branch::Block(_), ..}),
        };
        self.visit(&mut visitor);
        visitor.vis
    }

    pub fn generics(&self) -> Option<&Generics> {
        let mut visitor = GenericsVisitor {
            generics: None,
            block_visited: !matches!(self, ResolutionNode::Branch{branch: Branch::Block(_), ..}),
        };
        self.visit(&mut visitor);
        visitor.generics
    }

    pub fn is_valid_use_path_segment(&self) -> bool {
        matches!(self,
        ResolutionNode::Branch {
            branch: Branch::Mod { .. },
            ..
        }
        | ResolutionNode::Branch {
            branch: Branch::Enum { .. },
            ..
        }
        | ResolutionNode::Root { .. })
    }

    pub fn is_valid_pub_path_segment(&self) -> bool {
        matches!(self,
        ResolutionNode::Branch {
            branch: Branch::Mod { .. },
            ..
        }
        | ResolutionNode::Root { .. })
    }

    pub fn is_valid_type_path_segment(&self) -> bool {
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
        | ResolutionNode::Branch {
            branch: Branch::Arch { .. },
            ..
        }
        | ResolutionNode::Branch {
            branch: Branch::Block { .. },
            ..
        }
        | ResolutionNode::Leaf {
            leaf: Leaf::Const { .. },
            ..
        }
        | ResolutionNode::Leaf {
            leaf: Leaf::TraitAlias(_),
            ..
        }
        | ResolutionNode::Leaf {
            leaf: Leaf::Entity(_),
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
                | ResolutionNode::Leaf {
                    leaf: TraitAlias(_),
                    ..
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
                | ResolutionNode::Leaf {
                    leaf: TraitAlias(_),
                    ..
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
            | ResolutionNode::Branch {
                branch: Branch::Block(_),
                ..
            }
            | ResolutionNode::Leaf {
                leaf: TraitAlias(_),
                ..
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

    pub fn is_trait_or_impl_or_arch(&self) -> bool {
        self.is_trait()
            || self.is_impl()
            || matches!(self, ResolutionNode::Branch{branch: Branch::Arch(_), ..})
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
                Branch::Block(_) => None,
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::NamedField(f) => Some(&f.ident),
                Leaf::Const(c) => Some(&c.ident),
                Leaf::TraitAlias(t) => Some(&t.ident),
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
                Branch::Block(b) => v.visit_block(b),
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::NamedField(f) => v.visit_named_field(f),
                Leaf::UnnamedField(f) => v.visit_unnamed_field(f),
                Leaf::Const(c) => v.visit_item_const(c),
                Leaf::TraitAlias(t) => v.visit_item_trait_alias(t),
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
                Branch::Variant(..) => Some(ItemHint::Variant),
                Branch::Use(..) => None,
                Branch::Block(..) => None,
                Branch::Arch(..) => Some(ItemHint::Item),
            },
            ResolutionNode::Leaf { leaf, .. } => match leaf {
                Leaf::NamedField(..) => Some(ItemHint::Field),
                Leaf::UnnamedField(..) => Some(ItemHint::Field),
                Leaf::Const(..) => Some(ItemHint::Var),
                Leaf::TraitAlias(..) => Some(ItemHint::Trait),
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
    Block(&'ast Block),
}

#[derive(Debug)]
pub enum Leaf<'ast> {
    UseName(&'ast UseTreeName, Vec<ResolutionIndex>),
    UseRename(&'ast UseTreeRename, Vec<ResolutionIndex>),
    UseGlob(&'ast UseTreeGlob, ResolutionIndex),
    Const(&'ast ItemConst),
    Type(&'ast ItemType),
    TraitAlias(&'ast ItemTraitAlias),
    NamedField(&'ast NamedField),
    UnnamedField(&'ast UnnamedField),
    Entity(&'ast ItemEntity),
}

macro_rules! node_only_visitor {
    ($name: ident { $($member:ident : $ty: ty),* }, $visitor_override: item) => {
        struct $name<'ast> {
            $(
                $member: $ty
            ),*,
            block_visited: bool,
        }

        impl<'ast> Visit<'ast> for $name<'ast> {
            fn visit_file(&mut self, _file: &'ast File) {
                // purposefully do nothing so we don't recurse out of this scope
            }

            fn visit_item_mod(&mut self, item_mod: &'ast ItemMod) {
                if let Some(vis) = &item_mod.vis {
                    self.visit_vis(vis);
                }
                self.visit_ident(&item_mod.ident);
                // purposefully do nothing else so we don't recurse out of this scope
            }

            fn visit_item(&mut self, _item: &'ast Item) {
                // purposefully do nothing so we don't recurse out of this scope
            }

            fn visit_item_struct(&mut self, item_struct: &'ast ItemStruct) {
                if let Some(vis) = &item_struct.vis {
                    self.visit_vis(vis);
                }
                self.visit_ident(&item_struct.ident);
                if let Some(generics) = &item_struct.generics {
                    self.visit_generics(generics);
                }
                // purposefully do nothing else so we don't recurse out of this scope
            }

            fn visit_item_enum(&mut self, item_enum: &'ast ItemEnum) {
                if let Some(vis) = &item_enum.vis {
                    self.visit_vis(vis);
                }
                self.visit_ident(&item_enum.ident);
                if let Some(generics) = &item_enum.generics {
                    self.visit_generics(generics);
                }
                // purposefully do nothing else so we don't recurse out of this scope
            }

            fn visit_variant(&mut self, variant: &'ast Variant) {
                self.visit_ident(&variant.ident);
                match &variant.variant_type {
                    VariantType::Unit(u) => self.visit_variant_type_unit(u),
                    VariantType::Discrim(d) => self.visit_variant_type_discrim(d),
                    VariantType::Fields(_) => {}
                }
                // purposefully do nothing else so we don't recurse out of this scope
            }

            fn visit_item_impl(&mut self, item_impl: &'ast ItemImpl) {
                if let Some(generics) = &item_impl.generics {
                    self.visit_generics(generics);
                }
                if let Some((of_ty, _for)) = &item_impl.of {
                    self.visit_type_path(of_ty)
                }
                self.visit_type(&item_impl.ty);
                // purposefully do nothing else so we don't recurse out of this scope
            }

            fn visit_item_arch(&mut self, item_arch: &'ast ItemArch) {
                if let Some(generics) = &item_arch.generics {
                    self.visit_generics(generics);
                }
                self.visit_type_path(&item_arch.entity);
                // purposefully do nothing else so we don't recurse out of this scope
            }

            fn visit_block(&mut self, block: &'ast Block) {
                if !self.block_visited {
                    self.block_visited = true;
                    block.statements.iter().for_each(|stmt| self.visit_stmt(stmt));
                }
            }

            fn visit_item_trait(&mut self, item_trait: &'ast ItemTrait) {
                if let Some(vis) = &item_trait.vis {
                    self.visit_vis(vis);
                }
                if let Some(generics) = &item_trait.generics {
                    self.visit_generics(generics);
                }
                if let Some((_, super_traits)) = &item_trait.super_traits {
                    for super_trait in super_traits.iter() {
                        self.visit_type_path(super_trait);
                    }
                }
                // purposefully do nothing else so we don't recurse out of this scope
            }

            $visitor_override
        }
    };
}

node_only_visitor! {
    GenericsVisitor { generics: Option<&'ast Generics> },
    fn visit_generics(&mut self, generics: &'ast Generics) {
        self.generics = self.generics.or(Some(generics));
    }
}

node_only_visitor! {
    VisibilityVisitor { vis: Option<&'ast Vis> },
    fn visit_vis(&mut self, vis: &'ast Vis) {
        self.vis = self.vis.or(Some(vis));
    }
}
