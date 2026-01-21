/* tslint:disable */
/* eslint-disable */

export class WasmBridgeConfig {
    free(): void;
    [Symbol.dispose](): void;
    add_topic(mapping: WasmTopicMapping): void;
    constructor(name: string);
    /**
     * # Errors
     * Returns an error if the bridge configuration is invalid.
     */
    validate(): void;
    set clean_start(value: boolean);
    set client_id(value: string);
    set keep_alive_secs(value: number);
    /**
     * Sets the maximum number of message fingerprints to cache.
     *
     * When exceeded, expired entries are cleaned up. Default: 10000.
     */
    set loop_prevention_cache_size(value: number);
    /**
     * Sets how long message fingerprints are remembered for loop detection.
     *
     * Messages with the same fingerprint seen within this window are blocked.
     * Default: 60 seconds.
     */
    set loop_prevention_ttl_secs(value: bigint);
    set password(value: string | null | undefined);
    set username(value: string | null | undefined);
}

export enum WasmBridgeDirection {
    In = 0,
    Out = 1,
    Both = 2,
}

export class WasmBroker {
    free(): void;
    [Symbol.dispose](): void;
    acl_rule_count(): Promise<number>;
    /**
     * # Errors
     * Returns an error if the permission string is invalid.
     */
    add_acl_rule(username: string, topic_pattern: string, permission: string): Promise<void>;
    /**
     * # Errors
     * Returns an error if the bridge cannot be added.
     */
    add_bridge(config: WasmBridgeConfig, remote_port: MessagePort): Promise<void>;
    add_role(name: string): Promise<void>;
    /**
     * # Errors
     * Returns an error if the permission string is invalid or role does not exist.
     */
    add_role_rule(role_name: string, topic_pattern: string, permission: string): Promise<void>;
    /**
     * # Errors
     * Returns an error if adding the user fails.
     */
    add_user(username: string, password: string): void;
    add_user_with_hash(username: string, password_hash: string): void;
    /**
     * # Errors
     * Returns an error if the role does not exist.
     */
    assign_role(username: string, role_name: string): Promise<void>;
    clear_acl_rules(): Promise<void>;
    clear_roles(): Promise<void>;
    /**
     * # Errors
     * Returns an error if the `MessageChannel` cannot be created.
     */
    create_client_port(): MessagePort;
    get_config_hash(): number;
    get_max_clients(): number;
    get_max_packet_size(): number;
    get_session_expiry_interval_secs(): number;
    get_user_roles(username: string): Promise<string[]>;
    has_user(username: string): boolean;
    /**
     * # Errors
     * Returns an error if password hashing fails.
     */
    static hash_password(password: string): string;
    list_bridges(): string[];
    list_roles(): Promise<string[]>;
    /**
     * # Errors
     * Returns an error if broker initialization fails.
     */
    constructor();
    on_client_connect(callback: Function): void;
    on_client_disconnect(callback: Function): void;
    on_client_publish(callback: Function): void;
    on_client_subscribe(callback: Function): void;
    on_client_unsubscribe(callback: Function): void;
    on_config_change(callback: Function): void;
    on_message_delivered(callback: Function): void;
    /**
     * # Errors
     * Returns an error if the bridge cannot be removed.
     */
    remove_bridge(name: string): Promise<void>;
    remove_role(name: string): Promise<boolean>;
    remove_user(username: string): boolean;
    role_count(): Promise<number>;
    set_acl_default_allow(): Promise<void>;
    set_acl_default_deny(): Promise<void>;
    start_sys_topics(): void;
    start_sys_topics_with_interval_secs(interval_secs: number): void;
    stop_all_bridges(): Promise<void>;
    stop_sys_topics(): void;
    unassign_role(username: string, role_name: string): Promise<boolean>;
    /**
     * # Errors
     * Returns an error if the config write lock cannot be acquired.
     */
    update_config(new_config: WasmBrokerConfig): void;
    user_count(): number;
    /**
     * # Errors
     * Returns an error if broker initialization fails.
     */
    static with_config(wasm_config: WasmBrokerConfig): WasmBroker;
}

export class WasmBrokerConfig {
    free(): void;
    [Symbol.dispose](): void;
    add_delta_subscription_pattern(pattern: string): void;
    clear_delta_subscription_patterns(): void;
    constructor();
    set allow_anonymous(value: boolean);
    set delta_subscription_enabled(value: boolean);
    set max_clients(value: number);
    set max_packet_size(value: number);
    set maximum_qos(value: number);
    set retain_available(value: boolean);
    set server_keep_alive_secs(value: number | null | undefined);
    set session_expiry_interval_secs(value: number);
    set shared_subscription_available(value: boolean);
    set subscription_identifier_available(value: boolean);
    set topic_alias_maximum(value: number);
    set wildcard_subscription_available(value: boolean);
}

