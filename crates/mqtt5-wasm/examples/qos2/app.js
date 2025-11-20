import init, { WasmMqttClient } from './pkg/mqtt5_wasm.js';

let client = null;
let isConnected = false;

function updateStatus(status) {
    const statusEl = document.getElementById('connection-status');
    statusEl.className = `status ${status}`;
    statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
}

function toggleControls(connected) {
    isConnected = connected;

    document.getElementById('broker-url').disabled = connected;
    document.getElementById('client-id').disabled = connected;
    document.getElementById('connect-btn').disabled = connected;
    document.getElementById('disconnect-btn').disabled = !connected;

    document.getElementById('subscribe-topic').disabled = !connected;
    document.querySelectorAll('#subscribe-form button').forEach(btn => btn.disabled = !connected);

    document.getElementById('publish-topic').disabled = !connected;
    document.getElementById('publish-payload').disabled = !connected;
    document.querySelectorAll('#publish-form button').forEach(btn => btn.disabled = !connected);
}

function generateClientId() {
    return `qos2-test-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

function addFlowMessage(packetId, message, isError = false) {
    const flowEl = document.getElementById('qos2-flow-messages');

    const emptyState = flowEl.querySelector('.empty-state');
    if (emptyState) {
        emptyState.remove();
    }

    const flowDiv = document.createElement('div');
    flowDiv.className = `flow-item ${isError ? 'error' : 'success'}`;

    const now = new Date();
    const timeStr = now.toLocaleTimeString();

    flowDiv.innerHTML = `
        <div><strong>Packet ${packetId}:</strong> ${message}</div>
        <div class="timestamp">${timeStr}</div>
    `;

    flowEl.insertBefore(flowDiv, flowEl.firstChild);

    if (flowEl.children.length > 20) {
        flowEl.lastChild.remove();
    }
}

function addMessage(topic, payload, qos = 0) {
    const messagesEl = document.getElementById('messages');

    const emptyState = messagesEl.querySelector('.empty-state');
    if (emptyState) {
        emptyState.remove();
    }

    const messageDiv = document.createElement('div');
    messageDiv.className = qos === 2 ? 'message-item qos2' : 'message-item';

    const now = new Date();
    const timeStr = now.toLocaleTimeString();

    let payloadStr;
    if (payload instanceof Uint8Array) {
        payloadStr = new TextDecoder().decode(payload);
    } else if (typeof payload === 'string') {
        payloadStr = payload;
    } else {
        payloadStr = new TextDecoder().decode(payload);
    }

    messageDiv.innerHTML = `
        <div class="message-header">
            <span class="message-topic">${topic}</span>
            <span class="timestamp">${timeStr} - QoS ${qos}</span>
        </div>
        <div class="message-payload">${payloadStr}</div>
    `;

    messagesEl.insertBefore(messageDiv, messagesEl.firstChild);

    if (messagesEl.children.length > 50) {
        messagesEl.lastChild.remove();
    }
}

function showError(message) {
    alert(`Error: ${message}`);
    console.error(message);
}

async function handleConnect(e) {
    e.preventDefault();

    const brokerUrl = document.getElementById('broker-url').value.trim();
    if (!brokerUrl) {
        showError('Please enter a broker URL');
        return;
    }

    let clientId = document.getElementById('client-id').value.trim();
    if (!clientId) {
        clientId = generateClientId();
        document.getElementById('client-id').value = clientId;
    }

    try {
        updateStatus('connecting');
        console.log('Connecting to:', brokerUrl, 'with client ID:', clientId);

        client = new WasmMqttClient(clientId);
        console.log('WASM client created');

        client.on_connect((reasonCode, sessionPresent) => {
            console.log('onConnect callback:', reasonCode, sessionPresent);
            updateStatus('connected');
            toggleControls(true);
            addMessage('system', `Connected (reason: ${reasonCode}, session: ${sessionPresent})`, 0);
        });

        client.on_disconnect(() => {
            console.log('onDisconnect callback');
            updateStatus('disconnected');
            toggleControls(false);
            addMessage('system', 'Disconnected from broker', 0);
        });

        client.on_error((error) => {
            console.error('onError callback:', error);
            addMessage('system', `Error: ${error}`, 0);
        });

        await client.connect(brokerUrl);
        console.log('Connection initiated');

    } catch (error) {
        console.error('Connection error:', error);
        updateStatus('disconnected');
        showError(`Connection failed: ${error}`);
        client = null;
    }
}

async function handleDisconnect() {
    console.log('Disconnecting...');
    if (!client) {
        return;
    }

    try {
        await client.disconnect();
    } catch (error) {
        console.error('Disconnect error:', error);
        showError(`Disconnect failed: ${error}`);
    } finally {
        client = null;
    }
}

async function handleSubscribe(e) {
    e.preventDefault();

    if (!client || !isConnected) {
        showError('Not connected to broker');
        return;
    }

    const topic = document.getElementById('subscribe-topic').value.trim();
    if (!topic) {
        showError('Please enter a topic filter');
        return;
    }

    try {
        console.log('Subscribing to topic:', topic);
        const packetId = await client.subscribe_with_callback(topic, (receivedTopic, payload) => {
            console.log('QoS 2 message received:', receivedTopic, payload);
            addMessage(receivedTopic, payload, 2);
        });

        console.log('Subscribed, packet_id:', packetId);
        addMessage('system', `Subscribed to ${topic} (packet_id: ${packetId})`, 0);

    } catch (error) {
        console.error('Subscribe error:', error);
        showError(`Subscribe failed: ${error}`);
    }
}

async function handlePublish(e) {
    e.preventDefault();

    if (!client || !isConnected) {
        showError('Not connected to broker');
        return;
    }

    const topic = document.getElementById('publish-topic').value.trim();
    const payload = document.getElementById('publish-payload').value;

    if (!topic) {
        showError('Please enter a topic');
        return;
    }

    try {
        const encoder = new TextEncoder();
        const payloadBytes = encoder.encode(payload);

        console.log('Publishing QoS 2, topic:', topic, 'payload bytes:', payloadBytes.length);

        const packetId = await client.publish_qos2(topic, payloadBytes, (reasonCode) => {
            console.log('QoS 2 flow completed for packet', packetId, 'reason code:', reasonCode);

            if (reasonCode === 0) {
                addFlowMessage(packetId, 'PUBCOMP received - Flow completed successfully', false);
            } else if (reasonCode === 'Timeout') {
                addFlowMessage(packetId, 'Timeout waiting for PUBCOMP', true);
            } else {
                addFlowMessage(packetId, `Flow completed with reason code: ${reasonCode}`, reasonCode !== 0);
            }
        });

        console.log('QoS 2 publish initiated, packet_id:', packetId);
        addFlowMessage(packetId, 'PUBLISH sent - Waiting for PUBREC', false);
        addMessage(topic, payloadBytes, 2);

        document.getElementById('publish-payload').value = '';

    } catch (error) {
        console.error('Publish error:', error);
        showError(`Publish failed: ${error}`);
    }
}

function clearMessages() {
    const messagesEl = document.getElementById('messages');
    messagesEl.innerHTML = '<div class="empty-state">No messages yet</div>';
}

async function initApp() {
    try {
        await init();
        console.log('WASM module initialized');

        document.getElementById('connect-form').addEventListener('submit', handleConnect);
        document.getElementById('disconnect-btn').addEventListener('click', handleDisconnect);
        document.getElementById('subscribe-form').addEventListener('submit', handleSubscribe);
        document.getElementById('publish-form').addEventListener('submit', handlePublish);
        document.getElementById('clear-messages').addEventListener('click', clearMessages);

        console.log('Event listeners attached');
        console.log('QoS 2 testing app ready');

    } catch (error) {
        console.error('Initialization error:', error);
        showError(`Failed to initialize: ${error}`);
    }
}

initApp();
