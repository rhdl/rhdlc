use std::convert::TryFrom;

use log::{debug, error, warn};
use syn::{Ident, Item, ItemMod};

use super::r#use::UseType;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Name<'ast> {
    Function(&'ast Ident),
    Variable(&'ast Ident),
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
    /// * functions & variables can conflict with types
    /// * types can conflict with anything
    /// * macros only conflict with macros
    pub fn in_same_name_class(&self, other: &Name<'ast>) -> bool {
        use Name::*;
        match self {
            Mod(_) | Crate(_) => match other {
                Type(_) | Mod(_) | Crate(_) | UseName(_) | UseRename(_) => true,
                _ => false,
            },
            Function(_) | Variable(_) => match other {
                Function(_) | Variable(_) | Type(_) | UseName(_) | UseRename(_) => true,
                _ => false,
            },
            Type(_) => match other {
                Function(_) | Variable(_) | Type(_) | Mod(_) | Crate(_) | UseName(_)
                | UseRename(_) => true,
                Macro(_) => false,
            },
            UseName(_) | UseRename(_) => match other {
                Function(_) | Variable(_) | Type(_) | Mod(_) | Crate(_) | UseName(_)
                | UseRename(_) => true,
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
            Function(ident) | Variable(ident) | Macro(ident) | Type(ident) | Mod(ident)
            | Crate(ident) | UseName(ident) | UseRename(ident) => match other {
                Function(other_ident)
                | Variable(other_ident)
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

    pub fn to_string(&self) -> String {
        use Name::*;
        match self {
            Function(ident) | Variable(ident) | Macro(ident) | Type(ident) | Mod(ident)
            | Crate(ident) | UseName(ident) | UseRename(ident) => ident.to_string(),
        }
    }

    pub fn span(&self) -> proc_macro2::Span {
        use Name::*;
        match self {
            Function(ident) | Variable(ident) | Macro(ident) | Type(ident) | Mod(ident)
            | Crate(ident) | UseName(ident) | UseRename(ident) => ident.span(),
        }
    }
}

impl<'ast> From<&'ast ItemMod> for Name<'ast> {
    fn from(item_mod: &'ast ItemMod) -> Self {
        Self::Mod(&item_mod.ident)
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
                Ok(Self::Variable(ident))
            }
            Fn(syn::ItemFn {
                sig: syn::Signature { ident, .. },
                ..
            }) => Ok(Self::Function(ident)),
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