export class WasmConnectOptions {
    free(): void;
    [Symbol.dispose](): void;
    addBackupUrl(url: string): void;
    addUserProperty(key: string, value: string): void;
    clearBackupUrls(): void;
    clearUserProperties(): void;
    clear_will(): void;
    getBackupUrls(): string[];
    constructor();
    set_will(will: WasmWillMessage): void;
    get authenticationMethod(): string | undefined;
    set authenticationMethod(value: string | null | undefined);
    cleanStart: boolean;
    keepAlive: number;
    get maximumPacketSize(): number | undefined;
    set maximumPacketSize(value: number | null | undefined);
    protocolVersion: number;
    get receiveMaximum(): number | undefined;
    set receiveMaximum(value: number | null | undefined);
    get requestProblemInformation(): boolean | undefined;
    set requestProblemInformation(value: boolean | null | undefined);
    get requestResponseInformation(): boolean | undefined;
    set requestResponseInformation(value: boolean | null | undefined);
    get sessionExpiryInterval(): number | undefined;
    set sessionExpiryInterval(value: number | null | undefined);
    set authenticationData(value: Uint8Array);
    set password(value: Uint8Array);
    get topicAliasMaximum(): number | undefined;
    set topicAliasMaximum(value: number | null | undefined);
    get username(): string | undefined;
    set username(value: string | null | undefined);
}

export class WasmMessageProperties {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    getUserProperties(): Array<any>;
    readonly contentType: string | undefined;
    readonly correlationData: Uint8Array | undefined;
    readonly messageExpiryInterval: number | undefined;
    readonly payloadFormatIndicator: boolean | undefined;
    readonly responseTopic: string | undefined;
    readonly subscriptionIdentifiers: Uint32Array;
}

export class WasmMqttClient {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * # Errors
     * Returns an error if connection fails.
     */
    connect(url: string): Promise<void>;
    /**
     * # Errors
     * Returns an error if connection fails.
     */
    connect_broadcast_channel(channel_name: string): Promise<void>;
    /**
     * # Errors
     * Returns an error if connection fails.
     */
    connect_message_port(port: MessagePort): Promise<void>;
    /**
     * # Errors
     * Returns an error if connection fails.
     */
    connect_message_port_with_options(port: MessagePort, config: WasmConnectOptions): Promise<void>;
    /**
     * # Errors
     * Returns an error if connection fails.
     */
    connect_with_options(url: string, config: WasmConnectOptions): Promise<void>;
    /**
     * # Errors
     * Returns an error if disconnect fails.
     */
    disconnect(): Promise<void>;
    enable_auto_reconnect(enabled: boolean): void;
    is_connected(): boolean;
    is_reconnecting(): boolean;
    constructor(client_id: string);
    on_auth_challenge(callback: Function): void;
    on_connect(callback: Function): void;
    on_disconnect(callback: Function): void;
    on_error(callback: Function): void;
    on_reconnect_failed(callback: Function): void;
    on_reconnecting(callback: Function): void;
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     */
    publish(topic: string, payload: Uint8Array): Promise<void>;
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     */
    publish_qos1(topic: string, payload: Uint8Array, callback: Function): Promise<number>;
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     */
    publish_qos2(topic: string, payload: Uint8Array, callback: Function): Promise<number>;
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     */
    publish_with_options(topic: string, payload: Uint8Array, options: WasmPublishOptions): Promise<void>;
    /**
     * # Errors
     * Returns an error if no auth method is set or send fails.
     */
    respond_auth(auth_data: Uint8Array): void;
    set_reconnect_options(options: WasmReconnectOptions): void;
    /**
     * # Errors
     * Returns an error if not connected or subscribe fails.
     */
    subscribe(topic: string): Promise<number>;
    /**
     * # Errors
     * Returns an error if not connected or subscribe fails.
     */
    subscribe_with_callback(topic: string, callback: Function): Promise<number>;
    /**
     * # Errors
     * Returns an error if not connected or subscribe fails.
     */
    subscribe_with_options(topic: string, callback: Function, options: WasmSubscribeOptions): Promise<number>;
    /**
     * # Errors
     * Returns an error if not connected or unsubscribe fails.
     */
    unsubscribe(topic: string): Promise<number>;
}

