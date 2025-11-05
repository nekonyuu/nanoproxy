use crate::domain::models::{AuthRule, PacRule, ResolvConfRule};
use crate::ports::configuration::{ConfigurationPort, SystemConfiguration};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ConfigurationHolder {
    auth_rules: Arc<RwLock<Vec<AuthRule>>>,
    pac_rules: Arc<RwLock<Vec<PacRule>>>,
    resolvconf_rules: Arc<RwLock<Vec<ResolvConfRule>>>,
    system: Arc<RwLock<SystemConfiguration>>,
}

impl ConfigurationHolder {
    pub fn new(
        auth_rules: Vec<AuthRule>,
        pac_rules: Vec<PacRule>,
        resolvconf_rules: Vec<ResolvConfRule>,
        system: SystemConfiguration,
    ) -> Arc<Self> {
        Arc::new(Self {
            auth_rules: Arc::new(RwLock::new(auth_rules)),
            pac_rules: Arc::new(RwLock::new(pac_rules)),
            resolvconf_rules: Arc::new(RwLock::new(resolvconf_rules)),
            system: Arc::new(RwLock::new(system)),
        })
    }

    pub async fn reload(
        &self,
        auth_rules: Vec<AuthRule>,
        pac_rules: Vec<PacRule>,
        resolvconf_rules: Vec<ResolvConfRule>,
        system: SystemConfiguration,
    ) {
        let mut auth = self.auth_rules.write().await;
        let mut pac = self.pac_rules.write().await;
        let mut resolvconf = self.resolvconf_rules.write().await;
        let mut sys = self.system.write().await;

        *auth = auth_rules;
        *pac = pac_rules;
        *resolvconf = resolvconf_rules;
        *sys = system;
    }
}

#[async_trait]
impl ConfigurationPort for ConfigurationHolder {
    async fn get_auth_rules(&self) -> Vec<AuthRule> {
        self.auth_rules.read().await.clone()
    }

    async fn get_pac_rules(&self) -> Vec<PacRule> {
        self.pac_rules.read().await.clone()
    }

    async fn get_resolvconf_rules(&self) -> Vec<ResolvConfRule> {
        self.resolvconf_rules.read().await.clone()
    }

    async fn get_system_config(&self) -> SystemConfiguration {
        self.system.read().await.clone()
    }
}
