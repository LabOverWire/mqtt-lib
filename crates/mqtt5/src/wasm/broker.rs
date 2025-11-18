use crate::broker::auth::{AllowAllAuthProvider, AuthProvider};
use crate::broker::config::BrokerConfig;
use crate::broker::resource_monitor::{ResourceLimits, ResourceMonitor};
use crate::broker::router::MessageRouter;
use crate::broker::storage::{DynamicStorage, MemoryBackend};
use crate::broker::sys_topics::BrokerStats;
use crate::wasm::client_handler::WasmClientHandler;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

#[wasm_bindgen]
pub struct WasmBroker {
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Arc<DynamicStorage>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
}

#[wasm_bindgen]
impl WasmBroker {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmBroker, JsValue> {
        let config = Arc::new(BrokerConfig::default());

        let storage = Arc::new(DynamicStorage::Memory(MemoryBackend::new()));
        let router = Arc::new(MessageRouter::with_storage(Arc::clone(&storage)));

        let auth_provider: Arc<dyn AuthProvider> = Arc::new(AllowAllAuthProvider);

        let stats = Arc::new(BrokerStats::new());

        let limits = ResourceLimits {
            max_connections: config.max_clients,
            ..Default::default()
        };
        let resource_monitor = Arc::new(ResourceMonitor::new(limits));

        Ok(WasmBroker {
            config,
            router,
            auth_provider,
            storage,
            stats,
            resource_monitor,
        })
    }

    pub fn create_client_port(&self) -> Result<MessagePort, JsValue> {
        let channel = web_sys::MessageChannel::new()?;

        let client_port = channel.port1();
        let broker_port = channel.port2();

        WasmClientHandler::new(
            broker_port,
            Arc::clone(&self.config),
            Arc::clone(&self.router),
            Arc::clone(&self.auth_provider),
            Arc::clone(&self.storage),
            Arc::clone(&self.stats),
            Arc::clone(&self.resource_monitor),
        );

        Ok(client_port)
    }
}