export class WasmPublishOptions {
    free(): void;
    [Symbol.dispose](): void;
    addUserProperty(key: string, value: string): void;
    clearUserProperties(): void;
    constructor();
    get contentType(): string | undefined;
    set contentType(value: string | null | undefined);
    get messageExpiryInterval(): number | undefined;
    set messageExpiryInterval(value: number | null | undefined);
    get payloadFormatIndicator(): boolean | undefined;
    set payloadFormatIndicator(value: boolean | null | undefined);
    qos: number;
    get responseTopic(): string | undefined;
    set responseTopic(value: string | null | undefined);
    retain: boolean;
    set correlationData(value: Uint8Array);
    get topicAlias(): number | undefined;
    set topicAlias(value: number | null | undefined);
}

export class WasmReconnectOptions {
    free(): void;
    [Symbol.dispose](): void;
    static disabled(): WasmReconnectOptions;
    constructor();
    backoffFactor: number;
    enabled: boolean;
    initialDelayMs: number;
    get maxAttempts(): number | undefined;
    set maxAttempts(value: number | null | undefined);
    maxDelayMs: number;
}

export class WasmSubscribeOptions {
    free(): void;
    [Symbol.dispose](): void;
    constructor();
    noLocal: boolean;
    qos: number;
    retainAsPublished: boolean;
    retainHandling: number;
    get subscriptionIdentifier(): number | undefined;
    set subscriptionIdentifier(value: number | null | undefined);
}

export class WasmTopicMapping {
    free(): void;
    [Symbol.dispose](): void;
    constructor(pattern: string, direction: WasmBridgeDirection);
    set local_prefix(value: string | null | undefined);
    set qos(value: number);
    set remote_prefix(value: string | null | undefined);
}

