use crate::error::{MqttError, Result};
use crate::packet::connack::ConnAckPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use crate::session::TopicAliasManager;
use crate::types::PublishOptions;
use crate::validation::{
    parse_shared_subscription, validate_subscription_filter, validate_topic_name,
};

const MAX_SUBSCRIPTION_IDENTIFIER: u32 = 268_435_455;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ServerCapabilities {
    wildcard_subscriptions: bool,
    shared_subscriptions: bool,
    subscription_identifiers: bool,
}

impl Default for ServerCapabilities {
    fn default() -> Self {
        Self {
            wildcard_subscriptions: true,
            shared_subscriptions: true,
            subscription_identifiers: true,
        }
    }
}

impl ServerCapabilities {
    pub(crate) fn from_connack(connack: &ConnAckPacket) -> Self {
        let properties = &connack.properties;
        Self {
            wildcard_subscriptions: is_available(
                properties,
                PropertyId::WildcardSubscriptionAvailable,
            ),
            shared_subscriptions: is_available(properties, PropertyId::SharedSubscriptionAvailable),
            subscription_identifiers: is_available(
                properties,
                PropertyId::SubscriptionIdentifierAvailable,
            ),
        }
    }

    pub(crate) fn check_subscribe(self, packet: &SubscribePacket) -> Result<()> {
        let identifiers = packet.properties.subscription_identifiers();
        if let Some(invalid) = identifiers
            .iter()
            .find(|id| !(1..=MAX_SUBSCRIPTION_IDENTIFIER).contains(*id))
        {
            return Err(MqttError::ProtocolError(format!(
                "Subscription Identifier {invalid} is outside 1..={MAX_SUBSCRIPTION_IDENTIFIER}"
            )));
        }
        if !identifiers.is_empty() && !self.subscription_identifiers {
            return Err(MqttError::SubscriptionIdentifiersNotSupported);
        }
        for filter in &packet.filters {
            validate_subscription_filter(&filter.filter)?;
            let (topic_filter, share_name) = parse_shared_subscription(&filter.filter);
            if share_name.is_some() && !self.shared_subscriptions {
                return Err(MqttError::SharedSubscriptionsNotSupported);
            }
            if topic_filter.contains(['+', '#']) && !self.wildcard_subscriptions {
                return Err(MqttError::WildcardSubscriptionsNotSupported);
            }
        }
        Ok(())
    }
}

fn is_available(properties: &Properties, id: PropertyId) -> bool {
    !matches!(properties.get(id), Some(PropertyValue::Byte(0)))
}

pub(crate) fn check_unsubscribe(packet: &UnsubscribePacket) -> Result<()> {
    packet
        .filters
        .iter()
        .map(String::as_str)
        .try_for_each(validate_subscription_filter)
}

