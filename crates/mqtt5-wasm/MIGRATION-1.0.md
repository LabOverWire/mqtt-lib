# Migration Guide: mqtt5-wasm 0.x → 1.0

## Overview

Version 1.0 renames all JavaScript exports to follow standard camelCase conventions. Rust internals are unchanged — only the JS-facing API surface changed.

## Type Renames

All `Wasm`-prefixed types are now clean names:

| 0.x | 1.0 |
|-----|-----|
| `WasmMqttClient` | `MqttClient` |
| `WasmBroker` | `Broker` |
| `WasmBrokerConfig` | `BrokerConfig` |
| `WasmConnectOptions` | `ConnectOptions` |
| `WasmPublishOptions` | `PublishOptions` |
| `WasmSubscribeOptions` | `SubscribeOptions` |
| `WasmReconnectOptions` | `ReconnectOptions` |
| `WasmWillMessage` | `WillMessage` |
| `WasmMessageProperties` | `MessageProperties` |
| `WasmCodecRegistry` | `CodecRegistry` |
| `WasmGzipCodec` | `GzipCodec` |
| `WasmDeflateCodec` | `DeflateCodec` |
| `WasmBridgeConfig` | `BridgeConfig` |
| `WasmTopicMapping` | `TopicMapping` |
| `WasmBridgeDirection` | `BridgeDirection` |

**Example:**
```diff
-import init, { WasmBroker, WasmMqttClient } from "./pkg/mqtt5_wasm.js";
+import init, { Broker, MqttClient } from "./pkg/mqtt5_wasm.js";
```

## MqttClient Methods

| 0.x | 1.0 |
|-----|-----|
| `connect_with_options(url, opts)` | `connectWithOptions(url, opts)` |
| `connect_message_port(port)` | `connectMessagePort(port)` |
| `connect_message_port_with_options(port, opts)` | `connectMessagePortWithOptions(port, opts)` |
| `connect_broadcast_channel(name)` | `connectBroadcastChannel(name)` |
| `publish_with_options(topic, payload, opts)` | `publishWithOptions(topic, payload, opts)` |
| `publish_qos1(topic, payload, cb)` | `publishQos1(topic, payload, cb)` |
| `publish_qos2(topic, payload, cb)` | `publishQos2(topic, payload, cb)` |
| `subscribe_with_options(topic, cb, opts)` | `subscribeWithOptions(topic, cb, opts)` |
| `subscribe_with_callback(topic, cb)` | `subscribeWithCallback(topic, cb)` |
| `is_connected()` | `isConnected()` |
| `on_connect(cb)` | `onConnect(cb)` |
| `on_disconnect(cb)` | `onDisconnect(cb)` |
| `on_error(cb)` | `onError(cb)` |
| `on_auth_challenge(cb)` | `onAuthChallenge(cb)` |
| `on_reconnecting(cb)` | `onReconnecting(cb)` |
| `on_reconnect_failed(cb)` | `onReconnectFailed(cb)` |
| `on_connectivity_change(cb)` | `onConnectivityChange(cb)` |
| `is_browser_online()` | `isBrowserOnline()` |
| `set_reconnect_options(opts)` | `setReconnectOptions(opts)` |
| `enable_auto_reconnect(enabled)` | `enableAutoReconnect(enabled)` |
| `is_reconnecting()` | `isReconnecting()` |
| `respond_auth(data)` | `respondAuth(data)` |

## Broker Methods

