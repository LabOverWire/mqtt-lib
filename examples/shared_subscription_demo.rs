//! Demonstrates shared subscriptions feature
//!
//! Run with: cargo run --example `shared_subscription_demo`

use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::{ConnectOptions, MqttClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use mqtt5::time::Duration;
use tokio::time::sleep;

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Start broker
    let config = BrokerConfig::new()
        .with_bind_address(([127, 0, 0, 1], 1885))
        .with_max_clients(100);

    let mut broker = MqttBroker::with_config(config).await?;

    // Run broker in background
    let broker_handle = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            eprintln!("Broker error: {e}");
        }
    });

    // Give broker time to start
    sleep(Duration::from_millis(500)).await;

    println!("Broker started on 127.0.0.1:1885");

    // Create shared counters to track message distribution
    let worker1_count = Arc::new(AtomicUsize::new(0));
    let worker2_count = Arc::new(AtomicUsize::new(0));
    let worker3_count = Arc::new(AtomicUsize::new(0));

    // Spawn worker 1
    let count1 = worker1_count.clone();
    tokio::spawn(async move {
        let options = ConnectOptions::new("worker-1")
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(30));

        let client = MqttClient::with_options(options);
        client.connect("127.0.0.1:1885").await.unwrap();

        // Subscribe to shared subscription
        client
            .subscribe("$share/workers/tasks/+", move |msg| {
                count1.fetch_add(1, Ordering::Relaxed);
                println!(
                    "Worker 1 received: {} = {}",
                    msg.topic,
                    String::from_utf8_lossy(&msg.payload)
                );
            })
            .await
            .unwrap();

        // Keep running
        sleep(Duration::from_secs(60)).await;
    });

    // Spawn worker 2
    let count2 = worker2_count.clone();
    tokio::spawn(async move {
        let options = ConnectOptions::new("worker-2")
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(30));

        let client = MqttClient::with_options(options);
        client.connect("127.0.0.1:1885").await.unwrap();

        // Subscribe to same shared subscription
        client
            .subscribe("$share/workers/tasks/+", move |msg| {
                count2.fetch_add(1, Ordering::Relaxed);
                println!(
                    "Worker 2 received: {} = {}",
                    msg.topic,
                    String::from_utf8_lossy(&msg.payload)
                );
            })
            .await
            .unwrap();

        // Keep running
        sleep(Duration::from_secs(60)).await;
    });

    // Spawn worker 3
    let count3 = worker3_count.clone();
    tokio::spawn(async move {
        let options = ConnectOptions::new("worker-3")
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(30));

        let client = MqttClient::with_options(options);
        client.connect("127.0.0.1:1885").await.unwrap();

        // Subscribe to same shared subscription
        client
            .subscribe("$share/workers/tasks/+", move |msg| {
                count3.fetch_add(1, Ordering::Relaxed);
                println!(
                    "Worker 3 received: {} = {}",
                    msg.topic,
                    String::from_utf8_lossy(&msg.payload)
                );
            })
            .await
            .unwrap();

        // Keep running
        sleep(Duration::from_secs(60)).await;
    });

    // Wait for workers to connect
    sleep(Duration::from_secs(2)).await;

    // Create publisher
    let pub_options = ConnectOptions::new("publisher")
        .with_clean_start(true)
        .with_keep_alive(Duration::from_secs(30));

    let publisher = MqttClient::with_options(pub_options);
    publisher.connect("127.0.0.1:1885").await?;

    println!("\nPublishing 12 tasks...");

    // Publish tasks that will be distributed among workers
    for i in 0..12 {
        let topic = format!("tasks/job{}", i % 3);
        let payload = format!("Task {} - Process order #{}", i, i * 100);

        publisher.publish(&topic, payload.as_bytes()).await?;
        println!("Published task {i} to {topic}");

        // Small delay to see distribution
        sleep(Duration::from_millis(500)).await;
    }

    // Wait a bit for all messages to be processed
    sleep(Duration::from_secs(2)).await;

    // Print statistics
    println!("\n=== Message Distribution ===");
    println!(
        "Worker 1 processed: {} tasks",
        worker1_count.load(Ordering::Relaxed)
    );
    println!(
        "Worker 2 processed: {} tasks",
        worker2_count.load(Ordering::Relaxed)
    );
    println!(
        "Worker 3 processed: {} tasks",
        worker3_count.load(Ordering::Relaxed)
    );
    println!(
        "Total tasks: {}",
        worker1_count.load(Ordering::Relaxed)
            + worker2_count.load(Ordering::Relaxed)
            + worker3_count.load(Ordering::Relaxed)
    );

    // Demonstrate mixed subscriptions
    println!("\n=== Mixed Subscription Demo ===");

    // Add a regular (non-shared) subscriber
    let mon_options = ConnectOptions::new("monitor")
        .with_clean_start(true)
        .with_keep_alive(Duration::from_secs(30));

    let monitor = MqttClient::with_options(mon_options);
    monitor.connect("127.0.0.1:1885").await?;

    let monitor_count = Arc::new(AtomicUsize::new(0));
    let count_m = monitor_count.clone();

    monitor
        .subscribe("tasks/+", move |msg| {
            count_m.fetch_add(1, Ordering::Relaxed);
            println!(
                "Monitor received: {} = {}",
                msg.topic,
                String::from_utf8_lossy(&msg.payload)
            );
        })
        .await?;

    // Publish more tasks
    println!("\nPublishing 3 more tasks (with monitor active)...");
    for i in 12..15 {
        let topic = format!("tasks/job{}", i % 3);
        let payload = format!("Task {} - Special order #{}", i, i * 100);

        publisher.publish(&topic, payload.as_bytes()).await?;
        sleep(Duration::from_millis(500)).await;
    }

    sleep(Duration::from_secs(2)).await;

    println!("\nWith mixed subscriptions:");
    println!(
        "Monitor received: {} messages (all of them)",
        monitor_count.load(Ordering::Relaxed)
    );
    println!("Workers still share the load among themselves");

    // Disconnect clients
    publisher.disconnect().await?;
    monitor.disconnect().await?;

    println!("\nShared subscription demo completed!");
    println!("Press Ctrl+C to stop the broker");

    // Keep broker running until Ctrl+C
    tokio::signal::ctrl_c().await?;

    // Abort broker task
    broker_handle.abort();

    Ok(())
}
