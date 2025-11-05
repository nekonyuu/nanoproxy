mod adapters;
mod domain;
mod ports;

use clap::Parser;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder as ServerBuilder;
use rlimit::{getrlimit, setrlimit, Resource};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::error;
use tracing_subscriber::EnvFilter;

use adapters::{
    BeaconPoller, ConfigurationHolder, ConnectionTracker, CredentialProvider, HyperConnector, HyperHttpClient,
    HyperProxyAdapter, PacProxyResolver, ResolvConfListener,
};
use domain::{AuthRule, PacRule, ProxyService, ResolvConfRule};
use ports::configuration::SystemConfiguration;

#[derive(Debug, Serialize, Deserialize)]
struct ProxyConfig {
    #[serde(default)]
    system: SystemConfiguration,

    #[serde(default)]
    auth_rules: Option<Vec<AuthRule>>,

    #[serde(default)]
    pac_rules: Option<Vec<PacRule>>,

    #[serde(default)]
    resolvconf_rules: Option<Vec<ResolvConfRule>>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            system: SystemConfiguration::default(),
            auth_rules: None,
            pac_rules: None,
            resolvconf_rules: None,
        }
    }
}

#[derive(Parser, Debug)]
#[clap(version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"))]
pub struct Opts {
    #[clap(long, short = 'p', default_value = "8888")]
    port: u16,
    #[clap(long, default_value = "false")]
    no_greeting: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load configuration
    let cfg = confy::load::<ProxyConfig>("nanoproxy", "nanoproxy")?;

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Opts::parse();
    let listen_addr = SocketAddr::from(([127, 0, 0, 1], args.port));

    // Set up resource limits
    let (_, hard_limit) = getrlimit(Resource::NOFILE)?;
    let max_connections = if cfg.system.max_connections < hard_limit {
        cfg.system.max_connections
    } else {
        hard_limit
    };
    setrlimit(Resource::NOFILE, max_connections, hard_limit)?;

    // Create configuration holder
    let config_holder = ConfigurationHolder::new(
        cfg.auth_rules.unwrap_or_default(),
        cfg.pac_rules.unwrap_or_default(),
        cfg.resolvconf_rules.unwrap_or_default(),
        cfg.system.clone(),
    );

    // Create ports (dependency injection)
    let resolver: Arc<dyn ports::ProxyResolverPort> = Arc::new(PacProxyResolver::new());

    let credentials: Arc<dyn ports::CredentialsPort> = Arc::new(CredentialProvider::new(config_holder.clone()));

    let tracker = Arc::new(ConnectionTracker::new());
    let tracker_port: Arc<dyn ports::TrackingPort> = tracker.clone();

    // Start background tasks
    tracker.start_cleanup();

    // Start beacon poller if pac_rules configured
    let pac_rules = config_holder.get_pac_rules().await;
    if !pac_rules.is_empty() {
        let poller = BeaconPoller::new(config_holder.clone(), resolver.clone());
        poller.start();
    }

    // Start resolvconf listener if resolvconf_rules configured
    let resolvconf_rules = config_holder.get_resolvconf_rules().await;
    if !resolvconf_rules.is_empty() {
        let listener = ResolvConfListener::new(config_holder.clone(), resolver.clone());
        listener.start()?;
    }

    // Set up SIGHUP handler for configuration reload
    let config_holder_reload = config_holder.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sighup = signal(SignalKind::hangup()).expect("Failed to setup SIGHUP handler");
        loop {
            sighup.recv().await;
            match reload_configuration(&config_holder_reload).await {
                Ok(_) => tracing::info!("Configuration reloaded successfully"),
                Err(e) => tracing::error!("Failed to reload configuration: {}", e),
            }
        }
    });

    // Create Hyper client with connector
    let connector = HyperConnector::new(resolver.clone());
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .http1_title_case_headers(true)
        .http1_preserve_header_case(true)
        .build(connector.clone());

    // Create HTTP client adapter (implements HttpClientPort)
    let http_client = Arc::new(HyperHttpClient::new(connector.clone(), client.clone()));

    // Create domain proxy service
    let proxy_service = Arc::new(ProxyService::new(
        resolver.clone(),
        credentials.clone(),
        tracker_port.clone(),
        http_client,
    ));

    // Create Hyper adapter (for CONNECT tunnels)
    let adapter = Arc::new(HyperProxyAdapter::new(proxy_service, client));

    // Bind listener
    let listener = TcpListener::bind(&listen_addr).await?;

    if !args.no_greeting {
        print_greeting(&listener);
    }

    // Accept connections
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let adapter = adapter.clone();

        tokio::spawn(async move {
            let service_fn = service_fn(move |req| {
                let adapter = adapter.clone();
                async move { Ok::<_, hyper::Error>(adapter.handle(req).await) }
            });

            if let Err(err) = ServerBuilder::new(hyper_util::rt::TokioExecutor::new())
                .http1()
                .preserve_header_case(true)
                .title_case_headers(true)
                .serve_connection_with_upgrades(io, service_fn)
                .await
            {
                error!("Error serving connection: {:?}", err);
            }
        });
    }
}

async fn reload_configuration(config_holder: &Arc<ConfigurationHolder>) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = confy::load::<ProxyConfig>("nanoproxy", "nanoproxy")?;

    config_holder
        .reload(
            cfg.auth_rules.unwrap_or_default(),
            cfg.pac_rules.unwrap_or_default(),
            cfg.resolvconf_rules.unwrap_or_default(),
            cfg.system,
        )
        .await;

    Ok(())
}

fn print_greeting(listener: &TcpListener) {
    let addr = listener.local_addr().unwrap();
    println!(
        "🚀 Nanoproxy server is running on http://{}:{}.",
        addr.ip(),
        addr.port()
    );
    println!(
        "Configuration loaded from {:#?}",
        confy::get_configuration_file_path("nanoproxy", "nanoproxy").expect("failed to load config")
    );
    println!();
    println!("export http_proxy=http://{}:{};", addr.ip(), addr.port());
    println!("export https_proxy=http://{}:{};", addr.ip(), addr.port());
    println!("export all_proxy=http://{}:{};", addr.ip(), addr.port());
    println!("export HTTP_PROXY=http://{}:{};", addr.ip(), addr.port());
    println!("export HTTPS_PROXY=http://{}:{};", addr.ip(), addr.port());
    println!("export ALL_PROXY=http://{}:{};", addr.ip(), addr.port());
    println!("export no_proxy=localhost,127.0.0.0/8,*.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16;");
    println!();
    println!("Connection logs will appear below.");
}
