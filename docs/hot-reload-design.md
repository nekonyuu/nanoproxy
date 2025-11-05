# Hot Reload Configuration Design

## Overview

This document proposes a design for hot reloading configuration in nanoproxy, allowing the proxy to pick up configuration changes (particularly `pac_url` rules) without requiring a restart.

## Current State Analysis

### Configuration Loading
- Configuration is loaded once at startup via `confy::load()` from `~/.config/nanoproxy/nanoproxy.toml`
- Configuration includes:
  - `system`: SystemConfiguration (max_connections)
  - `auth_rules`: Vec<AuthRule> (authentication rules)
  - `pac_rules`: Vec<PacRule> (beacon-based PAC URL switching)
  - `resolvconf_rules`: Vec<ResolvConfRule> (resolver-based PAC URL switching)

### Current Dynamic Behavior
While the configuration file is static after load, PAC URLs can change dynamically at runtime via:
1. **BeaconPoller**: Monitors beacon hosts every 3s, updates PAC URL based on reachability
2. **ResolvConfListener**: Watches `/etc/resolv.conf`, updates PAC URL based on DNS resolver subnet matching

However, the underlying **rules** themselves (which beacon hosts to check, which subnets to match) cannot be changed without restart.

### Architecture
Follows hexagonal architecture:
- **Domain**: Core business logic and models
- **Ports**: Trait definitions for adapters
- **Adapters**: Implementations (PAC resolver, beacon poller, resolvconf listener, credentials)

## Design Goals

1. **Hot reload configuration file** without restart
2. **Preserve hexagonal architecture** principles
3. **Minimize new dependencies** (follow CLAUDE.md guidelines)
4. **Thread-safe updates** to shared configuration
5. **Graceful error handling** (invalid config should not crash proxy)
6. **Atomic updates** (all-or-nothing configuration changes)

## Proposed Design

### Option 1: Signal-Based Reload (Recommended)

**Pros:**
- No new dependencies required
- Standard Unix daemon pattern (SIGHUP)
- Simple implementation
- Explicit user control
- Works on all platforms with signal support

**Cons:**
- Requires manual trigger (not automatic)
- Platform-specific (signals are Unix-like only)

**Implementation:**
```rust
// In main.rs
use tokio::signal::unix::{signal, SignalKind};

// After starting all background tasks
tokio::spawn(async move {
    let mut sighup = signal(SignalKind::hangup()).unwrap();
    loop {
        sighup.recv().await;
        if let Err(e) = reload_configuration(&config_holder).await {
            error!("Failed to reload configuration: {}", e);
        } else {
            info!("Configuration reloaded successfully");
        }
    }
});
```

### Option 2: File Watcher (Alternative)

**Pros:**
- Automatic reload on file changes
- No manual intervention needed
- Works across platforms

**Cons:**
- Requires new dependency (`notify` crate)
- Can trigger multiple times on single edit (text editors write files in chunks)
- Potentially noisy (reloads on every save, even if invalid)

**Implementation:**
```rust
// New adapter: src/adapters/config_watcher.rs
use notify::{Watcher, RecursiveMode, watcher};

pub struct ConfigWatcher {
    config_path: PathBuf,
    config_holder: Arc<RwLock<ConfigHolder>>,
}

impl ConfigWatcher {
    pub fn start(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let (tx, rx) = channel();
            let mut watcher = watcher(tx, Duration::from_secs(2)).unwrap();
            watcher.watch(&self.config_path, RecursiveMode::NonRecursive).unwrap();

            loop {
                match rx.recv() {
                    Ok(event) => {
                        if let Err(e) = reload_configuration(&self.config_holder).await {
                            error!("Config reload failed: {}", e);
                        }
                    }
                    Err(e) => break,
                }
            }
        })
    }
}
```

### Option 3: Hybrid Approach (Most Flexible)

Support **both** signal-based and optional file watching, controlled by a config flag.

## Architecture Changes