export class WasmWillMessage {
    free(): void;
    [Symbol.dispose](): void;
    constructor(topic: string, payload: Uint8Array);
    get contentType(): string | undefined;
    set contentType(value: string | null | undefined);
    get messageExpiryInterval(): number | undefined;
    set messageExpiryInterval(value: number | null | undefined);
    qos: number;
    get responseTopic(): string | undefined;
    set responseTopic(value: string | null | undefined);
    retain: boolean;
    topic: string;
    get willDelayInterval(): number | undefined;
    set willDelayInterval(value: number | null | undefined);
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmbridgeconfig_free: (a: number, b: number) => void;
    readonly __wbg_wasmbroker_free: (a: number, b: number) => void;
    readonly __wbg_wasmbrokerconfig_free: (a: number, b: number) => void;
    readonly __wbg_wasmconnectoptions_free: (a: number, b: number) => void;
    readonly __wbg_wasmmessageproperties_free: (a: number, b: number) => void;
    readonly __wbg_wasmmqttclient_free: (a: number, b: number) => void;
    readonly __wbg_wasmpublishoptions_free: (a: number, b: number) => void;
    readonly __wbg_wasmreconnectoptions_free: (a: number, b: number) => void;
    readonly __wbg_wasmsubscribeoptions_free: (a: number, b: number) => void;
    readonly __wbg_wasmtopicmapping_free: (a: number, b: number) => void;
    readonly __wbg_wasmwillmessage_free: (a: number, b: number) => void;
    readonly wasmbridgeconfig_add_topic: (a: number, b: number) => void;
    readonly wasmbridgeconfig_new: (a: number, b: number) => number;
    readonly wasmbridgeconfig_set_clean_start: (a: number, b: number) => void;
    readonly wasmbridgeconfig_set_client_id: (a: number, b: number, c: number) => void;
    readonly wasmbridgeconfig_set_keep_alive_secs: (a: number, b: number) => void;
    readonly wasmbridgeconfig_set_loop_prevention_cache_size: (a: number, b: number) => void;
    readonly wasmbridgeconfig_set_loop_prevention_ttl_secs: (a: number, b: bigint) => void;
    readonly wasmbridgeconfig_set_password: (a: number, b: number, c: number) => void;
    readonly wasmbridgeconfig_set_username: (a: number, b: number, c: number) => void;
    readonly wasmbridgeconfig_validate: (a: number, b: number) => void;
    readonly wasmbroker_acl_rule_count: (a: number) => number;
    readonly wasmbroker_add_acl_rule: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly wasmbroker_add_bridge: (a: number, b: number, c: number) => number;
    readonly wasmbroker_add_role: (a: number, b: number, c: number) => number;
    readonly wasmbroker_add_role_rule: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly wasmbroker_add_user: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly wasmbroker_add_user_with_hash: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly wasmbroker_assign_role: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly wasmbroker_clear_acl_rules: (a: number) => number;
    readonly wasmbroker_clear_roles: (a: number) => number;
    readonly wasmbroker_create_client_port: (a: number, b: number) => void;
    readonly wasmbroker_get_config_hash: (a: number) => number;
    readonly wasmbroker_get_max_clients: (a: number) => number;
    readonly wasmbroker_get_max_packet_size: (a: number) => number;
    readonly wasmbroker_get_session_expiry_interval_secs: (a: number) => number;
    readonly wasmbroker_get_user_roles: (a: number, b: number, c: number) => number;
    readonly wasmbroker_has_user: (a: number, b: number, c: number) => number;
    readonly wasmbroker_hash_password: (a: number, b: number, c: number) => void;
    readonly wasmbroker_list_bridges: (a: number, b: number) => void;
    readonly wasmbroker_list_roles: (a: number) => number;
    readonly wasmbroker_new: (a: number) => void;
    readonly wasmbroker_on_client_connect: (a: number, b: number) => void;
    readonly wasmbroker_on_client_disconnect: (a: number, b: number) => void;
    readonly wasmbroker_on_client_publish: (a: number, b: number) => void;
    readonly wasmbroker_on_client_subscribe: (a: number, b: number) => void;
    readonly wasmbroker_on_client_unsubscribe: (a: number, b: number) => void;
    readonly wasmbroker_on_config_change: (a: number, b: number) => void;
    readonly wasmbroker_on_message_delivered: (a: number, b: number) => void;
    readonly wasmbroker_remove_bridge: (a: number, b: number, c: number) => number;
    readonly wasmbroker_remove_role: (a: number, b: number, c: number) => number;
    readonly wasmbroker_remove_user: (a: number, b: number, c: number) => number;
    readonly wasmbroker_role_count: (a: number) => number;
    readonly wasmbroker_set_acl_default_allow: (a: number) => number;
    readonly wasmbroker_set_acl_default_deny: (a: number) => number;
    readonly wasmbroker_start_sys_topics: (a: number) => void;
    readonly wasmbroker_start_sys_topics_with_interval_secs: (a: number, b: number) => void;
    readonly wasmbroker_stop_all_bridges: (a: number) => number;
    readonly wasmbroker_stop_sys_topics: (a: number) => void;
    readonly wasmbroker_unassign_role: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly wasmbroker_update_config: (a: number, b: number, c: number) => void;
    readonly wasmbroker_user_count: (a: number) => number;
    readonly wasmbroker_with_config: (a: number, b: number) => void;
    readonly wasmbrokerconfig_add_delta_subscription_pattern: (a: number, b: number, c: number) => void;
    readonly wasmbrokerconfig_clear_delta_subscription_patterns: (a: number) => void;
    readonly wasmbrokerconfig_new: () => number;
    readonly wasmbrokerconfig_set_allow_anonymous: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_delta_subscription_enabled: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_max_clients: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_max_packet_size: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_maximum_qos: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_retain_available: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_server_keep_alive_secs: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_session_expiry_interval_secs: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_shared_subscription_available: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_subscription_identifier_available: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_topic_alias_maximum: (a: number, b: number) => void;
    readonly wasmbrokerconfig_set_wildcard_subscription_available: (a: number, b: number) => void;
    readonly wasmconnectoptions_addBackupUrl: (a: number, b: number, c: number) => void;
    readonly wasmconnectoptions_addUserProperty: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly wasmconnectoptions_authenticationMethod: (a: number, b: number) => void;
    readonly wasmconnectoptions_cleanStart: (a: number) => number;
    readonly wasmconnectoptions_clearBackupUrls: (a: number) => void;
    readonly wasmconnectoptions_clearUserProperties: (a: number) => void;
    readonly wasmconnectoptions_clear_will: (a: number) => void;
    readonly wasmconnectoptions_getBackupUrls: (a: number, b: number) => void;
    readonly wasmconnectoptions_keepAlive: (a: number) => number;
    readonly wasmconnectoptions_maximumPacketSize: (a: number) => number;
    readonly wasmconnectoptions_new: () => number;
    readonly wasmconnectoptions_protocolVersion: (a: number) => number;
    readonly wasmconnectoptions_receiveMaximum: (a: number) => number;
    readonly wasmconnectoptions_requestProblemInformation: (a: number) => number;
    readonly wasmconnectoptions_requestResponseInformation: (a: number) => number;
    readonly wasmconnectoptions_sessionExpiryInterval: (a: number) => number;
    readonly wasmconnectoptions_set_authenticationData: (a: number, b: number, c: number) => void;
    readonly wasmconnectoptions_set_authenticationMethod: (a: number, b: number, c: number) => void;
    readonly wasmconnectoptions_set_cleanStart: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_keepAlive: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_maximumPacketSize: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_password: (a: number, b: number, c: number) => void;
    readonly wasmconnectoptions_set_protocolVersion: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_receiveMaximum: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_requestProblemInformation: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_requestResponseInformation: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_sessionExpiryInterval: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_topicAliasMaximum: (a: number, b: number) => void;
    readonly wasmconnectoptions_set_username: (a: number, b: number, c: number) => void;
    readonly wasmconnectoptions_set_will: (a: number, b: number) => void;
    readonly wasmconnectoptions_topicAliasMaximum: (a: number) => number;
    readonly wasmconnectoptions_username: (a: number, b: number) => void;
    readonly wasmmessageproperties_contentType: (a: number, b: number) => void;
    readonly wasmmessageproperties_correlationData: (a: number, b: number) => void;
    readonly wasmmessageproperties_getUserProperties: (a: number) => number;
    readonly wasmmessageproperties_messageExpiryInterval: (a: number) => number;
    readonly wasmmessageproperties_payloadFormatIndicator: (a: number) => number;
    readonly wasmmessageproperties_responseTopic: (a: number, b: number) => void;
    readonly wasmmessageproperties_subscriptionIdentifiers: (a: number, b: number) => void;
    readonly wasmmqttclient_connect: (a: number, b: number, c: number) => number;
    readonly wasmmqttclient_connect_broadcast_channel: (a: number, b: number, c: number) => number;
    readonly wasmmqttclient_connect_message_port: (a: number, b: number) => number;
    readonly wasmmqttclient_connect_message_port_with_options: (a: number, b: number, c: number) => number;
    readonly wasmmqttclient_connect_with_options: (a: number, b: number, c: number, d: number) => number;
    readonly wasmmqttclient_disconnect: (a: number) => number;
    readonly wasmmqttclient_enable_auto_reconnect: (a: number, b: number) => void;
    readonly wasmmqttclient_is_connected: (a: number) => number;
    readonly wasmmqttclient_is_reconnecting: (a: number) => number;
    readonly wasmmqttclient_new: (a: number, b: number) => number;
    readonly wasmmqttclient_on_auth_challenge: (a: number, b: number) => void;
    readonly wasmmqttclient_on_connect: (a: number, b: number) => void;
    readonly wasmmqttclient_on_disconnect: (a: number, b: number) => void;
    readonly wasmmqttclient_on_error: (a: number, b: number) => void;
    readonly wasmmqttclient_on_reconnect_failed: (a: number, b: number) => void;
    readonly wasmmqttclient_on_reconnecting: (a: number, b: number) => void;
    readonly wasmmqttclient_publish: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly wasmmqttclient_publish_qos1: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly wasmmqttclient_publish_qos2: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly wasmmqttclient_publish_with_options: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly wasmmqttclient_respond_auth: (a: number, b: number, c: number, d: number) => void;
    readonly wasmmqttclient_set_reconnect_options: (a: number, b: number) => void;
    readonly wasmmqttclient_subscribe: (a: number, b: number, c: number) => number;
    readonly wasmmqttclient_subscribe_with_callback: (a: number, b: number, c: number, d: number) => number;
    readonly wasmmqttclient_subscribe_with_options: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly wasmmqttclient_unsubscribe: (a: number, b: number, c: number) => number;
    readonly wasmpublishoptions_addUserProperty: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly wasmpublishoptions_clearUserProperties: (a: number) => void;
    readonly wasmpublishoptions_contentType: (a: number, b: number) => void;
    readonly wasmpublishoptions_messageExpiryInterval: (a: number) => number;
    readonly wasmpublishoptions_new: () => number;
    readonly wasmpublishoptions_payloadFormatIndicator: (a: number) => number;
    readonly wasmpublishoptions_qos: (a: number) => number;
    readonly wasmpublishoptions_responseTopic: (a: number, b: number) => void;
    readonly wasmpublishoptions_retain: (a: number) => number;
    readonly wasmpublishoptions_set_contentType: (a: number, b: number, c: number) => void;
    readonly wasmpublishoptions_set_correlationData: (a: number, b: number, c: number) => void;
    readonly wasmpublishoptions_set_messageExpiryInterval: (a: number, b: number) => void;
    readonly wasmpublishoptions_set_payloadFormatIndicator: (a: number, b: number) => void;
    readonly wasmpublishoptions_set_qos: (a: number, b: number) => void;
    readonly wasmpublishoptions_set_responseTopic: (a: number, b: number, c: number) => void;
    readonly wasmpublishoptions_set_retain: (a: number, b: number) => void;
    readonly wasmpublishoptions_set_topicAlias: (a: number, b: number) => void;
    readonly wasmpublishoptions_topicAlias: (a: number) => number;
    readonly wasmreconnectoptions_backoffFactor: (a: number) => number;
    readonly wasmreconnectoptions_disabled: () => number;
    readonly wasmreconnectoptions_enabled: (a: number) => number;
    readonly wasmreconnectoptions_initialDelayMs: (a: number) => number;
    readonly wasmreconnectoptions_maxAttempts: (a: number) => number;
    readonly wasmreconnectoptions_maxDelayMs: (a: number) => number;
    readonly wasmreconnectoptions_new: () => number;
    readonly wasmreconnectoptions_set_backoffFactor: (a: number, b: number) => void;
    readonly wasmreconnectoptions_set_enabled: (a: number, b: number) => void;
    readonly wasmreconnectoptions_set_initialDelayMs: (a: number, b: number) => void;
    readonly wasmreconnectoptions_set_maxAttempts: (a: number, b: number) => void;
    readonly wasmreconnectoptions_set_maxDelayMs: (a: number, b: number) => void;
    readonly wasmsubscribeoptions_new: () => number;
    readonly wasmsubscribeoptions_noLocal: (a: number) => number;
    readonly wasmsubscribeoptions_qos: (a: number) => number;
    readonly wasmsubscribeoptions_retainAsPublished: (a: number) => number;
    readonly wasmsubscribeoptions_retainHandling: (a: number) => number;
    readonly wasmsubscribeoptions_set_noLocal: (a: number, b: number) => void;
    readonly wasmsubscribeoptions_set_qos: (a: number, b: number) => void;
    readonly wasmsubscribeoptions_set_retainAsPublished: (a: number, b: number) => void;
    readonly wasmsubscribeoptions_set_retainHandling: (a: number, b: number) => void;
    readonly wasmsubscribeoptions_set_subscriptionIdentifier: (a: number, b: number) => void;
    readonly wasmsubscribeoptions_subscriptionIdentifier: (a: number) => number;
    readonly wasmtopicmapping_new: (a: number, b: number, c: number) => number;
    readonly wasmtopicmapping_set_local_prefix: (a: number, b: number, c: number) => void;
    readonly wasmtopicmapping_set_qos: (a: number, b: number) => void;
    readonly wasmtopicmapping_set_remote_prefix: (a: number, b: number, c: number) => void;
    readonly wasmwillmessage_contentType: (a: number, b: number) => void;
    readonly wasmwillmessage_messageExpiryInterval: (a: number) => number;
    readonly wasmwillmessage_new: (a: number, b: number, c: number, d: number) => number;
    readonly wasmwillmessage_qos: (a: number) => number;
    readonly wasmwillmessage_responseTopic: (a: number, b: number) => void;
    readonly wasmwillmessage_retain: (a: number) => number;
    readonly wasmwillmessage_set_contentType: (a: number, b: number, c: number) => void;
    readonly wasmwillmessage_set_messageExpiryInterval: (a: number, b: number) => void;
    readonly wasmwillmessage_set_qos: (a: number, b: number) => void;
    readonly wasmwillmessage_set_responseTopic: (a: number, b: number, c: number) => void;
    readonly wasmwillmessage_set_retain: (a: number, b: number) => void;
    readonly wasmwillmessage_set_topic: (a: number, b: number, c: number) => void;
    readonly wasmwillmessage_set_willDelayInterval: (a: number, b: number) => void;
    readonly wasmwillmessage_topic: (a: number, b: number) => void;
    readonly wasmwillmessage_willDelayInterval: (a: number) => number;
    readonly __wasm_bindgen_func_elem_2267: (a: number, b: number) => void;
    readonly __wasm_bindgen_func_elem_2318: (a: number, b: number) => void;
    readonly __wasm_bindgen_func_elem_125: (a: number, b: number) => void;
    readonly __wasm_bindgen_func_elem_3612: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_2334: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_875: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_2284: (a: number, b: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
