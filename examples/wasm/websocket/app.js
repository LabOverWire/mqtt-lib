import init, { WasmMqttClient } from './pkg/mqtt5.js';

let client = null;
let isConnected = false;
const subscriptions = new Set();

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
    return `mqtt5-wasm-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

function addMessage(topic, payload, direction = 'received') {
    const messagesEl = document.getElementById('messages');

    const emptyState = messagesEl.querySelector('.empty-state');
    if (emptyState) {
        emptyState.remove();
    }

    const messageDiv = document.createElement('div');
    messageDiv.className = 'message-item';

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
            <span class="message-time">${timeStr} (${direction})</span>
        </div>
        <div class="message-payload">${payloadStr}</div>
    `;

    messagesEl.insertBefore(messageDiv, messagesEl.firstChild);

    if (messagesEl.children.length > 50) {
        messagesEl.lastChild.remove();
    }
}

function addSubscription(topic) {
    subscriptions.add(topic);
    updateSubscriptionsList();
}

function removeSubscription(topic) {
    subscriptions.delete(topic);
    updateSubscriptionsList();
}

function updateSubscriptionsList() {
    const subsEl = document.getElementById('subscriptions');

    if (subscriptions.size === 0) {
        subsEl.innerHTML = '<div class="empty-state">No active subscriptions</div>';
        return;
    }

    subsEl.innerHTML = '';
    subscriptions.forEach(topic => {
        const subDiv = document.createElement('div');
        subDiv.className = 'subscription-item';
        subDiv.innerHTML = `
            <span class="subscription-topic">${topic}</span>
            <button class="btn btn-secondary btn-sm unsubscribe-btn" data-topic="${topic}">Unsubscribe</button>
        `;
        subsEl.appendChild(subDiv);
    });

    document.querySelectorAll('.unsubscribe-btn').forEach(btn => {
        btn.addEventListener('click', async (e) => {
            const topic = e.target.dataset.topic;
            try {
                await client.unsubscribe(topic);
                removeSubscription(topic);
                addMessage('system', `Unsubscribed from ${topic}`, 'system');
            } catch (error) {
                showError(`Unsubscribe failed: ${error}`);
            }
        });
    });
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
            addMessage('system', `Connected to ${brokerUrl} (reason: ${reasonCode}, session: ${sessionPresent})`, 'system');
        });

        client.on_disconnect(() => {
            console.log('onDisconnect callback');
            updateStatus('disconnected');
            toggleControls(false);
            subscriptions.clear();
            updateSubscriptionsList();
            addMessage('system', 'Disconnected from broker', 'system');
        });

        client.on_error((error) => {
            console.error('onError callback:', error);
            addMessage('system', `Error: ${error}`, 'error');
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
    if (!client) return;

    try {
        await client.disconnect();
    } catch (error) {
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

    if (subscriptions.has(topic)) {
        showError('Already subscribed to this topic');
        return;
    }

    try {
        const packetId = await client.subscribe_with_callback(topic, (receivedTopic, payload) => {
            console.log('Message received:', receivedTopic, payload);
            addMessage(receivedTopic, payload, 'received');
        });

        addSubscription(topic);
        addMessage('system', `Subscribed to ${topic} (packet_id: ${packetId})`, 'system');
        document.getElementById('subscribe-topic').value = '';

    } catch (error) {
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

        await client.publish(topic, payloadBytes);

        addMessage(topic, payloadBytes, 'sent');
        document.getElementById('publish-payload').value = '';

    } catch (error) {
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

        const messagesEl = document.getElementById('messages');
        messagesEl.innerHTML = '<div class="empty-state">No messages yet</div>';

        updateSubscriptionsList();

        console.log('Application ready');

    } catch (error) {
        console.error('Failed to initialize WASM:', error);
        alert(`Failed to initialize WASM module: ${error}`);
    }
}

initApp();