### 1. Shared Configuration State

Make all configuration shareable and mutable:

```rust
// New struct in main.rs or new file src/domain/config.rs
pub struct ConfigHolder {
    pub auth_rules: Arc<RwLock<Vec<AuthRule>>>,
    pub pac_rules: Arc<RwLock<Vec<PacRule>>>,
    pub resolvconf_rules: Arc<RwLock<Vec<ResolvConfRule>>>,
    pub system: Arc<RwLock<SystemConfiguration>>,
}

impl ConfigHolder {
    pub fn new(config: ProxyConfig) -> Self {
        Self {
            auth_rules: Arc::new(RwLock::new(config.auth_rules.unwrap_or_default())),
            pac_rules: Arc::new(RwLock::new(config.pac_rules.unwrap_or_default())),
            resolvconf_rules: Arc::new(RwLock::new(config.resolvconf_rules.unwrap_or_default())),
            system: Arc::new(RwLock::new(config.system)),
        }
    }

    pub async fn reload_from(&self, new_config: ProxyConfig) -> Result<()> {
        // Atomic update: acquire all locks, then swap
        let mut auth = self.auth_rules.write().await;
        let mut pac = self.pac_rules.write().await;
        let mut resolvconf = self.resolvconf_rules.write().await;
        let mut system = self.system.write().await;

        *auth = new_config.auth_rules.unwrap_or_default();
        *pac = new_config.pac_rules.unwrap_or_default();
        *resolvconf = new_config.resolvconf_rules.unwrap_or_default();
        *system = new_config.system;

        Ok(())
    }
}
```

### 2. Update Component Constructors

**BeaconPoller** (`src/adapters/pac_resolver/beacon.rs`):
```rust
pub struct BeaconPoller {
    rules: Arc<RwLock<Vec<PacRule>>>,  // Changed from Vec<PacRule>
    resolver: Arc<PacProxyResolver>,
}

impl BeaconPoller {
    pub fn new(rules: Arc<RwLock<Vec<PacRule>>>, resolver: Arc<PacProxyResolver>) -> Self {
        Self { rules, resolver }
    }

    async fn select_pac_url(&self) -> Option<String> {
        let rules = self.rules.read().await;  // Read from shared state
        for rule in rules.iter() {
            if rule.beacon_host.to_socket_addrs().is_ok() {
                return Some(rule.beacon_url.clone());
            }
        }
        None
    }
}
```

**ResolvConfListener** (`src/adapters/pac_resolver/resolvconf.rs`):
```rust
pub struct ResolvConfListener {
    rules: Arc<RwLock<Vec<ResolvConfRule>>>,  // Changed
    resolver: Arc<PacProxyResolver>,
}

// Similar changes to read from shared rules
```

**CredentialProvider** (`src/adapters/credentials/provider.rs`):
```rust
pub struct CredentialProvider {
    rules: Arc<RwLock<Vec<AuthRule>>>,  // Changed
}
```

### 3. Reload Function

```rust
// In main.rs
async fn reload_configuration(config_holder: &Arc<ConfigHolder>) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Load new configuration from file
    let new_config = confy::load::<ProxyConfig>("nanoproxy", "nanoproxy")?;

    // 2. Validate configuration (optional: add validation logic)
    validate_config(&new_config)?;

    // 3. Atomically update all shared state
    config_holder.reload_from(new_config).await?;

    info!("Configuration reloaded successfully");
    Ok(())
}

fn validate_config(config: &ProxyConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Validate pac_urls are valid URLs
    if let Some(pac_rules) = &config.pac_rules {
        for rule in pac_rules {
            url::Url::parse(&rule.pac_url)?;
        }
    }

    if let Some(resolvconf_rules) = &config.resolvconf_rules {
        for rule in resolvconf_rules {
            url::Url::parse(&rule.pac_url)?;
        }
    }

    // Validate max_connections is reasonable
    if config.system.max_connections == 0 {
        return Err("max_connections cannot be 0".into());
    }

    Ok(())
}
```

