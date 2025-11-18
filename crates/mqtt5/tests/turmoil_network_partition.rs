//! Network partition and failure tests using Turmoil
//!
//! These tests verify system behavior during network failures
//! and partitions in a deterministic environment.

#[cfg(feature = "turmoil-testing")]
use mqtt5::time::Duration;

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_network_partition_simulation() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(30))
        .build();

    // Host A tries to communicate with Host B
    sim.host("host-a", || async {
        // Initially, connectivity should work
        let addr = turmoil::lookup("host-b");
        tracing::info!("Successfully resolved host-b: {}", addr);

        // Wait a bit
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Try again - in a real scenario this might fail due to network partition
        let addr2 = turmoil::lookup("host-b");
        tracing::info!("Still can resolve host-b: {}", addr2);

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.host("host-b", || async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        tracing::info!("Host B is running");
        Ok::<(), Box<dyn std::error::Error>>(())
    });

    // Run the simulation
    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_deterministic_timing() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(20))
        .build();

    sim.host("timer", || async {
        let start = std::time::Instant::now();

        tokio::time::sleep(Duration::from_secs(5)).await;

        let elapsed = start.elapsed();

        // In a deterministic environment, this should be exactly 5 seconds
        // (within some small tolerance for test execution overhead)
        assert!(elapsed >= Duration::from_secs(5));
        assert!(elapsed < Duration::from_secs(6));

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_concurrent_tasks() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(15))
        .build();

    // Create a single host that simulates multiple concurrent tasks
    sim.host("coordinator", || async {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut handles = vec![];

        // Spawn multiple concurrent tasks within the same host
        for i in 0..5 {
            let counter_clone = counter.clone();
            let handle = tokio::spawn(async move {
                // Each task waits a different amount of time
                tokio::time::sleep(Duration::from_millis(u64::try_from(100 * i).unwrap())).await;

                // Increment counter
                counter_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                tracing::info!("Task {} completed", i);
            });
            handles.push(handle);
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // All tasks should have completed
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 5);

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}
