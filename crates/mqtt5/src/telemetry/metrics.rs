use crate::broker::sys_topics::BrokerStats;
use opentelemetry::metrics::Meter;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct MetricsBridge;

impl MetricsBridge {
    pub fn register(meter: &Meter, stats: Arc<BrokerStats>) {
        let s = Arc::clone(&stats);
        let _clients_connected = meter
            .u64_observable_gauge("mqtt.broker.clients.connected")
            .with_callback(move |observer| {
                observer.observe(s.clients_connected.load(Ordering::Relaxed) as u64, &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _clients_total = meter
            .u64_observable_counter("mqtt.broker.clients.total")
            .with_callback(move |observer| {
                observer.observe(s.clients_total.load(Ordering::Relaxed), &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _clients_maximum = meter
            .u64_observable_gauge("mqtt.broker.clients.maximum")
            .with_callback(move |observer| {
                observer.observe(s.clients_maximum.load(Ordering::Relaxed) as u64, &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _messages_sent = meter
            .u64_observable_counter("mqtt.broker.messages.sent")
            .with_callback(move |observer| {
                observer.observe(s.messages_sent.load(Ordering::Relaxed), &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _messages_received = meter
            .u64_observable_counter("mqtt.broker.messages.received")
            .with_callback(move |observer| {
                observer.observe(s.messages_received.load(Ordering::Relaxed), &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _publish_sent = meter
            .u64_observable_counter("mqtt.broker.publish.sent")
            .with_callback(move |observer| {
                observer.observe(s.publish_sent.load(Ordering::Relaxed), &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _publish_received = meter
            .u64_observable_counter("mqtt.broker.publish.received")
            .with_callback(move |observer| {
                observer.observe(s.publish_received.load(Ordering::Relaxed), &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _bytes_sent = meter
            .u64_observable_counter("mqtt.broker.bytes.sent")
            .with_callback(move |observer| {
                observer.observe(s.bytes_sent.load(Ordering::Relaxed), &[]);
            })
            .build();

        let s = Arc::clone(&stats);
        let _bytes_received = meter
            .u64_observable_counter("mqtt.broker.bytes.received")
            .with_callback(move |observer| {
                observer.observe(s.bytes_received.load(Ordering::Relaxed), &[]);
            })
            .build();

        let _uptime = meter
            .u64_observable_gauge("mqtt.broker.uptime")
            .with_callback(move |observer| {
                observer.observe(stats.uptime_seconds(), &[]);
            })
            .build();
    }
}