### 4. Main Function Changes

```rust
// In main.rs main()
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Load initial configuration
    let config = confy::load::<ProxyConfig>("nanoproxy", "nanoproxy")?;

    // 2. Create shared config holder
    let config_holder = Arc::new(ConfigHolder::new(config));

    // 3. Create components with shared references
    let resolver = Arc::new(PacProxyResolver::new());
    let credentials = Arc::new(CredentialProvider::new(config_holder.auth_rules.clone()));
    let tracker = Arc::new(ConnectionTracker::new(config_holder.system.clone()));

    // 4. Start background tasks
    tracker.start_cleanup();

    let beacon_poller = BeaconPoller::new(config_holder.pac_rules.clone(), resolver.clone());
    beacon_poller.start();

    let resolvconf_listener = ResolvConfListener::new(
        config_holder.resolvconf_rules.clone(),
        resolver.clone()
    );
    resolvconf_listener.start();

    // 5. Start reload listener (SIGHUP)
    let config_holder_clone = config_holder.clone();
    tokio::spawn(async move {
        let mut sighup = signal(SignalKind::hangup()).unwrap();
        loop {
            sighup.recv().await;
            if let Err(e) = reload_configuration(&config_holder_clone).await {
                error!("Failed to reload configuration: {}", e);
            }
        }
    });

    // 6. Continue with rest of main() setup...
}
```

## User Experience

### Signal-Based Reload
```bash
# Edit configuration
vim ~/.config/nanoproxy/nanoproxy.toml

# Reload configuration
kill -HUP $(pgrep nanoproxy)

# Or with systemd
systemctl reload nanoproxy
```

### File Watcher (if implemented)
```bash
# Edit configuration - automatically reloads
vim ~/.config/nanoproxy/nanoproxy.toml
# (Save triggers reload automatically)
```

## Implementation Phases

### Phase 1: Core Infrastructure (Minimal)
1. Create `ConfigHolder` struct with Arc<RwLock<>> for all config sections
2. Update component constructors to accept Arc<RwLock<>> references
3. Modify components to read from shared state
4. Implement `reload_configuration()` function
5. Add SIGHUP handler in main()

**Estimated effort:** 2-3 hours
**New dependencies:** None

### Phase 2: Validation (Optional)
1. Add config validation before applying
2. Add error handling and logging
3. Add metrics/tracing for reload events

**Estimated effort:** 1-2 hours
**New dependencies:** None

### Phase 3: File Watcher (Optional)
1. Add `notify` crate dependency
2. Create `ConfigWatcher` adapter
3. Add configuration flag to enable/disable file watching
4. Handle debouncing for multiple file events

**Estimated effort:** 2-3 hours
**New dependencies:** `notify = "6.0"`

## Testing Strategy

### Unit Tests
```rust
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_config_reload() {
        let config = ProxyConfig::default();
        let holder = ConfigHolder::new(config);

        // Modify config
        let new_config = ProxyConfig {
            pac_rules: Some(vec![PacRule {
                beacon_host: "example.com:80".to_string(),
                pac_url: "http://pac.example.com/proxy.pac".to_string(),
            }]),
            ..Default::default()
        };

        holder.reload_from(new_config).await.unwrap();

        let pac_rules = holder.pac_rules.read().await;
        assert_eq!(pac_rules.len(), 1);
    }
}
```

### Integration Tests
1. Start proxy with initial config
2. Make request, verify behavior
3. Modify config file
4. Send SIGHUP
5. Make request, verify new behavior

### Manual Testing
1. Edit config while proxy is running
2. Send reload signal
3. Verify logs show successful reload
4. Verify new PAC URLs are used

## Rollback & Error Handling

