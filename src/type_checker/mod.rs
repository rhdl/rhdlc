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
