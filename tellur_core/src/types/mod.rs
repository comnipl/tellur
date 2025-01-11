use crate::util::correspond::CorrespondExt as _;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

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
}

pub type TellurTypedValueContainer = Arc<RwLock<TellurTypedValue>>;

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
            (TellurTypedValue::Struct(fields), TellurType::Struct(t)) => {
                fields.iter().correspond(t.iter(), |(k1, v1), (k2, v2)| {
                    *k1 == *k2 && v1.is_instance_of(v2)
                })
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    #[rstest]
    #[test]
    #[case::usize(TellurTypedValue::USize(1), TellurType::USize)]
    #[case::i64(TellurTypedValue::I64(1), TellurType::I64)]
    #[case::f64(TellurTypedValue::F64(1.0), TellurType::F64)]
    #[case::string(TellurTypedValue::String("foo".to_string()), TellurType::String)]
    #[case::bool(TellurTypedValue::Bool(true), TellurType::Bool)]
    #[case::array(TellurTypedValue::Array(vec![TellurTypedValue::USize(1), TellurTypedValue::USize(2)]), TellurType::Array(Box::new(TellurType::USize)))]
    #[case::struct_fields(TellurTypedValue::Struct(
        vec![
            ("foo".to_string(), Box::new(TellurTypedValue::USize(1))),
            ("bar".to_string(), Box::new(TellurTypedValue::USize(2)))
        ]
        .into_iter()
        .collect()
    ), TellurType::Struct(
        vec![
            ("foo".to_string(), Box::new(TellurType::USize)),
            ("bar".to_string(), Box::new(TellurType::USize))
        ]
        .into_iter()
        .collect()
    ))]
    #[case::struct_with_array_field(TellurTypedValue::Struct(
        vec![
            ("foo".to_string(), Box::new(TellurTypedValue::Array(vec![TellurTypedValue::USize(1), TellurTypedValue::USize(2)])))
        ]
        .into_iter()
        .collect()
    ), TellurType::Struct(
        vec![
            ("foo".to_string(), Box::new(TellurType::Array(Box::new(TellurType::USize))))
        ]
        .into_iter()
        .collect()
    ))]
    fn to_original_type_primitive_ok(
        #[case] value: TellurTypedValue,
        #[case] expected: TellurType,
    ) {
        assert_eq!(value.to_original_type(), Some(expected));
    }

    #[test]
    fn to_original_type_empty_array_none() {
        assert_eq!(TellurTypedValue::Array(vec![]).to_original_type(), None);
    }

    #[test]
    #[should_panic]
    fn to_original_type_arbitary_array_panic() {
        TellurTypedValue::Array(vec![
            TellurTypedValue::USize(1),
            TellurTypedValue::I64(2),
            TellurTypedValue::USize(3),
        ])
        .to_original_type();
    }

    #[test]
    fn to_original_type_struct_ok() {
        assert_eq!(
            TellurTypedValue::Struct(
                vec![
                    ("foo".to_string(), Box::new(TellurTypedValue::USize(1))),
                    ("bar".to_string(), Box::new(TellurTypedValue::USize(2))),
                    ("baz".to_string(), Box::new(TellurTypedValue::USize(3)))
                ]
                .into_iter()
                .collect()
            )
            .to_original_type(),
            Some(TellurType::Struct(
                vec![
                    ("foo".to_string(), Box::new(TellurType::USize)),
                    ("bar".to_string(), Box::new(TellurType::USize)),
                    ("baz".to_string(), Box::new(TellurType::USize))
                ]
                .into_iter()
                .collect()
            ))
        );
    }

    #[rstest]
    #[test]
    #[case::usize(TellurTypedValue::USize(1), TellurType::USize)]
    #[case::i64(TellurTypedValue::I64(1), TellurType::I64)]
    #[case::f64(TellurTypedValue::F64(1.0), TellurType::F64)]
    #[case::string(TellurTypedValue::String("foo".to_string()), TellurType::String)]
    #[case::bool(TellurTypedValue::Bool(true), TellurType::Bool)]
    #[case::empty_array(TellurTypedValue::Array(vec![]), TellurType::Array(Box::new(TellurType::USize)))]
    #[case::array(TellurTypedValue::Array(vec![TellurTypedValue::USize(1), TellurTypedValue::USize(2)]), TellurType::Array(Box::new(TellurType::USize)))]
    #[case::empty_struct(
        TellurTypedValue::Struct(BTreeMap::new()),
        TellurType::Struct(BTreeMap::new())
    )]
    #[case::struct_fields(TellurTypedValue::Struct(
        vec![
            ("foo".to_string(), Box::new(TellurTypedValue::USize(1))),
            ("bar".to_string(), Box::new(TellurTypedValue::USize(2)))
        ]
        .into_iter()
        .collect()
    ), TellurType::Struct(
        vec![
            ("foo".to_string(), Box::new(TellurType::USize)),
            ("bar".to_string(), Box::new(TellurType::USize))
        ]
        .into_iter()
        .collect()
    ))]
    #[case::struct_fields_unordered(TellurTypedValue::Struct(
        vec![
            ("bar".to_string(), Box::new(TellurTypedValue::USize(2))),
            ("foo".to_string(), Box::new(TellurTypedValue::USize(1)))
        ]
        .into_iter()
        .collect()
    ), TellurType::Struct(
        vec![
            ("foo".to_string(), Box::new(TellurType::USize)),
            ("bar".to_string(), Box::new(TellurType::USize))
        ]
        .into_iter()
        .collect()
    ))]
    fn is_instance_of_primitive_ok(#[case] value: TellurTypedValue, #[case] t: TellurType) {
        assert!(value.is_instance_of(&t));
    }

    #[rstest]
    #[test]
    #[case::not_array(
        TellurTypedValue::Bool(true),
        TellurType::Array(Box::new(TellurType::USize))
    )]
    #[case::struct_fields_lack(
        TellurTypedValue::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurTypedValue::USize(1))),
                ("bar".to_string(), Box::new(TellurTypedValue::USize(2)))
            ]
            .into_iter()
            .collect()
        ),
        TellurType::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurType::USize)),
                ("bar".to_string(), Box::new(TellurType::USize)),
                ("baz".to_string(), Box::new(TellurType::USize))
            ]
            .into_iter()
            .collect()
        )
    )]
    #[case::struct_fields_type_mismatch(
        TellurTypedValue::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurTypedValue::USize(1))),
                ("bar".to_string(), Box::new(TellurTypedValue::I64(2)))
            ]
            .into_iter()
            .collect()
        ),
        TellurType::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurType::USize)),
                ("bar".to_string(), Box::new(TellurType::USize))
            ]
            .into_iter()
            .collect()
        )
    )]
    #[case::struct_fields_name_mismatch(
        TellurTypedValue::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurTypedValue::USize(1))),
                ("bar".to_string(), Box::new(TellurTypedValue::USize(2)))
            ]
            .into_iter()
            .collect()
        ),
        TellurType::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurType::USize)),
                ("baz".to_string(), Box::new(TellurType::USize))
            ]
            .into_iter()
            .collect()
        )
    )]
    #[case::struct_extra_fields(
        TellurTypedValue::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurTypedValue::USize(1))),
                ("bar".to_string(), Box::new(TellurTypedValue::USize(2))),
                ("baz".to_string(), Box::new(TellurTypedValue::USize(3)))
            ]
            .into_iter()
            .collect()
        ),
        TellurType::Struct(
            vec![
                ("foo".to_string(), Box::new(TellurType::USize)),
                ("bar".to_string(), Box::new(TellurType::USize))
            ]
            .into_iter()
            .collect()
        )
    )]
    fn is_instance_of_primitive_fail(#[case] value: TellurTypedValue, #[case] t: TellurType) {
        assert!(!value.is_instance_of(&t));
    }
}
