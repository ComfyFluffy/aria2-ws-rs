use serde::Serialize;
use serde_json::{to_value, Value};

pub mod serde_option_from_str {
    use std::{fmt::Display, str::FromStr};

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: FromStr,
        <T as FromStr>::Err: Display,
    {
        let v: Option<String> = Option::deserialize(deserializer)?;
        Ok(if let Some(s) = v {
            if s.is_empty() {
                None
            } else {
                Some(T::from_str(&s).map_err(serde::de::Error::custom)?)
            }
        } else {
            None
        })
    }

    pub fn serialize<T, S>(t: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: ToString,
    {
        if let Some(t) = t {
            serializer.serialize_str(&t.to_string())
        } else {
            serializer.serialize_none()
        }
    }

    #[cfg(test)]
    mod tests {
        use serde::{Deserialize, Serialize};
        use serde_json::json;

        #[derive(PartialEq, Serialize, Deserialize, Debug, Default)]
        struct TestOptions {
            #[serde(with = "super", skip_serializing_if = "Option::is_none", default)]
            split: Option<i32>,
        }

        #[test]
        fn deserialize() {
            let v = json!({"split": "3"});
            assert_eq!(
                serde_json::from_value::<TestOptions>(v).unwrap(),
                TestOptions { split: Some(3) }
            );

            let v = json!({});
            assert_eq!(
                serde_json::from_value::<TestOptions>(v).unwrap(),
                TestOptions {
                    ..Default::default()
                }
            );
        }

        #[test]
        fn serialize() {
            let v = json!({"split": "3"});
            assert_eq!(
                v,
                serde_json::to_value(TestOptions { split: Some(3) }).unwrap()
            );

            let v = json!({});
            assert_eq!(
                v,
                serde_json::to_value(TestOptions {
                    ..Default::default()
                })
                .unwrap()
            );
        }
    }
}

pub mod serde_from_str {
    use std::{fmt::Display, str::FromStr};

    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: FromStr,
        <T as FromStr>::Err: Display,
    {
        let s = String::deserialize(deserializer)?;
        T::from_str(&s).map_err(serde::de::Error::custom)
    }

    // pub fn serialize<T, S>(t: &T, serializer: S) -> Result<S::Ok, S::Error>
    // where
    //     S: Serializer,
    //     T: ToString,
    // {
    //     serializer.serialize_str(&t.to_string())
    // }
}

pub trait PushExt {
    fn push_some<T: Serialize>(&mut self, t: Option<T>) -> Result<(), serde_json::Error>;

    fn push_else<T: Serialize>(&mut self, t: Option<T>, v: Value) -> Result<(), serde_json::Error>;
}

impl PushExt for Vec<Value> {
    fn push_some<T: Serialize>(&mut self, t: Option<T>) -> Result<(), serde_json::Error> {
        if let Some(t) = t {
            self.push(to_value(t)?);
        }
        Ok(())
    }

    fn push_else<T: Serialize>(&mut self, t: Option<T>, v: Value) -> Result<(), serde_json::Error> {
        if let Some(t) = t {
            self.push(to_value(t)?);
        } else {
            self.push(v);
        }
        Ok(())
    }
}
