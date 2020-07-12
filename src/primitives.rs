
use num_traits::WrappingAdd;
use num_traits::AsPrimitive;
use core::cmp::{Eq, Ord, PartialEq};
use core::ops::{Add, BitAnd, BitOr, BitXor, Div, Mul, Neg, Rem, Shl, Shr, Sub};
use num_traits::Bounded;
use num_traits::{Num, One, Zero};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Edge {
    Rising,
    Falling,
    /// Gives the current logic level
    None(bool),
}

impl Edge {
    pub fn rising(&self) -> bool {
        *self == Edge::Rising
    }

    pub fn falling(&self) -> bool {
        *self == Edge::Falling
    }

    pub fn high(&self) -> bool {
        self.rising() || *self == Edge::None(true)
    }

    pub fn low(&self) -> bool {
        self.falling() || *self == Edge::None(false)
    }

    pub fn cycle() -> impl Iterator<Item = Self> {
        static VALUES: [Edge ;4] = [Edge::None(false), Edge::Rising, Edge::None(true), Edge::Falling];
        VALUES.iter().cloned().cycle()
    }
}

macro_rules! declare_signedness_traits {
    (i, $width: expr, $ceiling_width: expr) => {
        paste::item! {
            impl [<i $width>] {
                pub const MAX: Self = Self(((1 as [<i $ceiling_width>]) << ($width - 1)) - 1);
                pub const MIN: Self = Self(-(1 as [<i $ceiling_width>]) << ($width - 1));
            }

            impl Neg for [<i $width>] {
                type Output = Self;
                fn neg(self) -> Self {
                    Self(-self.0)
                }
            }
        }
    };
    (u, $width: expr, $ceiling_width: expr) => {
        paste::item! {
            impl [<u $width>] {
                pub const MAX: Self = Self(((1 as [<u $ceiling_width>]) << $width) - 1);
                pub const MIN: Self = Self(0);
            }
        }
    };
}

macro_rules! declare_numeric_type {
    ($set: ident, $width: expr, $ceiling_width: expr) => {
        paste::item! {
            #[derive(Debug, Clone, Copy)]
            pub struct [<$set $width>]([<$set $ceiling_width>]);
            impl From<[<$set $ceiling_width>]> for [<$set $width>] {
                fn from(other: [<$set $ceiling_width>]) -> Self {
                    Self(other)
                }
            }

            impl Add<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn add (self, other: [<$set $width>]) -> Self {
                    Self(self.0 + other.0)
                }
            }

            impl Sub<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn sub (self, other: [<$set $width>]) -> Self {
                    Self(self.0 - other.0)
                }
            }

            impl Mul<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn mul (self, other: [<$set $width>]) -> Self {
                    Self(self.0 * other.0)
                }
            }

            impl Div<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn div (self, other: [<$set $width>]) -> Self {
                    Self(self.0 / other.0)
                }
            }

            impl BitXor<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn bitxor (self, other: [<$set $width>]) -> Self {
                    Self(self.0 ^ other.0)
                }
            }

            impl BitOr<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn bitor (self, other: [<$set $width>]) -> Self {
                    Self(self.0 | other.0)
                }
            }

            impl BitAnd<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn bitand (self, other: [<$set $width>]) -> Self {
                    Self(self.0 & other.0)
                }
            }

            impl Shl<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn shl (self, other: [<$set $width>]) -> Self {
                    Self(self.0 << other.0)
                }
            }

            impl Shr<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn shr (self, other: [<$set $width>]) -> Self {
                    Self(self.0 >> other.0)
                }
            }

            impl Rem<[<$set $width>]> for [<$set $width>] {
                type Output = Self;
                fn rem(self, other: [<$set $width>]) -> Self {
                    Self(self.0 % other.0)
                }
            }

            impl PartialEq<Self> for [<$set $width>] {
                fn eq(&self, other: &Self) -> bool {
                    self.0 == other.0
                }
            }

            impl PartialEq<[<$set $ceiling_width>]> for [<$set $width>] {
                fn eq(&self, other: &[<$set $ceiling_width>]) -> bool {
                    self.0 == *other
                }
            }

            impl Eq for [<$set $width>] {}

            impl Zero for [<$set $width>] {
                fn zero() -> Self {
                    Self(0)
                }

                fn is_zero(&self) -> bool {
                    *self == Self::zero()
                }
            }

            impl One for [<$set $width>] {
                fn one() -> Self {
                    Self(1)
                }

                fn is_one(&self) -> bool {
                    *self == Self::one()
                }
            }

            impl Num for [<$set $width>] {
                type FromStrRadixErr = <[<$set $ceiling_width>] as Num>::FromStrRadixErr;

                fn from_str_radix(str: &str, radix: u32) -> Result<Self, Self::FromStrRadixErr> {
                    <[<$set $ceiling_width>] as Num>::from_str_radix(str, radix).map(|x| Self(x))
                }
            }

            impl Bounded for [<$set $width>] {
                fn min_value() -> Self {
                    Self::MIN
                }
    
                fn max_value() -> Self {
                    Self::MAX
                }
            }

            impl Into<f64> for [<$set $width>] {
                fn into(self) -> f64 { self.0 as f64 }
            }

            impl Into<[<$set $ceiling_width>]> for [<$set $width>] {
                fn into(self) -> [<$set $ceiling_width>] { self.0 }
            }

            impl AsPrimitive<[<$set $width>]> for f64 {
                fn as_(self) -> [<$set $width>] {
                    [<$set $width>](self as [<$set $ceiling_width>])
                }
            }

            impl WrappingAdd for [<$set $width>] {
                fn wrapping_add(&self, other: &Self) -> Self { 
                    Self(self.0.wrapping_add(other.0))
                }
            }

        }

        declare_signedness_traits!($set, $width, $ceiling_width);
    };
}

declare_numeric_type!(u, 24, 32);
declare_numeric_type!(i, 24, 32);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_are_correct() {
        assert_eq!(u24::min_value(), 0);
        assert_eq!(u24::max_value(), 16777215);
        assert_eq!(i24::min_value(), -8388608);
        assert_eq!(i24::max_value(), 8388607);
    }
}
