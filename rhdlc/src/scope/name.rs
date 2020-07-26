use syn::{File, Ident, ImplItem, Item, ItemImpl, ItemMod, ItemUse, Visibility};

use crate::error::{MultipleDefinitionError, ScopeError};
use log::{debug, error, warn};
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Name<'ast> {
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
    pub fn in_same_name_class(&self, other: &Name<'ast>) -> bool {
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
    pub fn has_same_ident(&self, other: &Name<'ast>) -> bool {
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
    pub fn conflicts_with(&self, other: &Name<'ast>) -> bool {
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
