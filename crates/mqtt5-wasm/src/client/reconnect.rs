use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::protocol::v5::properties::Properties;
use mqtt5_protocol::u128_to_u32_saturating;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use crate::transport::WasmTransportType;

use super::callbacks::{trigger_reconnect_failed_callback, trigger_reconnecting_callback};
use super::connection::establish;
use super::connectivity::{is_browser_online, wait_for_online};
use super::sleep_ms;
use super::state::{ClientState, SessionState, StoredConnectOptions};

pub fn spawn_reconnection_task(state: Rc<RefCell<ClientState>>) {
    spawn_local(async move {
        {
            let mut state_ref = state.borrow_mut();
            state_ref.reconnecting = true;
            state_ref.reconnect_attempt = 0;
        }

        loop {
            let (attempt, delay, should_continue, primary_url, options) = {
                let state_ref = state.borrow();
                let attempt = state_ref.reconnect_attempt;
                let base_delay = state_ref.reconnect_config.calculate_delay(attempt);
                let base_delay_ms = u128_to_u32_saturating(base_delay.as_millis());
                let jitter_range = base_delay_ms / 4;
                let jitter = match jitter_range {
                    0 => 0,
                    range => getrandom::u32().unwrap_or(0) % range,
                };
                let delay_ms = base_delay_ms.saturating_add(jitter);
                let should_continue = state_ref.reconnect_config.should_retry(attempt);
                let url = state_ref.last_url.clone();
                let options = state_ref.last_options.clone();
                (attempt, delay_ms, should_continue, url, options)
            };

            if !should_continue {
                trigger_reconnect_failed_callback(&state, "Max reconnection attempts exceeded");
                state.borrow_mut().reconnecting = false;
                return;
            }

            let (Some(primary_url), Some(options)) = (primary_url, options) else {
                trigger_reconnect_failed_callback(&state, "No stored connection parameters");
                state.borrow_mut().reconnecting = false;
                return;
            };

            if !is_browser_online() {
                trigger_reconnecting_callback(&state, attempt + 1, 0);
                wait_for_online().await;
                if state.borrow().user_initiated_disconnect {
                    state.borrow_mut().reconnecting = false;
                    return;
                }
                state.borrow_mut().reconnect_attempt = 0;
                continue;
            }

            trigger_reconnecting_callback(&state, attempt + 1, delay);

            sleep_ms(delay).await;

            if state.borrow().user_initiated_disconnect {
                state.borrow_mut().reconnecting = false;
                return;
            }

            if try_all_brokers(&state, &primary_url, &options, attempt).await {
                return;
            }
            state.borrow_mut().reconnect_attempt = attempt + 1;
        }
    });
}

async fn try_all_brokers(
    state: &Rc<RefCell<ClientState>>,
    primary_url: &str,
    options: &StoredConnectOptions,
    attempt: u32,
) -> bool {
    let mut all_urls = vec![primary_url.to_string()];
    all_urls.extend(options.backup_urls.clone());

    for (idx, url) in all_urls.iter().enumerate() {
        match attempt_reconnect(state, url, options).await {
            Ok(()) => {
                {
                    let mut state_ref = state.borrow_mut();
                    state_ref.reconnecting = false;
                    state_ref.reconnect_attempt = 0;
                    state_ref.current_broker_index = idx;
                }
                tracing::info!(broker_index = idx, attempts = attempt + 1, "reconnected");
                return true;
            }
            Err(e) => {
                tracing::warn!(broker_index = idx, error = %e, "reconnection attempt failed");
            }
        }
    }

    tracing::warn!(attempt = attempt + 1, "all brokers failed");
    false
}

async fn attempt_reconnect(
    state: &Rc<RefCell<ClientState>>,
    url: &str,
    options: &StoredConnectOptions,
) -> Result<(), String> {
    let transport = WasmTransportType::WebSocket(
        crate::transport::websocket::WasmWebSocketTransport::new(url),
    );
    let (client_id, clean_start) = {
        let state_ref = state.borrow();
        (
            state_ref.client_id.clone(),
            state_ref.session == SessionState::Absent && !options.resume_existing_session,
        )
    };
    let properties = if options.protocol_version == 5 {
        build_properties_from_stored(options)
    } else {
        Properties::default()
    };

    let connect_packet = ConnectPacket {
        protocol_version: options.protocol_version,
        clean_start,
        keep_alive: options.keep_alive,
        client_id,
        username: options.username.clone(),
        password: options.password.clone(),
        will: None,
        properties,
        will_properties: Properties::default(),
    };

    establish(state, transport, connect_packet, options)
        .await
        .map(|_| ())
        .map_err(|failure| failure.to_string())
}

pub fn build_properties_from_stored(options: &StoredConnectOptions) -> Properties {
    let mut properties = Properties::default();

    if let Some(interval) = options.session_expiry_interval {
        properties.set_session_expiry_interval(interval);
    }

    if let Some(max) = options.receive_maximum {
        properties.set_receive_maximum(max);
    }

    if let Some(size) = options.maximum_packet_size {
        properties.set_maximum_packet_size(size);
    }

    if let Some(max) = options.topic_alias_maximum {
        properties.set_topic_alias_maximum(max);
    }

    if let Some(req) = options.request_response_information {
        properties.set_request_response_information(req);
    }

    if let Some(req) = options.request_problem_information {
        properties.set_request_problem_information(req);
    }

    if let Some(method) = &options.authentication_method {
        properties.set_authentication_method(method.clone());
    }

    if let Some(data) = &options.authentication_data {
        properties.set_authentication_data(data.clone().into());
    }

    for (key, value) in &options.user_properties {
        properties.add_user_property(key.clone(), value.clone());
    }

    properties
}
