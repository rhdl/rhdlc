//! Type checker todos:
//! * Verify correctness of angle bracketed arguments
//!     * make sure references map to trait type parameters or associated types
//!     * check super-traits too
//!     * i.e. Iterator<Item = bool>
//! * Return types
//!     * () implied
//! * References to self
//!     * is this a method?
//! * Construction: are all necesary parameters present
//!     * named vs unnamed
//!     * enum variant construction
//! * Match arms
//!     * are all cases covered?
//! * Inferrability
//!     * can rhdl identify concrete types for variables and consts?
//! * casting: are both the src and dest types primitive?
//! * calls: is there a method with the name
//!     * arity: are all parameters present
//! * references and dereferencing
//!     * hint when the user needs to dereference or reference
//! * mutability
//!     * can rhdl assign to a self member?
//!     * are local variables mutable
//!     * can mutable methods be called
//! * tuples
//!     * referencing tuple members
//!     * effectively artificial items
//!  * destructuring
//!     * enums
//!     * tuples
//! * question mark error handling
//! * primitive types
//!     * floats
//!     * uints/ints
//!     * chars/strings
//! * recursive types
//! * do type parameters / impl returns correspond to a single concrete type?

use fxhash::FxHashMap as HashMap;
use syn::Ident;

pub enum Type<'a> {
    /// i.e. u16, f64, i128
    /// bool and others are treated as aliases to u1, etc.
    /// i1
    Primitive(PrimitiveType),
    Concrete(ConcreteType<'a>),
    Generic(Vec<Trait<'a>>),
}

pub struct TypeParam<'a> {
    bounds: Vec<&'a Trait<'a>>,
}

impl<'a> TypeParam<'a> {
    pub fn satisfied_by(&self, r#type: &ConcreteType) -> bool {
        self.bounds
            .iter()
            .all(|bound| bound.is_implemented_for(r#type))
    }
}

pub struct Trait<'a> {
    supers: Vec<&'a Trait<'a>>,
    type_params: Vec<TypeParam<'a>>,
}

impl<'a> Trait<'a> {
    pub fn is_implemented_for(&self, r#type: &ConcreteType) -> bool {
        unimplemented!()
    }
}

/// Types are held as references for type de-duping
pub enum ConcreteType<'a> {
    /// A function can act as a type.
    /// It can be a free-standing function,
    /// a method, or a closure.
    Fn {
        type_params: Vec<TypeParam<'a>>,
        args: Vec<&'a Type<'a>>,
        ret: &'a Type<'a>,
    },
    Struct {
        type_params: Vec<TypeParam<'a>>,
        members: Fields<'a>,
    },
    Enum {
        type_params: Vec<TypeParam<'a>>,
        variants: HashMap<&'a Ident, Fields<'a>>,
    },
    Tuple(Vec<&'a Type<'a>>),
    Array(&'a Type<'a>, usize),
}

pub enum Fields<'a> {
    Named(HashMap<&'a Ident, &'a Type<'a>>),
    Unnamed(Vec<&'a Type<'a>>),
}

/// These are the basic types you would expect to be available and accelerated on hardware.
/// Strings complicate things because of their variable size
pub enum PrimitiveType {
    UnsignedInteger(usize),
    /// 2's complement representation
    SignedInteger(usize),
    /// All IEEE 754 floating point types:
    /// * Half (16-bit)
    /// * Single (32-bit)
    /// * Double (64-bit)
    /// * Quadruple (128-bit)
    /// * Octuple (256-bit)
    FloatingPoint(usize),
}
