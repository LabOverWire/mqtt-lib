pub mod propagation;

#[cfg(feature = "opentelemetry")]
use crate::error::{MqttError, Result};
#[cfg(feature = "opentelemetry")]
use crate::time::Duration;

#[cfg(feature = "opentelemetry")]
use opentelemetry::{trace::TracerProvider as _, KeyValue};
#[cfg(feature = "opentelemetry")]
use opentelemetry_otlp::WithExportConfig;
#[cfg(feature = "opentelemetry")]
use opentelemetry_sdk::{
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
    Resource,
};

#[cfg(feature = "opentelemetry")]
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub otlp_endpoint: String,
    pub service_name: String,
    pub service_version: String,
    pub sampling_ratio: f64,
    pub timeout: Duration,
}

#[cfg(feature = "opentelemetry")]
impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: "http://localhost:4317".to_string(),
            service_name: "mqtt-broker".to_string(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            sampling_ratio: 0.1,
            timeout: Duration::from_secs(10),
        }
    }
}

#[cfg(feature = "opentelemetry")]
impl TelemetryConfig {
    #[must_use]
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = endpoint.into();
        self
    }

    #[must_use]
    pub fn with_sampling_ratio(mut self, ratio: f64) -> Self {
        self.sampling_ratio = ratio.clamp(0.0, 1.0);
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Initializes the OpenTelemetry tracer provider.
///
/// # Errors
/// Returns an error if the OTLP exporter cannot be created.
#[cfg(feature = "opentelemetry")]
pub fn init_tracer(config: &TelemetryConfig) -> Result<SdkTracerProvider> {
    let sampler = if (config.sampling_ratio - 1.0).abs() < f64::EPSILON {
        Sampler::AlwaysOn
    } else if config.sampling_ratio < f64::EPSILON {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(config.sampling_ratio)
    };

    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attributes([KeyValue::new(
            "service.version",
            config.service_version.clone(),
        )])
        .build();

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&config.otlp_endpoint)
        .with_timeout(config.timeout)
        .build()
        .map_err(|e| MqttError::Configuration(format!("Failed to create OTLP exporter: {e}")))?;

    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(sampler)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .build();

    Ok(tracer_provider)
}

/// Initializes the tracing subscriber with OpenTelemetry support.
///
/// # Errors
/// Returns an error if the tracer or subscriber cannot be initialized.
#[cfg(feature = "opentelemetry")]
pub fn init_tracing_subscriber(config: &TelemetryConfig) -> Result<()> {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let tracer_provider = init_tracer(config)?;
    let tracer = tracer_provider.tracer("mqtt5");

    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(telemetry_layer)
        .try_init()
        .map_err(|e| {
            MqttError::Configuration(format!("Failed to initialize tracing subscriber: {e}"))
        })?;

    tracing::info!(
        service_name = %config.service_name,
        otlp_endpoint = %config.otlp_endpoint,
        sampling_ratio = %config.sampling_ratio,
        "OpenTelemetry tracing initialized"
    );

    Ok(())
}

#[cfg(feature = "opentelemetry")]
pub fn shutdown_tracer() {
    tracing::debug!("OpenTelemetry tracer shutdown initiated");
}

#[cfg(all(test, feature = "opentelemetry"))]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_config_default() {
        let config = TelemetryConfig::default();
        assert_eq!(config.otlp_endpoint, "http://localhost:4317");
        assert_eq!(config.service_name, "mqtt-broker");
        assert!((config.sampling_ratio - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_telemetry_config_builder() {
        let config = TelemetryConfig::new("test-service")
            .with_endpoint("http://jaeger:4317")
            .with_sampling_ratio(0.5)
            .with_timeout(Duration::from_secs(5));

        assert_eq!(config.service_name, "test-service");
        assert_eq!(config.otlp_endpoint, "http://jaeger:4317");
        assert!((config.sampling_ratio - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.timeout, Duration::from_secs(5));
    }

    #[test]
    fn test_sampling_ratio_clamping() {
        let config = TelemetryConfig::new("test").with_sampling_ratio(1.5);
        assert!((config.sampling_ratio - 1.0).abs() < f64::EPSILON);

        let config = TelemetryConfig::new("test").with_sampling_ratio(-0.5);
        assert!(config.sampling_ratio.abs() < f64::EPSILON);
    }
}
