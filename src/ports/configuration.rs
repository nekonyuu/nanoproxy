use crate::domain::models::{AuthRule, PacRule, ResolvConfRule};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfiguration {
    pub max_connections: u64,
}

impl Default for SystemConfiguration {
    fn default() -> Self {
        Self {
            max_connections: 1024,
        }
    }
}

#[async_trait]
pub trait ConfigurationPort: Send + Sync {
    async fn get_auth_rules(&self) -> Vec<AuthRule>;
    async fn get_pac_rules(&self) -> Vec<PacRule>;
    async fn get_resolvconf_rules(&self) -> Vec<ResolvConfRule>;
    async fn get_system_config(&self) -> SystemConfiguration;
}

pub type ConfigurationPortSync = Arc<dyn ConfigurationPort>;