| 0.x | 1.0 |
|-----|-----|
| `Broker.with_config(config)` | `Broker.withConfig(config)` |
| `add_user(user, pass)` | `addUser(user, pass)` |
| `add_user_with_hash(user, hash)` | `addUserWithHash(user, hash)` |
| `remove_user(user)` | `removeUser(user)` |
| `has_user(user)` | `hasUser(user)` |
| `user_count()` | `userCount()` |
| `Broker.hash_password(pass)` | `Broker.hashPassword(pass)` |
| `add_acl_rule(user, pattern, perm)` | `addAclRule(user, pattern, perm)` |
| `clear_acl_rules()` | `clearAclRules()` |
| `acl_rule_count()` | `aclRuleCount()` |
| `add_role(name)` | `addRole(name)` |
| `remove_role(name)` | `removeRole(name)` |
| `list_roles()` | `listRoles()` |
| `role_count()` | `roleCount()` |
| `add_role_rule(role, pattern, perm)` | `addRoleRule(role, pattern, perm)` |
| `assign_role(user, role)` | `assignRole(user, role)` |
| `unassign_role(user, role)` | `unassignRole(user, role)` |
| `get_user_roles(user)` | `getUserRoles(user)` |
| `clear_roles()` | `clearRoles()` |
| `set_acl_default_deny()` | `setAclDefaultDeny()` |
| `set_acl_default_allow()` | `setAclDefaultAllow()` |
| `create_client_port()` | `createClientPort()` |
| `add_bridge(config, port)` | `addBridge(config, port)` |
| `remove_bridge(name)` | `removeBridge(name)` |
| `list_bridges()` | `listBridges()` |
| `stop_all_bridges()` | `stopAllBridges()` |
| `start_sys_topics()` | `startSysTopics()` |
| `start_sys_topics_with_interval_secs(n)` | `startSysTopicsWithIntervalSecs(n)` |
| `stop_sys_topics()` | `stopSysTopics()` |
| `update_config(config)` | `updateConfig(config)` |
| `get_config_hash()` | `getConfigHash()` |
| `on_config_change(cb)` | `onConfigChange(cb)` |
| `on_client_connect(cb)` | `onClientConnect(cb)` |
| `on_client_disconnect(cb)` | `onClientDisconnect(cb)` |
| `on_client_publish(cb)` | `onClientPublish(cb)` |
| `on_client_subscribe(cb)` | `onClientSubscribe(cb)` |
| `on_client_unsubscribe(cb)` | `onClientUnsubscribe(cb)` |
| `on_message_delivered(cb)` | `onMessageDelivered(cb)` |
| `get_max_clients()` | `getMaxClients()` |
| `get_max_packet_size()` | `getMaxPacketSize()` |
| `get_session_expiry_interval_secs()` | `getSessionExpiryIntervalSecs()` |

## BrokerConfig Properties

All setter properties changed from snake_case to camelCase:

| 0.x | 1.0 |
|-----|-----|
| `config.max_clients` | `config.maxClients` |
| `config.session_expiry_interval_secs` | `config.sessionExpiryIntervalSecs` |
| `config.max_packet_size` | `config.maxPacketSize` |
| `config.topic_alias_maximum` | `config.topicAliasMaximum` |
| `config.retain_available` | `config.retainAvailable` |
| `config.maximum_qos` | `config.maximumQos` |
| `config.wildcard_subscription_available` | `config.wildcardSubscriptionAvailable` |
| `config.subscription_identifier_available` | `config.subscriptionIdentifierAvailable` |
| `config.shared_subscription_available` | `config.sharedSubscriptionAvailable` |
| `config.server_keep_alive_secs` | `config.serverKeepAliveSecs` |
| `config.allow_anonymous` | `config.allowAnonymous` |
| `config.change_only_delivery_enabled` | `config.changeOnlyDeliveryEnabled` |
| `config.echo_suppression_enabled` | `config.echoSuppressionEnabled` |
| `config.echo_suppression_property_key` | `config.echoSuppressionPropertyKey` |

**BrokerConfig methods:**

| 0.x | 1.0 |
|-----|-----|
| `add_change_only_delivery_pattern(p)` | `addChangeOnlyDeliveryPattern(p)` |
| `clear_change_only_delivery_patterns()` | `clearChangeOnlyDeliveryPatterns()` |

## BridgeConfig Properties

| 0.x | 1.0 |
|-----|-----|
| `bridge.client_id` | `bridge.clientId` |
| `bridge.clean_start` | `bridge.cleanStart` |
| `bridge.keep_alive_secs` | `bridge.keepAliveSecs` |
| `bridge.loop_prevention_ttl_secs` | `bridge.loopPreventionTtlSecs` |
| `bridge.loop_prevention_cache_size` | `bridge.loopPreventionCacheSize` |
| `add_topic(mapping)` | `addTopic(mapping)` |

## TopicMapping Properties

| 0.x | 1.0 |
|-----|-----|
| `mapping.local_prefix` | `mapping.localPrefix` |
| `mapping.remote_prefix` | `mapping.remotePrefix` |

## ConnectOptions Methods

| 0.x | 1.0 |
|-----|-----|
| `set_will(will)` | `setWill(will)` |
| `clear_will()` | `clearWill()` |

## Free Functions

| 0.x | 1.0 |
|-----|-----|
| `create_codec_registry()` | `createCodecRegistry()` |
| `create_gzip_codec(level, minSize)` | `createGzipCodec(level, minSize)` |
| `create_deflate_codec(level, minSize)` | `createDeflateCodec(level, minSize)` |

## Quick Migration

For most projects, a global find-and-replace handles the bulk of the migration:

1. Replace type names: `WasmBroker` → `Broker`, `WasmMqttClient` → `MqttClient`, etc.
2. Replace method calls: search for `_` in method names and convert to camelCase
3. Replace config properties: `config.max_clients` → `config.maxClients`, etc.

All `ConnectOptions`, `PublishOptions`, `SubscribeOptions`, `WillMessage`, and `MessageProperties` properties were already camelCase in 0.x and remain unchanged.
