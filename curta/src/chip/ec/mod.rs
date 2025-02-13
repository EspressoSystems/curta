use super::field::parameters::FieldParameters;

pub mod edwards;
pub mod gadget;
pub mod point;

pub trait EllipticCurveParameters: Send + Sync + Copy + 'static {
    type BaseField: FieldParameters;
}
