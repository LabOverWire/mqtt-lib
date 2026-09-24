use mqtt5_protocol::constants::variable_byte::MAX_VALUE as MAX_SUBSCRIPTION_IDENTIFIER;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::properties::PropertyId;
use mqtt5_protocol::validation::{
    parse_shared_subscription, validate_subscription_filter, validate_topic_name,
};
use mqtt5_protocol::QoS;

use super::packet::encode_checked;
use super::state::{ClientState, ServerLimits};

pub fn check_publish(state: &ClientState, publish: &PublishPacket) -> Result<(), String> {
    if state.protocol_version == 5 {
        check_v5_publish_properties(state, publish)?;
    } else {
        validate_topic_name(&publish.topic_name).map_err(|e| e.to_string())?;
    }
    check_server_limits(&state.server, publish)
}

pub fn check_server_limits(server: &ServerLimits, publish: &PublishPacket) -> Result<(), String> {
    if publish.qos as u8 > server.maximum_qos as u8 {
        return Err(format!(
            "QoS {} exceeds the server Maximum QoS {}",
            publish.qos as u8, server.maximum_qos as u8
        ));
    }
    if publish.retain && !server.retain_available {
        return Err("The server does not support retained messages".to_string());
    }
    let mut sized = publish.clone();
    if sized.qos != QoS::AtMostOnce {
        sized.packet_id = Some(sized.packet_id.unwrap_or(u16::MAX));
    }
    encode_checked(&Packet::Publish(sized), server.maximum_packet_size)?;
    Ok(())
}

pub fn downgrade_to_server_maximum(server: &ServerLimits, publish: &mut PublishPacket) {
    if publish.qos as u8 > server.maximum_qos as u8 {
        tracing::warn!(
            requested = publish.qos as u8,
            maximum = server.maximum_qos as u8,
            "requested QoS exceeds the server Maximum QoS; publishing at the maximum"
        );
        publish.qos = server.maximum_qos;
    }
}

fn check_v5_publish_properties(state: &ClientState, publish: &PublishPacket) -> Result<(), String> {
    let alias = publish.topic_alias();
    match (publish.topic_name.is_empty(), alias) {
        (true, None) => return Err("A zero-length Topic Name requires a Topic Alias".to_string()),
        (true, Some(_)) => {}
        (false, _) => validate_topic_name(&publish.topic_name).map_err(|e| e.to_string())?,
    }
    if let Some(alias) = alias {
        check_topic_alias(state, &publish.topic_name, alias)?;
    }
    if let Some(response_topic) = response_topic(publish) {
        validate_topic_name(response_topic).map_err(|e| format!("Invalid Response Topic: {e}"))?;
    }
    if publish
        .properties
        .contains(PropertyId::SubscriptionIdentifier)
    {
        return Err("A client PUBLISH must not contain a Subscription Identifier".to_string());
    }
    Ok(())
}

fn response_topic(publish: &PublishPacket) -> Option<&str> {
    match publish.properties.get(PropertyId::ResponseTopic) {
        Some(mqtt5_protocol::protocol::v5::properties::PropertyValue::Utf8String(topic)) => {
            Some(topic.as_str())
        }
        _ => None,
    }
}

fn check_topic_alias(state: &ClientState, topic: &str, alias: u16) -> Result<(), String> {
    if alias == 0 {
        return Err("Topic Alias 0 is not permitted".to_string());
    }
    if alias > state.server.topic_alias_maximum {
        return Err(format!(
            "Topic Alias {alias} exceeds the server Topic Alias Maximum {}",
            state.server.topic_alias_maximum
        ));
    }
    if topic.is_empty() && state.outbound_aliases.get_topic(alias).is_none() {
        return Err(format!(
            "Topic Alias {alias} has no mapping on this connection"
        ));
    }
    Ok(())
}

pub fn check_subscribe(state: &ClientState, packet: &SubscribePacket) -> Result<(), String> {
    let identifiers = packet.properties.subscription_identifiers();
    if let Some(invalid) = identifiers
        .iter()
        .find(|id| !(1..=MAX_SUBSCRIPTION_IDENTIFIER).contains(*id))
    {
        return Err(format!(
            "Subscription Identifier {invalid} is outside 1..={MAX_SUBSCRIPTION_IDENTIFIER}"
        ));
    }
    if !identifiers.is_empty() && !state.server.subscriptions.identifiers {
        return Err("The server does not support Subscription Identifiers".to_string());
    }
    for filter in &packet.filters {
        validate_subscription_filter(&filter.filter).map_err(|e| e.to_string())?;
        let (topic_filter, share_name) = parse_shared_subscription(&filter.filter);
        if share_name.is_some() && !state.server.subscriptions.shared {
            return Err("The server does not support Shared Subscriptions".to_string());
        }
        if share_name.is_some() && filter.options.no_local {
            return Err("No Local must not be set on a Shared Subscription".to_string());
        }
        if topic_filter.contains(['+', '#']) && !state.server.subscriptions.wildcards {
            return Err("The server does not support Wildcard Subscriptions".to_string());
        }
    }
    encode_checked(
        &Packet::Subscribe(packet.clone()),
        state.server.maximum_packet_size,
    )?;
    Ok(())
}

pub fn check_unsubscribe(state: &ClientState, packet: &UnsubscribePacket) -> Result<(), String> {
    for filter in &packet.filters {
        validate_subscription_filter(filter).map_err(|e| e.to_string())?;
    }
    encode_checked(
        &Packet::Unsubscribe(packet.clone()),
        state.server.maximum_packet_size,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn state_for(protocol_version: u8) -> ClientState {
        let mut state = ClientState::new("outbound".to_string());
        state.protocol_version = protocol_version;
        state
    }

    fn publish_with_v5_only_properties(protocol_version: u8, topic: &str) -> PublishPacket {
        let mut publish = PublishPacket::new(topic, b"x".to_vec(), QoS::AtLeastOnce);
        publish.protocol_version = protocol_version;
        publish.properties.set_topic_alias(3);
        publish.properties.set_response_topic("reply/+".to_string());
        publish.properties.set_subscription_identifier(7);
        publish
    }

    #[wasm_bindgen_test]
    fn v311_publish_skips_v5_only_property_checks() {
        let publish = publish_with_v5_only_properties(4, "a/b");
        assert_eq!(check_publish(&state_for(4), &publish), Ok(()));
    }

    #[wasm_bindgen_test]
    fn v311_publish_still_validates_topic_name() {
        let publish = publish_with_v5_only_properties(4, "a/+");
        assert!(check_publish(&state_for(4), &publish).is_err());
        let publish = publish_with_v5_only_properties(4, "");
        assert!(check_publish(&state_for(4), &publish).is_err());
    }

    #[wasm_bindgen_test]
    fn v5_publish_applies_v5_only_property_checks() {
        let publish = publish_with_v5_only_properties(5, "a/b");
        assert!(check_publish(&state_for(5), &publish).is_err());
    }
}
