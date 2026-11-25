pub(crate) mod manager;
pub(crate) mod postgres;
pub(crate) mod remember;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionTier {
    Local,
    Staging,
    Production,
}

impl ConnectionTier {
    /// A convenience guard, not a database security boundary.
    pub(crate) fn allows_execution(self) -> bool {
        self != Self::Production
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Staging => "Staging",
            Self::Production => "Production",
        }
    }
}