### Invalid Configuration
```rust
async fn reload_configuration(config_holder: &Arc<ConfigHolder>) -> Result<()> {
    match confy::load::<ProxyConfig>("nanoproxy", "nanoproxy") {
        Ok(new_config) => {
            match validate_config(&new_config) {
                Ok(_) => {
                    config_holder.reload_from(new_config).await?;
                    info!("Configuration reloaded successfully");
                    Ok(())
                }
                Err(e) => {
                    error!("Invalid configuration, keeping current config: {}", e);
                    Err(e)
                }
            }
        }
        Err(e) => {
            error!("Failed to load configuration file: {}", e);
            Err(e.into())
        }
    }
}
```

**Behavior:**
- If reload fails, keep current configuration
- Log error details
- Continue running with old config
- User can fix config and retry

## Considerations & Trade-offs

### Performance Impact
- **Minimal**: RwLock reads are cheap when no writes occur
- BeaconPoller and ResolvConfListener already run in background
- One additional read per 3-second beacon poll cycle
- Config reloads are rare events (user-triggered)

### Memory Impact
- **Minimal**: One additional Arc wrapper per config section
- Arc overhead: 16 bytes per reference
- RwLock overhead: ~40 bytes per lock

### Complexity
- **Low to Medium**:
  - Signal-based: Low complexity, standard pattern
  - File watcher: Medium complexity, requires debouncing

### Compatibility
- SIGHUP approach: Unix-like systems only (Linux, macOS, BSD)
- File watcher: Cross-platform (Windows, Linux, macOS)
- Could implement Windows equivalent (service control messages)

## Alternatives Considered

### 1. HTTP API for Config Reload
```rust
// Add endpoint: POST /admin/reload
```
**Pros:** Platform-independent, programmatic access
**Cons:** Requires authentication, exposes attack surface, additional complexity

### 2. Restart Background Tasks
Instead of updating rules in place, stop and restart BeaconPoller/ResolvConfListener with new rules.

**Pros:** Simpler (no shared state needed)
**Cons:** Task churn, potential race conditions, more complex lifecycle management

### 3. No Hot Reload (Status Quo)
Users restart the entire proxy when config changes.

**Pros:** Simplest implementation (no code changes)
**Cons:** Downtime during restart, lost connections, poor UX

## Recommendation

**Implement Phase 1 (Signal-Based Reload) first:**
1. Zero new dependencies
2. Standard Unix pattern
3. Clean architecture
4. Minimal complexity
5. Solves 90% of use cases

**Consider Phase 3 (File Watcher) later** if users request automatic reloading, as it requires a new dependency.

## Example Configuration Update Workflow

```bash
# 1. Check current config
cat ~/.config/nanoproxy/nanoproxy.toml

# 2. Edit config (add/modify pac_rules)
vim ~/.config/nanoproxy/nanoproxy.toml

# Example: Add new beacon rule
[[pac_rules]]
beacon_host = "internal.corp.com:80"
pac_url = "http://proxy.corp.com/proxy.pac"

# 3. Validate syntax (optional)
# Could add: nanoproxy --check-config

# 4. Reload configuration
kill -HUP $(pgrep nanoproxy)

# 5. Verify reload in logs
journalctl -u nanoproxy -f
# Expected: "Configuration reloaded successfully"

# 6. Test new behavior
curl -x localhost:8888 http://example.com
```

## Future Enhancements

1. **Configuration versioning**: Track config version, log version on reload
2. **Partial reloads**: Reload only specific sections (e.g., only pac_rules)
3. **Reload API**: HTTP endpoint for programmatic reload
4. **Metrics**: Prometheus metrics for reload success/failure counts
5. **Config diff logging**: Log what changed on reload
6. **Graceful connection handling**: Wait for active connections before applying certain changes

## Conclusion

This design enables hot reloading of configuration while:
- Preserving hexagonal architecture
- Minimizing dependencies (Phase 1 requires zero new crates)
- Maintaining thread safety via Arc<RwLock<>>
- Providing graceful error handling
- Following Rust and project conventions

The signal-based approach (Phase 1) is recommended as the initial implementation, with file watching available as an optional enhancement.
