/* @ts-self-types="./mqtt5_wasm.d.ts" */

export class WasmBridgeConfig {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmBridgeConfigFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmbridgeconfig_free(ptr, 0);
    }
    /**
     * @param {WasmTopicMapping} mapping
     */
    add_topic(mapping) {
        _assertClass(mapping, WasmTopicMapping);
        var ptr0 = mapping.__destroy_into_raw();
        wasm.wasmbridgeconfig_add_topic(this.__wbg_ptr, ptr0);
    }
    /**
     * @param {string} name
     */
    constructor(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbridgeconfig_new(ptr0, len0);
        this.__wbg_ptr = ret >>> 0;
        WasmBridgeConfigFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @param {boolean} clean_start
     */
    set clean_start(clean_start) {
        wasm.wasmbridgeconfig_set_clean_start(this.__wbg_ptr, clean_start);
    }
    /**
     * @param {string} client_id
     */
    set client_id(client_id) {
        const ptr0 = passStringToWasm0(client_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmbridgeconfig_set_client_id(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {number} secs
     */
    set keep_alive_secs(secs) {
        wasm.wasmbridgeconfig_set_keep_alive_secs(this.__wbg_ptr, secs);
    }
    /**
     * Sets the maximum number of message fingerprints to cache.
     *
     * When exceeded, expired entries are cleaned up. Default: 10000.
     * @param {number} size
     */
    set loop_prevention_cache_size(size) {
        wasm.wasmbridgeconfig_set_loop_prevention_cache_size(this.__wbg_ptr, size);
    }
    /**
     * Sets how long message fingerprints are remembered for loop detection.
     *
     * Messages with the same fingerprint seen within this window are blocked.
     * Default: 60 seconds.
     * @param {bigint} secs
     */
    set loop_prevention_ttl_secs(secs) {
        wasm.wasmbridgeconfig_set_loop_prevention_ttl_secs(this.__wbg_ptr, secs);
    }
    /**
     * @param {string | null} [password]
     */
    set password(password) {
        var ptr0 = isLikeNone(password) ? 0 : passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmbridgeconfig_set_password(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {string | null} [username]
     */
    set username(username) {
        var ptr0 = isLikeNone(username) ? 0 : passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmbridgeconfig_set_username(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * # Errors
     * Returns an error if the bridge configuration is invalid.
     */
    validate() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmbridgeconfig_validate(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
}
if (Symbol.dispose) WasmBridgeConfig.prototype[Symbol.dispose] = WasmBridgeConfig.prototype.free;

/**
 * @enum {0 | 1 | 2}
 */
export const WasmBridgeDirection = Object.freeze({
    In: 0, "0": "In",
    Out: 1, "1": "Out",
    Both: 2, "2": "Both",
});

export class WasmBroker {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(WasmBroker.prototype);
        obj.__wbg_ptr = ptr;
        WasmBrokerFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmBrokerFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmbroker_free(ptr, 0);
    }
    /**
     * @returns {Promise<number>}
     */
    acl_rule_count() {
        const ret = wasm.wasmbroker_acl_rule_count(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if the permission string is invalid.
     * @param {string} username
     * @param {string} topic_pattern
     * @param {string} permission
     * @returns {Promise<void>}
     */
    add_acl_rule(username, topic_pattern, permission) {
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic_pattern, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(permission, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_add_acl_rule(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if the bridge cannot be added.
     * @param {WasmBridgeConfig} config
     * @param {MessagePort} remote_port
     * @returns {Promise<void>}
     */
    add_bridge(config, remote_port) {
        _assertClass(config, WasmBridgeConfig);
        var ptr0 = config.__destroy_into_raw();
        const ret = wasm.wasmbroker_add_bridge(this.__wbg_ptr, ptr0, addHeapObject(remote_port));
        return takeObject(ret);
    }
    /**
     * @param {string} name
     * @returns {Promise<void>}
     */
    add_role(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_add_role(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if the permission string is invalid or role does not exist.
     * @param {string} role_name
     * @param {string} topic_pattern
     * @param {string} permission
     * @returns {Promise<void>}
     */
    add_role_rule(role_name, topic_pattern, permission) {
        const ptr0 = passStringToWasm0(role_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic_pattern, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(permission, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_add_role_rule(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if adding the user fails.
     * @param {string} username
     * @param {string} password
     */
    add_user(username, password) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            wasm.wasmbroker_add_user(retptr, this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {string} username
     * @param {string} password_hash
     */
    add_user_with_hash(username, password_hash) {
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(password_hash, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.wasmbroker_add_user_with_hash(this.__wbg_ptr, ptr0, len0, ptr1, len1);
    }
    /**
     * # Errors
     * Returns an error if the role does not exist.
     * @param {string} username
     * @param {string} role_name
     * @returns {Promise<void>}
     */
    assign_role(username, role_name) {
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(role_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_assign_role(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<void>}
     */
    clear_acl_rules() {
        const ret = wasm.wasmbroker_clear_acl_rules(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<void>}
     */
    clear_roles() {
        const ret = wasm.wasmbroker_clear_roles(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if the `MessageChannel` cannot be created.
     * @returns {MessagePort}
     */
    create_client_port() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmbroker_create_client_port(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            return takeObject(r0);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {number}
     */
    get_config_hash() {
        const ret = wasm.wasmbroker_get_config_hash(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number}
     */
    get_max_clients() {
        const ret = wasm.wasmbroker_get_max_clients(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @returns {number}
     */
    get_max_packet_size() {
        const ret = wasm.wasmbroker_get_max_packet_size(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @returns {number}
     */
    get_session_expiry_interval_secs() {
        const ret = wasm.wasmbroker_get_session_expiry_interval_secs(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @param {string} username
     * @returns {Promise<string[]>}
     */
    get_user_roles(username) {
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_get_user_roles(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @param {string} username
     * @returns {boolean}
     */
    has_user(username) {
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_has_user(this.__wbg_ptr, ptr0, len0);
        return ret !== 0;
    }
    /**
     * # Errors
     * Returns an error if password hashing fails.
     * @param {string} password
     * @returns {string}
     */
    static hash_password(password) {
        let deferred3_0;
        let deferred3_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.wasmbroker_hash_password(retptr, ptr0, len0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
            var ptr2 = r0;
            var len2 = r1;
            if (r3) {
                ptr2 = 0; len2 = 0;
                throw takeObject(r2);
            }
            deferred3_0 = ptr2;
            deferred3_1 = len2;
            return getStringFromWasm0(ptr2, len2);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export4(deferred3_0, deferred3_1, 1);
        }
    }
    /**
     * @returns {string[]}
     */
    list_bridges() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmbroker_list_bridges(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayJsValueFromWasm0(r0, r1).slice();
            wasm.__wbindgen_export4(r0, r1 * 4, 4);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {Promise<string[]>}
     */
    list_roles() {
        const ret = wasm.wasmbroker_list_roles(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if broker initialization fails.
     */
    constructor() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmbroker_new(retptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            this.__wbg_ptr = r0 >>> 0;
            WasmBrokerFinalization.register(this, this.__wbg_ptr, this);
            return this;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {Function} callback
     */
    on_client_connect(callback) {
        wasm.wasmbroker_on_client_connect(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_client_disconnect(callback) {
        wasm.wasmbroker_on_client_disconnect(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_client_publish(callback) {
        wasm.wasmbroker_on_client_publish(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_client_subscribe(callback) {
        wasm.wasmbroker_on_client_subscribe(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_client_unsubscribe(callback) {
        wasm.wasmbroker_on_client_unsubscribe(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_config_change(callback) {
        wasm.wasmbroker_on_config_change(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_message_delivered(callback) {
        wasm.wasmbroker_on_message_delivered(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * # Errors
     * Returns an error if the bridge cannot be removed.
     * @param {string} name
     * @returns {Promise<void>}
     */
    remove_bridge(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_remove_bridge(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @param {string} name
     * @returns {Promise<boolean>}
     */
    remove_role(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_remove_role(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @param {string} username
     * @returns {boolean}
     */
    remove_user(username) {
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_remove_user(this.__wbg_ptr, ptr0, len0);
        return ret !== 0;
    }
    /**
     * @returns {Promise<number>}
     */
    role_count() {
        const ret = wasm.wasmbroker_role_count(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<void>}
     */
    set_acl_default_allow() {
        const ret = wasm.wasmbroker_set_acl_default_allow(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<void>}
     */
    set_acl_default_deny() {
        const ret = wasm.wasmbroker_set_acl_default_deny(this.__wbg_ptr);
        return takeObject(ret);
    }
    start_sys_topics() {
        wasm.wasmbroker_start_sys_topics(this.__wbg_ptr);
    }
    /**
     * @param {number} interval_secs
     */
    start_sys_topics_with_interval_secs(interval_secs) {
        wasm.wasmbroker_start_sys_topics_with_interval_secs(this.__wbg_ptr, interval_secs);
    }
    /**
     * @returns {Promise<void>}
     */
    stop_all_bridges() {
        const ret = wasm.wasmbroker_stop_all_bridges(this.__wbg_ptr);
        return takeObject(ret);
    }
    stop_sys_topics() {
        wasm.wasmbroker_stop_sys_topics(this.__wbg_ptr);
    }
    /**
     * @param {string} username
     * @param {string} role_name
     * @returns {Promise<boolean>}
     */
    unassign_role(username, role_name) {
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(role_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmbroker_unassign_role(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if the config write lock cannot be acquired.
     * @param {WasmBrokerConfig} new_config
     */
    update_config(new_config) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            _assertClass(new_config, WasmBrokerConfig);
            var ptr0 = new_config.__destroy_into_raw();
            wasm.wasmbroker_update_config(retptr, this.__wbg_ptr, ptr0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {number}
     */
    user_count() {
        const ret = wasm.wasmbroker_user_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * # Errors
     * Returns an error if broker initialization fails.
     * @param {WasmBrokerConfig} wasm_config
     * @returns {WasmBroker}
     */
    static with_config(wasm_config) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            _assertClass(wasm_config, WasmBrokerConfig);
            var ptr0 = wasm_config.__destroy_into_raw();
            wasm.wasmbroker_with_config(retptr, ptr0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
            if (r2) {
                throw takeObject(r1);
            }
            return WasmBroker.__wrap(r0);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
}
if (Symbol.dispose) WasmBroker.prototype[Symbol.dispose] = WasmBroker.prototype.free;

export class WasmBrokerConfig {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmBrokerConfigFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmbrokerconfig_free(ptr, 0);
    }
    /**
     * @param {string} pattern
     */
    add_delta_subscription_pattern(pattern) {
        const ptr0 = passStringToWasm0(pattern, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmbrokerconfig_add_delta_subscription_pattern(this.__wbg_ptr, ptr0, len0);
    }
    clear_delta_subscription_patterns() {
        wasm.wasmbrokerconfig_clear_delta_subscription_patterns(this.__wbg_ptr);
    }
    constructor() {
        const ret = wasm.wasmbrokerconfig_new();
        this.__wbg_ptr = ret >>> 0;
        WasmBrokerConfigFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @param {boolean} value
     */
    set allow_anonymous(value) {
        wasm.wasmbrokerconfig_set_allow_anonymous(this.__wbg_ptr, value);
    }
    /**
     * @param {boolean} value
     */
    set delta_subscription_enabled(value) {
        wasm.wasmbrokerconfig_set_delta_subscription_enabled(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set max_clients(value) {
        wasm.wasmbrokerconfig_set_max_clients(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set max_packet_size(value) {
        wasm.wasmbrokerconfig_set_max_packet_size(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set maximum_qos(value) {
        wasm.wasmbrokerconfig_set_maximum_qos(this.__wbg_ptr, value);
    }
    /**
     * @param {boolean} value
     */
    set retain_available(value) {
        wasm.wasmbrokerconfig_set_retain_available(this.__wbg_ptr, value);
    }
    /**
     * @param {number | null} [value]
     */
    set server_keep_alive_secs(value) {
        wasm.wasmbrokerconfig_set_server_keep_alive_secs(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @param {number} value
     */
    set session_expiry_interval_secs(value) {
        wasm.wasmbrokerconfig_set_session_expiry_interval_secs(this.__wbg_ptr, value);
    }
    /**
     * @param {boolean} value
     */
    set shared_subscription_available(value) {
        wasm.wasmbrokerconfig_set_shared_subscription_available(this.__wbg_ptr, value);
    }
    /**
     * @param {boolean} value
     */
    set subscription_identifier_available(value) {
        wasm.wasmbrokerconfig_set_subscription_identifier_available(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set topic_alias_maximum(value) {
        wasm.wasmbrokerconfig_set_topic_alias_maximum(this.__wbg_ptr, value);
    }
    /**
     * @param {boolean} value
     */
    set wildcard_subscription_available(value) {
        wasm.wasmbrokerconfig_set_wildcard_subscription_available(this.__wbg_ptr, value);
    }
}
if (Symbol.dispose) WasmBrokerConfig.prototype[Symbol.dispose] = WasmBrokerConfig.prototype.free;

export class WasmConnectOptions {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmConnectOptionsFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmconnectoptions_free(ptr, 0);
    }
    /**
     * @param {string} url
     */
    addBackupUrl(url) {
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmconnectoptions_addBackupUrl(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {string} key
     * @param {string} value
     */
    addUserProperty(key, value) {
        const ptr0 = passStringToWasm0(key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.wasmconnectoptions_addUserProperty(this.__wbg_ptr, ptr0, len0, ptr1, len1);
    }
    /**
     * @returns {string | undefined}
     */
    get authenticationMethod() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmconnectoptions_authenticationMethod(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {boolean}
     */
    get cleanStart() {
        const ret = wasm.wasmconnectoptions_cleanStart(this.__wbg_ptr);
        return ret !== 0;
    }
    clearBackupUrls() {
        wasm.wasmconnectoptions_clearBackupUrls(this.__wbg_ptr);
    }
    clearUserProperties() {
        wasm.wasmconnectoptions_clearUserProperties(this.__wbg_ptr);
    }
    clear_will() {
        wasm.wasmconnectoptions_clear_will(this.__wbg_ptr);
    }
    /**
     * @returns {string[]}
     */
    getBackupUrls() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmconnectoptions_getBackupUrls(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayJsValueFromWasm0(r0, r1).slice();
            wasm.__wbindgen_export4(r0, r1 * 4, 4);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {number}
     */
    get keepAlive() {
        const ret = wasm.wasmconnectoptions_keepAlive(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number | undefined}
     */
    get maximumPacketSize() {
        const ret = wasm.wasmconnectoptions_maximumPacketSize(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
    constructor() {
        const ret = wasm.wasmconnectoptions_new();
        this.__wbg_ptr = ret >>> 0;
        WasmConnectOptionsFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @returns {number}
     */
    get protocolVersion() {
        const ret = wasm.wasmconnectoptions_protocolVersion(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number | undefined}
     */
    get receiveMaximum() {
        const ret = wasm.wasmconnectoptions_receiveMaximum(this.__wbg_ptr);
        return ret === 0xFFFFFF ? undefined : ret;
    }
    /**
     * @returns {boolean | undefined}
     */
    get requestProblemInformation() {
        const ret = wasm.wasmconnectoptions_requestProblemInformation(this.__wbg_ptr);
        return ret === 0xFFFFFF ? undefined : ret !== 0;
    }
    /**
     * @returns {boolean | undefined}
     */
    get requestResponseInformation() {
        const ret = wasm.wasmconnectoptions_requestResponseInformation(this.__wbg_ptr);
        return ret === 0xFFFFFF ? undefined : ret !== 0;
    }
    /**
     * @returns {number | undefined}
     */
    get sessionExpiryInterval() {
        const ret = wasm.wasmconnectoptions_sessionExpiryInterval(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
    /**
     * @param {Uint8Array} value
     */
    set authenticationData(value) {
        const ptr0 = passArray8ToWasm0(value, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmconnectoptions_set_authenticationData(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {string | null} [value]
     */
    set authenticationMethod(value) {
        var ptr0 = isLikeNone(value) ? 0 : passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmconnectoptions_set_authenticationMethod(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {boolean} value
     */
    set cleanStart(value) {
        wasm.wasmconnectoptions_set_cleanStart(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set keepAlive(value) {
        wasm.wasmconnectoptions_set_keepAlive(this.__wbg_ptr, value);
    }
    /**
     * @param {number | null} [value]
     */
    set maximumPacketSize(value) {
        wasm.wasmconnectoptions_set_maximumPacketSize(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @param {Uint8Array} value
     */
    set password(value) {
        const ptr0 = passArray8ToWasm0(value, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmconnectoptions_set_password(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {number} value
     */
    set protocolVersion(value) {
        wasm.wasmconnectoptions_set_protocolVersion(this.__wbg_ptr, value);
    }
    /**
     * @param {number | null} [value]
     */
    set receiveMaximum(value) {
        wasm.wasmconnectoptions_set_receiveMaximum(this.__wbg_ptr, isLikeNone(value) ? 0xFFFFFF : value);
    }
    /**
     * @param {boolean | null} [value]
     */
    set requestProblemInformation(value) {
        wasm.wasmconnectoptions_set_requestProblemInformation(this.__wbg_ptr, isLikeNone(value) ? 0xFFFFFF : value ? 1 : 0);
    }
    /**
     * @param {boolean | null} [value]
     */
    set requestResponseInformation(value) {
        wasm.wasmconnectoptions_set_requestResponseInformation(this.__wbg_ptr, isLikeNone(value) ? 0xFFFFFF : value ? 1 : 0);
    }
    /**
     * @param {number | null} [value]
     */
    set sessionExpiryInterval(value) {
        wasm.wasmconnectoptions_set_sessionExpiryInterval(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @param {number | null} [value]
     */
    set topicAliasMaximum(value) {
        wasm.wasmconnectoptions_set_topicAliasMaximum(this.__wbg_ptr, isLikeNone(value) ? 0xFFFFFF : value);
    }
    /**
     * @param {string | null} [value]
     */
    set username(value) {
        var ptr0 = isLikeNone(value) ? 0 : passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmconnectoptions_set_username(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {WasmWillMessage} will
     */
    set_will(will) {
        _assertClass(will, WasmWillMessage);
        var ptr0 = will.__destroy_into_raw();
        wasm.wasmconnectoptions_set_will(this.__wbg_ptr, ptr0);
    }
    /**
     * @returns {number | undefined}
     */
    get topicAliasMaximum() {
        const ret = wasm.wasmconnectoptions_topicAliasMaximum(this.__wbg_ptr);
        return ret === 0xFFFFFF ? undefined : ret;
    }
    /**
     * @returns {string | undefined}
     */
    get username() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmconnectoptions_username(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
}
if (Symbol.dispose) WasmConnectOptions.prototype[Symbol.dispose] = WasmConnectOptions.prototype.free;

export class WasmMessageProperties {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(WasmMessageProperties.prototype);
        obj.__wbg_ptr = ptr;
        WasmMessagePropertiesFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmMessagePropertiesFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmmessageproperties_free(ptr, 0);
    }
    /**
     * @returns {string | undefined}
     */
    get contentType() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmmessageproperties_contentType(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {Uint8Array | undefined}
     */
    get correlationData() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmmessageproperties_correlationData(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getArrayU8FromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {Array<any>}
     */
    getUserProperties() {
        const ret = wasm.wasmmessageproperties_getUserProperties(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @returns {number | undefined}
     */
    get messageExpiryInterval() {
        const ret = wasm.wasmmessageproperties_messageExpiryInterval(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
    /**
     * @returns {boolean | undefined}
     */
    get payloadFormatIndicator() {
        const ret = wasm.wasmmessageproperties_payloadFormatIndicator(this.__wbg_ptr);
        return ret === 0xFFFFFF ? undefined : ret !== 0;
    }
    /**
     * @returns {string | undefined}
     */
    get responseTopic() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmmessageproperties_responseTopic(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {Uint32Array}
     */
    get subscriptionIdentifiers() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmmessageproperties_subscriptionIdentifiers(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayU32FromWasm0(r0, r1).slice();
            wasm.__wbindgen_export4(r0, r1 * 4, 4);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
}
if (Symbol.dispose) WasmMessageProperties.prototype[Symbol.dispose] = WasmMessageProperties.prototype.free;

export class WasmMqttClient {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmMqttClientFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmmqttclient_free(ptr, 0);
    }
    /**
     * # Errors
     * Returns an error if connection fails.
     * @param {string} url
     * @returns {Promise<void>}
     */
    connect(url) {
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_connect(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if connection fails.
     * @param {string} channel_name
     * @returns {Promise<void>}
     */
    connect_broadcast_channel(channel_name) {
        const ptr0 = passStringToWasm0(channel_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_connect_broadcast_channel(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if connection fails.
     * @param {MessagePort} port
     * @returns {Promise<void>}
     */
    connect_message_port(port) {
        const ret = wasm.wasmmqttclient_connect_message_port(this.__wbg_ptr, addHeapObject(port));
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if connection fails.
     * @param {MessagePort} port
     * @param {WasmConnectOptions} config
     * @returns {Promise<void>}
     */
    connect_message_port_with_options(port, config) {
        _assertClass(config, WasmConnectOptions);
        const ret = wasm.wasmmqttclient_connect_message_port_with_options(this.__wbg_ptr, addHeapObject(port), config.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if connection fails.
     * @param {string} url
     * @param {WasmConnectOptions} config
     * @returns {Promise<void>}
     */
    connect_with_options(url, config) {
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        _assertClass(config, WasmConnectOptions);
        const ret = wasm.wasmmqttclient_connect_with_options(this.__wbg_ptr, ptr0, len0, config.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if disconnect fails.
     * @returns {Promise<void>}
     */
    disconnect() {
        const ret = wasm.wasmmqttclient_disconnect(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {boolean} enabled
     */
    enable_auto_reconnect(enabled) {
        wasm.wasmmqttclient_enable_auto_reconnect(this.__wbg_ptr, enabled);
    }
    /**
     * @returns {boolean}
     */
    is_connected() {
        const ret = wasm.wasmmqttclient_is_connected(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @returns {boolean}
     */
    is_reconnecting() {
        const ret = wasm.wasmmqttclient_is_reconnecting(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @param {string} client_id
     */
    constructor(client_id) {
        const ptr0 = passStringToWasm0(client_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_new(ptr0, len0);
        this.__wbg_ptr = ret >>> 0;
        WasmMqttClientFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @param {Function} callback
     */
    on_auth_challenge(callback) {
        wasm.wasmmqttclient_on_auth_challenge(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_connect(callback) {
        wasm.wasmmqttclient_on_connect(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_disconnect(callback) {
        wasm.wasmmqttclient_on_disconnect(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_error(callback) {
        wasm.wasmmqttclient_on_error(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_reconnect_failed(callback) {
        wasm.wasmmqttclient_on_reconnect_failed(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * @param {Function} callback
     */
    on_reconnecting(callback) {
        wasm.wasmmqttclient_on_reconnecting(this.__wbg_ptr, addHeapObject(callback));
    }
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     * @param {string} topic
     * @param {Uint8Array} payload
     * @returns {Promise<void>}
     */
    publish(topic, payload) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(payload, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_publish(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     * @param {string} topic
     * @param {Uint8Array} payload
     * @param {Function} callback
     * @returns {Promise<number>}
     */
    publish_qos1(topic, payload, callback) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(payload, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_publish_qos1(this.__wbg_ptr, ptr0, len0, ptr1, len1, addHeapObject(callback));
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     * @param {string} topic
     * @param {Uint8Array} payload
     * @param {Function} callback
     * @returns {Promise<number>}
     */
    publish_qos2(topic, payload, callback) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(payload, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_publish_qos2(this.__wbg_ptr, ptr0, len0, ptr1, len1, addHeapObject(callback));
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if not connected or publish fails.
     * @param {string} topic
     * @param {Uint8Array} payload
     * @param {WasmPublishOptions} options
     * @returns {Promise<void>}
     */
    publish_with_options(topic, payload, options) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(payload, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        _assertClass(options, WasmPublishOptions);
        const ret = wasm.wasmmqttclient_publish_with_options(this.__wbg_ptr, ptr0, len0, ptr1, len1, options.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if no auth method is set or send fails.
     * @param {Uint8Array} auth_data
     */
    respond_auth(auth_data) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passArray8ToWasm0(auth_data, wasm.__wbindgen_export);
            const len0 = WASM_VECTOR_LEN;
            wasm.wasmmqttclient_respond_auth(retptr, this.__wbg_ptr, ptr0, len0);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {WasmReconnectOptions} options
     */
    set_reconnect_options(options) {
        _assertClass(options, WasmReconnectOptions);
        wasm.wasmmqttclient_set_reconnect_options(this.__wbg_ptr, options.__wbg_ptr);
    }
    /**
     * # Errors
     * Returns an error if not connected or subscribe fails.
     * @param {string} topic
     * @returns {Promise<number>}
     */
    subscribe(topic) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_subscribe(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if not connected or subscribe fails.
     * @param {string} topic
     * @param {Function} callback
     * @returns {Promise<number>}
     */
    subscribe_with_callback(topic, callback) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_subscribe_with_callback(this.__wbg_ptr, ptr0, len0, addHeapObject(callback));
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if not connected or subscribe fails.
     * @param {string} topic
     * @param {Function} callback
     * @param {WasmSubscribeOptions} options
     * @returns {Promise<number>}
     */
    subscribe_with_options(topic, callback, options) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        _assertClass(options, WasmSubscribeOptions);
        const ret = wasm.wasmmqttclient_subscribe_with_options(this.__wbg_ptr, ptr0, len0, addHeapObject(callback), options.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * # Errors
     * Returns an error if not connected or unsubscribe fails.
     * @param {string} topic
     * @returns {Promise<number>}
     */
    unsubscribe(topic) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmmqttclient_unsubscribe(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
}
if (Symbol.dispose) WasmMqttClient.prototype[Symbol.dispose] = WasmMqttClient.prototype.free;

export class WasmPublishOptions {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmPublishOptionsFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmpublishoptions_free(ptr, 0);
    }
    /**
     * @param {string} key
     * @param {string} value
     */
    addUserProperty(key, value) {
        const ptr0 = passStringToWasm0(key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.wasmpublishoptions_addUserProperty(this.__wbg_ptr, ptr0, len0, ptr1, len1);
    }
    clearUserProperties() {
        wasm.wasmpublishoptions_clearUserProperties(this.__wbg_ptr);
    }
    /**
     * @returns {string | undefined}
     */
    get contentType() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmpublishoptions_contentType(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {number | undefined}
     */
    get messageExpiryInterval() {
        const ret = wasm.wasmpublishoptions_messageExpiryInterval(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
    constructor() {
        const ret = wasm.wasmpublishoptions_new();
        this.__wbg_ptr = ret >>> 0;
        WasmPublishOptionsFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @returns {boolean | undefined}
     */
    get payloadFormatIndicator() {
        const ret = wasm.wasmpublishoptions_payloadFormatIndicator(this.__wbg_ptr);
        return ret === 0xFFFFFF ? undefined : ret !== 0;
    }
    /**
     * @returns {number}
     */
    get qos() {
        const ret = wasm.wasmpublishoptions_qos(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {string | undefined}
     */
    get responseTopic() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmpublishoptions_responseTopic(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {boolean}
     */
    get retain() {
        const ret = wasm.wasmpublishoptions_retain(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @param {string | null} [value]
     */
    set contentType(value) {
        var ptr0 = isLikeNone(value) ? 0 : passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmpublishoptions_set_contentType(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {Uint8Array} value
     */
    set correlationData(value) {
        const ptr0 = passArray8ToWasm0(value, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmpublishoptions_set_correlationData(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {number | null} [value]
     */
    set messageExpiryInterval(value) {
        wasm.wasmpublishoptions_set_messageExpiryInterval(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @param {boolean | null} [value]
     */
    set payloadFormatIndicator(value) {
        wasm.wasmpublishoptions_set_payloadFormatIndicator(this.__wbg_ptr, isLikeNone(value) ? 0xFFFFFF : value ? 1 : 0);
    }
    /**
     * @param {number} value
     */
    set qos(value) {
        wasm.wasmpublishoptions_set_qos(this.__wbg_ptr, value);
    }
    /**
     * @param {string | null} [value]
     */
    set responseTopic(value) {
        var ptr0 = isLikeNone(value) ? 0 : passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmpublishoptions_set_responseTopic(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {boolean} value
     */
    set retain(value) {
        wasm.wasmpublishoptions_set_retain(this.__wbg_ptr, value);
    }
    /**
     * @param {number | null} [value]
     */
    set topicAlias(value) {
        wasm.wasmpublishoptions_set_topicAlias(this.__wbg_ptr, isLikeNone(value) ? 0xFFFFFF : value);
    }
    /**
     * @returns {number | undefined}
     */
    get topicAlias() {
        const ret = wasm.wasmpublishoptions_topicAlias(this.__wbg_ptr);
        return ret === 0xFFFFFF ? undefined : ret;
    }
}
if (Symbol.dispose) WasmPublishOptions.prototype[Symbol.dispose] = WasmPublishOptions.prototype.free;

export class WasmReconnectOptions {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(WasmReconnectOptions.prototype);
        obj.__wbg_ptr = ptr;
        WasmReconnectOptionsFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmReconnectOptionsFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmreconnectoptions_free(ptr, 0);
    }
    /**
     * @returns {number}
     */
    get backoffFactor() {
        const ret = wasm.wasmreconnectoptions_backoffFactor(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {WasmReconnectOptions}
     */
    static disabled() {
        const ret = wasm.wasmreconnectoptions_disabled();
        return WasmReconnectOptions.__wrap(ret);
    }
    /**
     * @returns {boolean}
     */
    get enabled() {
        const ret = wasm.wasmreconnectoptions_enabled(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @returns {number}
     */
    get initialDelayMs() {
        const ret = wasm.wasmreconnectoptions_initialDelayMs(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * @returns {number | undefined}
     */
    get maxAttempts() {
        const ret = wasm.wasmreconnectoptions_maxAttempts(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
    /**
     * @returns {number}
     */
    get maxDelayMs() {
        const ret = wasm.wasmreconnectoptions_maxDelayMs(this.__wbg_ptr);
        return ret >>> 0;
    }
    constructor() {
        const ret = wasm.wasmreconnectoptions_new();
        this.__wbg_ptr = ret >>> 0;
        WasmReconnectOptionsFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @param {number} value
     */
    set backoffFactor(value) {
        wasm.wasmreconnectoptions_set_backoffFactor(this.__wbg_ptr, value);
    }
    /**
     * @param {boolean} value
     */
    set enabled(value) {
        wasm.wasmreconnectoptions_set_enabled(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set initialDelayMs(value) {
        wasm.wasmreconnectoptions_set_initialDelayMs(this.__wbg_ptr, value);
    }
    /**
     * @param {number | null} [value]
     */
    set maxAttempts(value) {
        wasm.wasmreconnectoptions_set_maxAttempts(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @param {number} value
     */
    set maxDelayMs(value) {
        wasm.wasmreconnectoptions_set_maxDelayMs(this.__wbg_ptr, value);
    }
}
if (Symbol.dispose) WasmReconnectOptions.prototype[Symbol.dispose] = WasmReconnectOptions.prototype.free;

export class WasmSubscribeOptions {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmSubscribeOptionsFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmsubscribeoptions_free(ptr, 0);
    }
    constructor() {
        const ret = wasm.wasmsubscribeoptions_new();
        this.__wbg_ptr = ret >>> 0;
        WasmSubscribeOptionsFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @returns {boolean}
     */
    get noLocal() {
        const ret = wasm.wasmsubscribeoptions_noLocal(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @returns {number}
     */
    get qos() {
        const ret = wasm.wasmsubscribeoptions_qos(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {boolean}
     */
    get retainAsPublished() {
        const ret = wasm.wasmsubscribeoptions_retainAsPublished(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @returns {number}
     */
    get retainHandling() {
        const ret = wasm.wasmsubscribeoptions_retainHandling(this.__wbg_ptr);
        return ret;
    }
    /**
     * @param {boolean} value
     */
    set noLocal(value) {
        wasm.wasmsubscribeoptions_set_noLocal(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set qos(value) {
        wasm.wasmsubscribeoptions_set_qos(this.__wbg_ptr, value);
    }
    /**
     * @param {boolean} value
     */
    set retainAsPublished(value) {
        wasm.wasmsubscribeoptions_set_retainAsPublished(this.__wbg_ptr, value);
    }
    /**
     * @param {number} value
     */
    set retainHandling(value) {
        wasm.wasmsubscribeoptions_set_retainHandling(this.__wbg_ptr, value);
    }
    /**
     * @param {number | null} [value]
     */
    set subscriptionIdentifier(value) {
        wasm.wasmsubscribeoptions_set_subscriptionIdentifier(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @returns {number | undefined}
     */
    get subscriptionIdentifier() {
        const ret = wasm.wasmsubscribeoptions_subscriptionIdentifier(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
}
if (Symbol.dispose) WasmSubscribeOptions.prototype[Symbol.dispose] = WasmSubscribeOptions.prototype.free;

export class WasmTopicMapping {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmTopicMappingFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmtopicmapping_free(ptr, 0);
    }
    /**
     * @param {string} pattern
     * @param {WasmBridgeDirection} direction
     */
    constructor(pattern, direction) {
        const ptr0 = passStringToWasm0(pattern, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmtopicmapping_new(ptr0, len0, direction);
        this.__wbg_ptr = ret >>> 0;
        WasmTopicMappingFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @param {string | null} [prefix]
     */
    set local_prefix(prefix) {
        var ptr0 = isLikeNone(prefix) ? 0 : passStringToWasm0(prefix, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmtopicmapping_set_local_prefix(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {number} qos
     */
    set qos(qos) {
        wasm.wasmtopicmapping_set_qos(this.__wbg_ptr, qos);
    }
    /**
     * @param {string | null} [prefix]
     */
    set remote_prefix(prefix) {
        var ptr0 = isLikeNone(prefix) ? 0 : passStringToWasm0(prefix, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmtopicmapping_set_remote_prefix(this.__wbg_ptr, ptr0, len0);
    }
}
if (Symbol.dispose) WasmTopicMapping.prototype[Symbol.dispose] = WasmTopicMapping.prototype.free;

export class WasmWillMessage {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmWillMessageFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmwillmessage_free(ptr, 0);
    }
    /**
     * @returns {string | undefined}
     */
    get contentType() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmwillmessage_contentType(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {number | undefined}
     */
    get messageExpiryInterval() {
        const ret = wasm.wasmwillmessage_messageExpiryInterval(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
    /**
     * @param {string} topic
     * @param {Uint8Array} payload
     */
    constructor(topic, payload) {
        const ptr0 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(payload, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmwillmessage_new(ptr0, len0, ptr1, len1);
        this.__wbg_ptr = ret >>> 0;
        WasmWillMessageFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @returns {number}
     */
    get qos() {
        const ret = wasm.wasmwillmessage_qos(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {string | undefined}
     */
    get responseTopic() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmwillmessage_responseTopic(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getStringFromWasm0(r0, r1).slice();
                wasm.__wbindgen_export4(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @returns {boolean}
     */
    get retain() {
        const ret = wasm.wasmwillmessage_retain(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @param {string | null} [value]
     */
    set contentType(value) {
        var ptr0 = isLikeNone(value) ? 0 : passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmwillmessage_set_contentType(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {number | null} [value]
     */
    set messageExpiryInterval(value) {
        wasm.wasmwillmessage_set_messageExpiryInterval(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @param {number} value
     */
    set qos(value) {
        wasm.wasmwillmessage_set_qos(this.__wbg_ptr, value);
    }
    /**
     * @param {string | null} [value]
     */
    set responseTopic(value) {
        var ptr0 = isLikeNone(value) ? 0 : passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.wasmwillmessage_set_responseTopic(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {boolean} value
     */
    set retain(value) {
        wasm.wasmwillmessage_set_retain(this.__wbg_ptr, value);
    }
    /**
     * @param {string} value
     */
    set topic(value) {
        const ptr0 = passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.wasmwillmessage_set_topic(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * @param {number | null} [value]
     */
    set willDelayInterval(value) {
        wasm.wasmwillmessage_set_willDelayInterval(this.__wbg_ptr, isLikeNone(value) ? 0x100000001 : (value) >>> 0);
    }
    /**
     * @returns {string}
     */
    get topic() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.wasmwillmessage_topic(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export4(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * @returns {number | undefined}
     */
    get willDelayInterval() {
        const ret = wasm.wasmwillmessage_willDelayInterval(this.__wbg_ptr);
        return ret === 0x100000001 ? undefined : ret;
    }
}
if (Symbol.dispose) WasmWillMessage.prototype[Symbol.dispose] = WasmWillMessage.prototype.free;

function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_debug_string_0bc8482c6e3508ae: function(arg0, arg1) {
            const ret = debugString(getObject(arg1));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_0095a73b8b156f76: function(arg0) {
            const ret = typeof(getObject(arg0)) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_undefined_9e4d92534c42d778: function(arg0) {
            const ret = getObject(arg0) === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_8ff4255516ccad3e: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_throw_be289d5034ed271b: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_d9b87ff7982e3b21: function(arg0) {
            getObject(arg0)._wbg_cb_unref();
        },
        __wbg_addEventListener_3acb0aad4483804c: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            getObject(arg0).addEventListener(getStringFromWasm0(arg1, arg2), getObject(arg3));
        }, arguments); },
        __wbg_buffer_26d0910f3a5bc899: function(arg0) {
            const ret = getObject(arg0).buffer;
            return addHeapObject(ret);
        },
        __wbg_call_389efe28435a9388: function() { return handleError(function (arg0, arg1) {
            const ret = getObject(arg0).call(getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_call_4708e0c13bdc8e95: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).call(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_call_812d25f1510c13c8: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = getObject(arg0).call(getObject(arg1), getObject(arg2), getObject(arg3));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_call_e8c868596c950cf6: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            const ret = getObject(arg0).call(getObject(arg1), getObject(arg2), getObject(arg3), getObject(arg4));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_clearTimeout_5a54f8841c30079a: function(arg0) {
            const ret = clearTimeout(takeObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_close_1d08eaf57ed325c0: function() { return handleError(function (arg0) {
            getObject(arg0).close();
        }, arguments); },
        __wbg_close_36e3b6eed1f8c59d: function(arg0) {
            getObject(arg0).close();
        },
        __wbg_close_fad2f0ee451926ed: function(arg0) {
            getObject(arg0).close();
        },
        __wbg_data_5330da50312d0bc1: function(arg0) {
            const ret = getObject(arg0).data;
            return addHeapObject(ret);
        },
        __wbg_error_7534b8e9a36f1ab4: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_export4(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_error_9a7fe3f932034cde: function(arg0) {
            console.error(getObject(arg0));
        },
        __wbg_from_bddd64e7d5ff6941: function(arg0) {
            const ret = Array.from(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_getRandomValues_1c61fac11405ffdc: function() { return handleError(function (arg0, arg1) {
            globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_get_9b94d73e6221f75c: function(arg0, arg1) {
            const ret = getObject(arg0)[arg1 >>> 0];
            return addHeapObject(ret);
        },
        __wbg_instanceof_ArrayBuffer_c367199e2fa2aa04: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof ArrayBuffer;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_ed49b2db8df90359: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_length_32ed9a279acd054c: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_length_35a7bace40f36eac: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_log_6b5ca2e6124b2808: function(arg0) {
            console.log(getObject(arg0));
        },
        __wbg_new_361308b2356cecd0: function() {
            const ret = new Object();
            return addHeapObject(ret);
        },
        __wbg_new_3eb36ae241fe6f44: function() {
            const ret = new Array();
            return addHeapObject(ret);
        },
        __wbg_new_6f0524fbfa300c47: function() { return handleError(function () {
            const ret = new MessageChannel();
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_8a6f238a6ece86ea: function() {
            const ret = new Error();
            return addHeapObject(ret);
        },
        __wbg_new_afb8dbb951819ab7: function() { return handleError(function (arg0, arg1) {
            const ret = new BroadcastChannel(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_new_b5d9e2fb389fef91: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return __wasm_bindgen_func_elem_3612(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return addHeapObject(ret);
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_new_dd2b680c8bf6ae29: function(arg0) {
            const ret = new Uint8Array(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_new_from_slice_a3d2629dc1826784: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_no_args_1c7c842f08d00ebb: function(arg0, arg1) {
            const ret = new Function(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_with_str_8406051fb31dddaa: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = new WebSocket(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_now_2c95c9de01293173: function(arg0) {
            const ret = getObject(arg0).now();
            return ret;
        },
        __wbg_now_a3af9a2f4bbaa4d1: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_now_ebffdf7e580f210d: function(arg0) {
            const ret = getObject(arg0).now();
            return ret;
        },
        __wbg_performance_06f12ba62483475d: function(arg0) {
            const ret = getObject(arg0).performance;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_performance_7a3ffd0b17f663ad: function(arg0) {
            const ret = getObject(arg0).performance;
            return addHeapObject(ret);
        },
        __wbg_port1_6251ddc5cf5c9287: function(arg0) {
            const ret = getObject(arg0).port1;
            return addHeapObject(ret);
        },
        __wbg_port2_b2a294b0ede1e13c: function(arg0) {
            const ret = getObject(arg0).port2;
            return addHeapObject(ret);
        },
        __wbg_postMessage_46eeeef39934b448: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).postMessage(getObject(arg1));
        }, arguments); },
        __wbg_postMessage_6962a8f13ab51b6a: function() { return handleError(function (arg0, arg1) {
            getObject(arg0).postMessage(getObject(arg1));
        }, arguments); },
        __wbg_prototypesetcall_bdcdcc5842e4d77d: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), getObject(arg2));
        },
        __wbg_push_8ffdcb2063340ba5: function(arg0, arg1) {
            const ret = getObject(arg0).push(getObject(arg1));
            return ret;
        },
        __wbg_queueMicrotask_0aa0a927f78f5d98: function(arg0) {
            const ret = getObject(arg0).queueMicrotask;
            return addHeapObject(ret);
        },
        __wbg_queueMicrotask_5bb536982f78a56f: function(arg0) {
            queueMicrotask(getObject(arg0));
        },
        __wbg_random_912284dbf636f269: function() {
            const ret = Math.random();
            return ret;
        },
        __wbg_resolve_002c4b7d9d8f6b64: function(arg0) {
            const ret = Promise.resolve(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_send_542f95dea2df7994: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).send(getArrayU8FromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_setTimeout_db2dbaeefb6f39c7: function() { return handleError(function (arg0, arg1) {
            const ret = setTimeout(getObject(arg0), arg1);
            return addHeapObject(ret);
        }, arguments); },
        __wbg_setTimeout_eff32631ea138533: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = getObject(arg0).setTimeout(getObject(arg1), arg2);
            return ret;
        }, arguments); },
        __wbg_set_6cb8631f80447a67: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(getObject(arg0), getObject(arg1), getObject(arg2));
            return ret;
        }, arguments); },
        __wbg_set_binaryType_5bbf62e9f705dc1a: function(arg0, arg1) {
            getObject(arg0).binaryType = __wbindgen_enum_BinaryType[arg1];
        },
        __wbg_set_onclose_d382f3e2c2b850eb: function(arg0, arg1) {
            getObject(arg0).onclose = getObject(arg1);
        },
        __wbg_set_onerror_377f18bf4569bf85: function(arg0, arg1) {
            getObject(arg0).onerror = getObject(arg1);
        },
        __wbg_set_onmessage_0e1ffb1c0d91d2ad: function(arg0, arg1) {
            getObject(arg0).onmessage = getObject(arg1);
        },
        __wbg_set_onmessage_2114aa5f4f53051e: function(arg0, arg1) {
            getObject(arg0).onmessage = getObject(arg1);
        },
        __wbg_set_onmessage_41e84d56e3597e90: function(arg0, arg1) {
            getObject(arg0).onmessage = getObject(arg1);
        },
        __wbg_set_onopen_b7b52d519d6c0f11: function(arg0, arg1) {
            getObject(arg0).onopen = getObject(arg1);
        },
        __wbg_stack_0ed75d68575b0f3c: function(arg0, arg1) {
            const ret = getObject(arg1).stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_start_ffb4b426b1e661bd: function(arg0) {
            getObject(arg0).start();
        },
        __wbg_static_accessor_GLOBAL_12837167ad935116: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_e628e89ab3b1c95f: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_SELF_a621d3dfbb60d0ce: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_WINDOW_f8727f0cf888e0bd: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_then_0d9fe2c7b1857d32: function(arg0, arg1, arg2) {
            const ret = getObject(arg0).then(getObject(arg1), getObject(arg2));
            return addHeapObject(ret);
        },
        __wbg_then_b9e7b3b5f1a9e1b5: function(arg0, arg1) {
            const ret = getObject(arg0).then(getObject(arg1));
            return addHeapObject(ret);
        },
        __wbg_warn_f7ae1b2e66ccb930: function(arg0) {
            console.warn(getObject(arg0));
        },
        __wbg_wasmmessageproperties_new: function(arg0) {
            const ret = WasmMessageProperties.__wrap(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 358, function: Function { arguments: [], shim_idx: 359, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_2267, __wasm_bindgen_func_elem_2284);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 369, function: Function { arguments: [Externref], shim_idx: 370, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_2318, __wasm_bindgen_func_elem_2334);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 7, function: Function { arguments: [NamedExternref("CloseEvent")], shim_idx: 8, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_125, __wasm_bindgen_func_elem_875);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 7, function: Function { arguments: [NamedExternref("ErrorEvent")], shim_idx: 8, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_125, __wasm_bindgen_func_elem_875);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 7, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 8, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.__wasm_bindgen_func_elem_125, __wasm_bindgen_func_elem_875);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000006: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000007: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000008: function(arg0, arg1) {
            var v0 = getArrayJsValueFromWasm0(arg0, arg1).slice();
            wasm.__wbindgen_export4(arg0, arg1 * 4, 4);
            // Cast intrinsic for `Vector(NamedExternref("string")) -> Externref`.
            const ret = v0;
            return addHeapObject(ret);
        },
        __wbindgen_object_clone_ref: function(arg0) {
            const ret = getObject(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
    };
    return {
        __proto__: null,
        "./mqtt5_wasm_bg.js": import0,
    };
}

function __wasm_bindgen_func_elem_2284(arg0, arg1) {
    wasm.__wasm_bindgen_func_elem_2284(arg0, arg1);
}

function __wasm_bindgen_func_elem_2334(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_2334(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_875(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_875(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_3612(arg0, arg1, arg2, arg3) {
    wasm.__wasm_bindgen_func_elem_3612(arg0, arg1, addHeapObject(arg2), addHeapObject(arg3));
}


const __wbindgen_enum_BinaryType = ["blob", "arraybuffer"];
const WasmBridgeConfigFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmbridgeconfig_free(ptr >>> 0, 1));
const WasmBrokerFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmbroker_free(ptr >>> 0, 1));
const WasmBrokerConfigFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmbrokerconfig_free(ptr >>> 0, 1));
const WasmConnectOptionsFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmconnectoptions_free(ptr >>> 0, 1));
const WasmMessagePropertiesFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmmessageproperties_free(ptr >>> 0, 1));
const WasmMqttClientFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmmqttclient_free(ptr >>> 0, 1));
const WasmPublishOptionsFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmpublishoptions_free(ptr >>> 0, 1));
const WasmReconnectOptionsFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmreconnectoptions_free(ptr >>> 0, 1));
const WasmSubscribeOptionsFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmsubscribeoptions_free(ptr >>> 0, 1));
const WasmTopicMappingFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmtopicmapping_free(ptr >>> 0, 1));
const WasmWillMessageFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmwillmessage_free(ptr >>> 0, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

function _assertClass(instance, klass) {
    if (!(instance instanceof klass)) {
        throw new Error(`expected instance of ${klass.name}`);
    }
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => state.dtor(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function dropObject(idx) {
    if (idx < 132) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayJsValueFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    const mem = getDataViewMemory0();
    const result = [];
    for (let i = ptr; i < ptr + 4 * len; i += 4) {
        result.push(takeObject(mem.getUint32(i, true)));
    }
    return result;
}

function getArrayU32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint32ArrayMemory0 = null;
function getUint32ArrayMemory0() {
    if (cachedUint32ArrayMemory0 === null || cachedUint32ArrayMemory0.byteLength === 0) {
        cachedUint32ArrayMemory0 = new Uint32Array(wasm.memory.buffer);
    }
    return cachedUint32ArrayMemory0;
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        wasm.__wbindgen_export3(addHeapObject(e));
    }
}

let heap = new Array(128).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, dtor, f) {
    const state = { a: arg0, b: arg1, cnt: 1, dtor };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            state.dtor(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('mqtt5_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