pub(crate) fn check_publish(topic: &str, options: &PublishOptions) -> Result<()> {
    let properties = &options.properties;
    match (topic.is_empty(), properties.topic_alias) {
        (true, None) => {
            return Err(MqttError::InvalidTopicName(
                "zero-length Topic Name requires a Topic Alias".to_string(),
            ));
        }
        (true, Some(_)) => {}
        (false, _) => validate_topic_name(topic)?,
    }
    if properties.topic_alias == Some(0) {
        return Err(MqttError::TopicAliasInvalid(0));
    }
    if let Some(response_topic) = &properties.response_topic {
        validate_topic_name(response_topic)?;
    }
    if !properties.subscription_identifiers.is_empty() {
        return Err(MqttError::ProtocolError(
            "a client PUBLISH must not contain a Subscription Identifier".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn check_topic_alias(
    aliases: &TopicAliasManager,
    topic: &str,
    alias: u16,
) -> Result<()> {
    let known = if topic.is_empty() {
        aliases.get_topic(alias).is_some()
    } else {
        (1..=aliases.topic_alias_maximum()).contains(&alias)
    };
    if known {
        Ok(())
    } else {
        Err(MqttError::TopicAliasInvalid(alias))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::subscribe::{SubscriptionOptions, TopicFilter};
    use crate::protocol::v5::reason_codes::ReasonCode;
    use crate::types::PublishProperties;

    fn subscribe(filter: &str, identifier: Option<u32>) -> SubscribePacket {
        let packet = SubscribePacket {
            packet_id: 0,
            filters: vec![TopicFilter {
                filter: filter.to_string(),
                options: SubscriptionOptions::default(),
            }],
            properties: Properties::default(),
            protocol_version: 5,
        };
        match identifier {
            Some(id) => packet.with_subscription_identifier(id),
            None => packet,
        }
    }

    fn capabilities(id: PropertyId) -> ServerCapabilities {
        let mut connack = ConnAckPacket::new(false, ReasonCode::Success);
        let _ = connack.properties.add(id, PropertyValue::Byte(0));
        ServerCapabilities::from_connack(&connack)
    }

    fn publish_with(properties: PublishProperties) -> PublishOptions {
        PublishOptions {
            properties,
            ..Default::default()
        }
    }

    #[test]
    fn subscribe_respects_server_capabilities() {
        let all = ServerCapabilities::default();
        assert!(all.check_subscribe(&subscribe("a/#", Some(1))).is_ok());
        assert!(all.check_subscribe(&subscribe("$share/g/a", None)).is_ok());

        let no_wildcards = capabilities(PropertyId::WildcardSubscriptionAvailable);
        assert!(matches!(
            no_wildcards.check_subscribe(&subscribe("a/+", None)),
            Err(MqttError::WildcardSubscriptionsNotSupported)
        ));
        assert!(no_wildcards
            .check_subscribe(&subscribe("a/b", None))
            .is_ok());

        let no_shared = capabilities(PropertyId::SharedSubscriptionAvailable);
        assert!(matches!(
            no_shared.check_subscribe(&subscribe("$share/g/a", None)),
            Err(MqttError::SharedSubscriptionsNotSupported)
        ));

        let no_ids = capabilities(PropertyId::SubscriptionIdentifierAvailable);
        assert!(matches!(
            no_ids.check_subscribe(&subscribe("a", Some(3))),
            Err(MqttError::SubscriptionIdentifiersNotSupported)
        ));
    }

    #[test]
    fn subscribe_rejects_identifier_zero_and_invalid_filters() {
        let all = ServerCapabilities::default();
        assert!(all.check_subscribe(&subscribe("a", Some(0))).is_err());
        assert!(all.check_subscribe(&subscribe("a/#/b", None)).is_err());
        assert!(all.check_subscribe(&subscribe("$share/+/x", None)).is_err());
    }

    #[test]
    fn publish_validation() {
        let plain = PublishOptions::default();
        assert!(check_publish("a/b", &plain).is_ok());
        assert!(check_publish("a/+", &plain).is_err());
        assert!(check_publish("", &plain).is_err());
        assert!(check_publish(
            "",
            &publish_with(PublishProperties {
                topic_alias: Some(1),
                ..Default::default()
            })
        )
        .is_ok());
        assert!(check_publish(
            "a",
            &publish_with(PublishProperties {
                topic_alias: Some(0),
                ..Default::default()
            })
        )
        .is_err());
        assert!(check_publish(
            "a",
            &publish_with(PublishProperties {
                response_topic: Some("r/#".to_string()),
                ..Default::default()
            })
        )
        .is_err());
        assert!(check_publish(
            "a",
            &publish_with(PublishProperties {
                subscription_identifiers: vec![1],
                ..Default::default()
            })
        )
        .is_err());
    }

    #[test]
    fn topic_alias_bounds_and_mapping() {
        let mut aliases = TopicAliasManager::new(2);
        assert!(check_topic_alias(&aliases, "a", 2).is_ok());
        assert!(check_topic_alias(&aliases, "a", 3).is_err());
        assert!(check_topic_alias(&aliases, "", 1).is_err());
        aliases.register_alias(1, "a").unwrap();
        assert!(check_topic_alias(&aliases, "", 1).is_ok());
        assert!(check_topic_alias(&TopicAliasManager::new(0), "a", 1).is_err());
    }
}
