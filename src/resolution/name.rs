use std::convert::TryFrom;

use log::{debug, error, warn};
use syn::{
    Ident, ImplItem, Item, ItemConst, ItemEnum, ItemFn, ItemMod, ItemStruct, ItemTrait, ItemType,
};

use super::r#use::UseType;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Name<'ast> {
    Fn(&'ast Ident),
    Var(&'ast Ident),
    Macro(&'ast Ident),
    Type(&'ast Ident),
    Mod(&'ast Ident),
    Crate(&'ast Ident),
    UseName(&'ast Ident),
    UseRename(&'ast Ident),
}

impl<'ast> Name<'ast> {
    /// The names in a name class must be unique
    /// * mods can conflict with types, other mods, and crates
    /// * Fns & Vars can conflict with types
    /// * types can conflict with anything
    /// * macros only conflict with macros
    pub fn in_same_name_class(&self, other: &Name<'ast>) -> bool {
        use Name::*;
        match self {
            Mod(_) | Crate(_) => match other {
                Type(_) | Mod(_) | Crate(_) | UseName(_) | UseRename(_) => true,
                _ => false,
            },
            Fn(_) | Var(_) => match other {
                Fn(_) | Var(_) | Type(_) | UseName(_) | UseRename(_) => true,
                _ => false,
            },
            Type(_) => match other {
                Fn(_) | Var(_) | Type(_) | Mod(_) | Crate(_) | UseName(_) | UseRename(_) => true,
                Macro(_) => false,
            },
            UseName(_) | UseRename(_) => match other {
                Fn(_) | Var(_) | Type(_) | Mod(_) | Crate(_) | UseName(_) | UseRename(_) => true,
                Macro(_) => false,
            },
            Macro(_) => match other {
                Macro(_) => true,
                _ => false,
            },
        }
    }

    /// Check ident
    pub fn has_same_ident(&self, other: &Name<'ast>) -> bool {
        use Name::*;
        match self {
            Fn(ident) | Var(ident) | Macro(ident) | Type(ident) | Mod(ident) | Crate(ident)
            | UseName(ident) | UseRename(ident) => match other {
                Fn(other_ident)
                | Var(other_ident)
                | Macro(other_ident)
                | Type(other_ident)
                | Mod(other_ident)
                | Crate(other_ident)
                | UseName(other_ident)
                | UseRename(other_ident) => **ident == other_ident.to_string(),
            },
        }
    }

    /// Two names in the same name class with the same identifier are conflicting
    pub fn conflicts_with(&self, other: &Name<'ast>) -> bool {
        self.in_same_name_class(other) && self.has_same_ident(other)
    }

    pub fn ident(&self) -> &syn::Ident {
        use Name::*;
        match self {
            Fn(ident) | Var(ident) | Macro(ident) | Type(ident) | Mod(ident) | Crate(ident)
            | UseName(ident) | UseRename(ident) => ident,
        }
    }

    pub fn to_string(&self) -> String {
        self.ident().to_string()
    }

    pub fn span(&self) -> proc_macro2::Span {
        self.ident().span()
    }
}

impl<'ast> From<&'ast ItemMod> for Name<'ast> {
    fn from(item_mod: &'ast ItemMod) -> Self {
        Self::Mod(&item_mod.ident)
    }
}

impl<'ast> From<&'ast ItemFn> for Name<'ast> {
    fn from(item_fn: &'ast ItemFn) -> Self {
        Self::Fn(&item_fn.sig.ident)
    }
}

impl<'ast> From<&'ast ItemConst> for Name<'ast> {
    fn from(item_const: &'ast ItemConst) -> Self {
        Self::Var(&item_const.ident)
    }
}

impl<'ast> From<&'ast ItemStruct> for Name<'ast> {
    fn from(item_struct: &'ast ItemStruct) -> Self {
        Self::Type(&item_struct.ident)
    }
}

impl<'ast> From<&'ast ItemType> for Name<'ast> {
    fn from(item_type: &'ast ItemType) -> Self {
        Self::Type(&item_type.ident)
    }
}

impl<'ast> From<&'ast ItemEnum> for Name<'ast> {
    fn from(item_enum: &'ast ItemEnum) -> Self {
        Self::Type(&item_enum.ident)
    }
}

impl<'ast> From<&'ast ItemTrait> for Name<'ast> {
    fn from(item_trait: &'ast ItemTrait) -> Self {
        Self::Type(&item_trait.ident)
    }
}

impl<'ast> TryFrom<&UseType<'ast>> for Name<'ast> {
    type Error = ();
    fn try_from(use_type: &UseType<'ast>) -> Result<Self, Self::Error> {
        use UseType::*;
        match use_type {
            Name { name, .. } => Ok(Self::UseName(&name.ident)),
            Rename { rename, .. } => Ok(Self::UseRename(&rename.rename)),
            _ => Err(()),
        }
    }
}

impl<'ast> TryFrom<&'ast ImplItem> for Name<'ast> {
    type Error = ();
    fn try_from(impl_item: &'ast ImplItem) -> Result<Self, Self::Error> {
        use ImplItem::*;
        match impl_item {
            Const(r#const) => Ok(Self::Var(&r#const.ident)),
            Method(r#fn) => Ok(Self::Fn(&r#fn.sig.ident)),
            Type(r#type) => Ok(Self::Type(&r#type.ident)),
            _ => Err(()),
        }
    }
}

impl<'ast> TryFrom<&'ast Item> for Name<'ast> {
    type Error = ();
    fn try_from(item: &'ast Item) -> Result<Self, Self::Error> {
        use Item::*;
        match item {
            ExternCrate(syn::ItemExternCrate { ident, .. }) => Ok(Self::Crate(ident)),
            Mod(syn::ItemMod { ident, .. }) => Ok(Self::Mod(ident)),
            Verbatim(_) | ForeignMod(_) => {
                warn!("Cannot handle {:?}", item);
                Err(())
            }
            Struct(syn::ItemStruct { ident, .. })
            | Enum(syn::ItemEnum { ident, .. })
            | Trait(syn::ItemTrait { ident, .. })
            | TraitAlias(syn::ItemTraitAlias { ident, .. })
            | Type(syn::ItemType { ident, .. })
            | Union(syn::ItemUnion { ident, .. }) => Ok(Self::Type(ident)),
            Const(syn::ItemConst { ident, .. }) | Static(syn::ItemStatic { ident, .. }) => {
                Ok(Self::Var(ident))
            }
            Fn(syn::ItemFn {
                sig: syn::Signature { ident, .. },
                ..
            }) => Ok(Self::Fn(ident)),
            Macro(syn::ItemMacro {
                ident: Some(ident), ..
            })
            | Macro2(syn::ItemMacro2 { ident, .. }) => Ok(Self::Macro(ident)),
            Impl(_) => {
                debug!("Skipping impl, tie this to struct in next scope stage");
                Err(())
            }
            Use(_) => {
                debug!("Skipping use");
                Err(())
            }
            unknown => {
                // syn is implemented so that any additions to the items in Rust syntax will fall into this arm
                error!("Not handling {:?}", unknown);
                Err(())
            }
        }
    }
}
