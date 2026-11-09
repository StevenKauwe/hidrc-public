//! Persistent preferences, read and written with the `defaults` tool so
//! cfprefsd stays in sync and System Settings shows the same values.

use std::fmt;
use std::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl Value {
    pub fn from_toml(v: &toml::Value) -> Result<Value, String> {
        match v {
            toml::Value::Boolean(b) => Ok(Value::Bool(*b)),
            toml::Value::Integer(i) => Ok(Value::Int(*i)),
            toml::Value::Float(f) => Ok(Value::Float(*f)),
            toml::Value::String(s) => Ok(Value::Str(s.clone())),
            other => Err(format!("unsupported value {other} (use a bool, int, float or string)")),
        }
    }

    fn type_flag(&self) -> &'static str {
        match self {
            Value::Bool(_) => "-bool",
            Value::Int(_) => "-int",
            Value::Float(_) => "-float",
            Value::Str(_) => "-string",
        }
    }

    /// Compares against the text `defaults read` prints (bools print as 1/0).
    pub(crate) fn matches(&self, raw: &str) -> bool {
        match self {
            Value::Bool(b) => raw == if *b { "1" } else { "0" },
            Value::Int(i) => raw.parse::<i64>() == Ok(*i),
            Value::Float(f) => raw.parse::<f64>().is_ok_and(|c| (c - f).abs() < 1e-6),
            Value::Str(s) => raw == s,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s:?}"),
        }
    }
}

pub struct Pref {
    pub domain: String,
    pub key: String,
    pub current_host: bool,
    pub value: Value,
}

impl Pref {
    pub fn new(domain: &str, key: &str, value: Value) -> Pref {
        Pref { domain: domain.into(), key: key.into(), current_host: false, value }
    }

    pub fn current_host(mut self) -> Pref {
        self.current_host = true;
        self
    }

    pub fn label(&self) -> String {
        let host = if self.current_host { " (currentHost)" } else { "" };
        format!("{} {}{host}", self.domain, self.key)
    }

    pub fn current(&self) -> Option<String> {
        read(self.current_host, &self.domain, &self.key)
    }

    pub fn is_applied(&self) -> bool {
        self.current().is_some_and(|c| self.value.matches(&c))
    }

    pub fn write(&self) -> Result<(), String> {
        let mut cmd = Command::new("defaults");
        if self.current_host {
            cmd.arg("-currentHost");
        }
        let arg = match &self.value {
            Value::Str(s) => s.clone(),
            v => v.to_string(),
        };
        let status = cmd
            .args(["write", &self.domain, &self.key, self.value.type_flag(), &arg])
            .status()
            .map_err(|e| e.to_string())?;
        status.success().then_some(()).ok_or(format!("defaults write {} failed", self.label()))
    }
}

pub fn read(current_host: bool, domain: &str, key: &str) -> Option<String> {
    let mut cmd = Command::new("defaults");
    if current_host {
        cmd.arg("-currentHost");
    }
    let out = cmd.args(["read", domain, key]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
