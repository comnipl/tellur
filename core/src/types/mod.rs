use std::collections::BTreeMap;
use crate::util::correspond::CorrespondExt as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TellurRefType {
    Immutable,
    Mutable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TellurType {
    USize,
    I64,
    F64,
    String,
    Bool,
    Array(Box<TellurType>),
    Struct(BTreeMap<String, Box<TellurType>>),
    Function(Vec<(TellurRefType, Box<TellurType>)>, Vec<Box<TellurType>>),
}

#[derive(Clone, Debug)]
pub enum TellurTypedValue {
    USize(usize),
    I64(i64),
    F64(f64),
    String(String),
    Bool(bool),
    Array(Vec<TellurTypedValue>),
    Struct(BTreeMap<String, Box<TellurTypedValue>>),
    // TODO: Implement Function TypedValue
}

impl TellurTypedValue {
    pub fn to_original_type(&self) -> Option<TellurType> {
        Some(match self {
            TellurTypedValue::USize(_) => TellurType::USize,
            TellurTypedValue::I64(_) => TellurType::I64,
            TellurTypedValue::F64(_) => TellurType::F64,
            TellurTypedValue::String(_) => TellurType::String,
            TellurTypedValue::Bool(_) => TellurType::Bool,
            TellurTypedValue::Array(values) => {
                let canditate = values.first()?.to_original_type();
                if values
                    .iter()
                    .skip(1)
                    .all(|v| v.to_original_type() == canditate)
                {
                    TellurType::Array(Box::new(canditate?))
                } else {
                    panic!("Array elements have different types.")
                }
            }
            TellurTypedValue::Struct(fields) => {
                let type_map = fields
                    .iter()
                    .map(|(k, v)| Some((k.clone(), Box::new(v.to_original_type()?))))
                    .collect::<Option<BTreeMap<_, _>>>()?;
                TellurType::Struct(type_map)
            }
        })
    }

    pub fn is_instance_of(&self, t: &TellurType) -> bool {
        match (self, t) {
            (TellurTypedValue::USize(_), TellurType::USize) => true,
            (TellurTypedValue::I64(_), TellurType::I64) => true,
            (TellurTypedValue::F64(_), TellurType::F64) => true,
            (TellurTypedValue::String(_), TellurType::String) => true,
            (TellurTypedValue::Bool(_), TellurType::Bool) => true,
            (TellurTypedValue::Array(values), TellurType::Array(t)) => {
                values.iter().all(|v| v.is_instance_of(t))
            }
            (TellurTypedValue::Struct(fields), TellurType::Struct(t)) => fields
                .iter()
                .correspond(
                    t.iter(),
                    |(k1, v1), (k2, v2)| *k1 == *k2 && v1.is_instance_of(v2)
                ),
            _ => false,
        }
    }
}
