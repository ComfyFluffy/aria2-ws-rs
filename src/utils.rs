use serde::Serialize;
use serde_json::{to_value, Value};
use snafu::ResultExt;

use crate::{error, Error};

pub trait PushExt {
    fn push_some<T: Serialize>(&mut self, t: Option<T>) -> Result<(), Error>;

    fn push_else<T: Serialize>(&mut self, t: Option<T>, v: Value) -> Result<(), Error>;

    fn push_value<T: Serialize>(&mut self, t: T) -> Result<(), Error>;
}

impl PushExt for Vec<Value> {
    fn push_some<T: Serialize>(&mut self, t: Option<T>) -> Result<(), Error> {
        if let Some(t) = t {
            self.push(to_value(t).context(error::JsonSnafu)?);
        }
        Ok(())
    }

    fn push_else<T: Serialize>(&mut self, t: Option<T>, v: Value) -> Result<(), Error> {
        if let Some(t) = t {
            self.push(to_value(t).context(error::JsonSnafu)?);
        } else {
            self.push(v);
        }
        Ok(())
    }

    fn push_value<T: Serialize>(&mut self, t: T) -> Result<(), Error> {
        self.push(to_value(t).context(error::JsonSnafu)?);
        Ok(())
    }
}

/// Convert `Value` into `Vec<Value>`
///
/// # Panics
///
/// Panic if value is not of type `Value::Array`
pub fn value_into_vec(value: Value) -> Vec<Value> {
    if let Value::Array(v) = value {
        return v;
    }
    panic!("value is not Value::Array");
}
