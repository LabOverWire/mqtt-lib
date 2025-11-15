import init, { WasmMqttClient } from './pkg/mqtt5.js';

let client = null;
let controlChannel = null;
let isConnected = false;
let isBroker = false;
let tabId = null;
const subscriptions = new Set();
const activeTabs = new Map();
const brokerState = {
    subscriptions: new Map(),
    retained: new Map()
};

function generateTabId() {
    return `tab-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

function generateClientId() {
    return `mqtt5-bc-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

function updateRoleStatus(role) {
    const roleEl = document.getElementById('role-status');
    roleEl.textContent = role;
    roleEl.className = `status-value role-${role.toLowerCase()}`;
}

function updateChannelStatus(status) {
    const statusEl = document.getElementById('channel-status');
    statusEl.className = `status ${status}`;
    statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
}

function toggleControls(connected) {
    isConnected = connected;

    document.getElementById('channel-name').disabled = connected;
    document.getElementById('client-id').disabled = connected;
    document.getElementById('connect-btn').disabled = connected;
    document.getElementById('disconnect-btn').disabled = !connected;

    document.getElementById('subscribe-topic').disabled = !connected;
    document.querySelectorAll('#subscribe-form button').forEach(btn => btn.disabled = !connected);

    document.getElementById('publish-topic').disabled = !connected;
    document.getElementById('publish-payload').disabled = !connected;
    document.querySelectorAll('#publish-form button').forEach(btn => btn.disabled = !connected);
}

function addMessage(topic, payload, direction = 'received', source = '') {
    const messagesEl = document.getElementById('messages');

    const emptyState = messagesEl.querySelector('.empty-state');
    if (emptyState) {
        emptyState.remove();
    }

    const messageDiv = document.createElement('div');
    messageDiv.className = 'message-item';

    const now = new Date();
    const timeStr = now.toLocaleTimeString();

    const payloadStr = typeof payload === 'string' ? payload : new TextDecoder().decode(payload);
    const sourceStr = source ? ` from ${source}` : '';

    messageDiv.innerHTML = `
        <div class="message-header">
            <span class="message-topic">${topic}</span>
            <span class="message-time">${timeStr} (${direction}${sourceStr})</span>
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
            removeSubscription(topic);
        });
    });
}

function updateActiveTabsList() {
    const tabsEl = document.getElementById('active-tabs');

    if (activeTabs.size === 0) {
        tabsEl.innerHTML = '<div class="empty-state">No other tabs connected</div>';
        return;
    }

    tabsEl.innerHTML = '';
    activeTabs.forEach((tabInfo, id) => {
        const tabDiv = document.createElement('div');
        tabDiv.className = 'tab-item';
        const role = tabInfo.isBroker ? '👑 Broker' : '📱 Client';
        tabDiv.innerHTML = `
            <span class="tab-id">${id}</span>
            <span class="tab-role">${role}</span>
        `;
        tabsEl.appendChild(tabDiv);
    });
}

function showError(message) {
    alert(`Error: ${message}`);
    console.error(message);
}

function handleControlMessage(event) {
    const { type, tabId: senderId, ...data } = event.data;

    if (type === 'ANNOUNCE') {
        activeTabs.set(senderId, {
            isBroker: data.isBroker,
            timestamp: Date.now()
        });
        updateActiveTabsList();
        console.log(`Tab announced: ${senderId} (${data.isBroker ? 'Broker' : 'Client'})`);
    }

    if (type === 'GOODBYE') {
        activeTabs.delete(senderId);
        updateActiveTabsList();
        console.log(`Tab disconnected: ${senderId}`);
    }
}

function announcePresence() {
    if (controlChannel) {
        controlChannel.postMessage({
            type: 'ANNOUNCE',
            tabId,
            isBroker,
            timestamp: Date.now()
        });
    }
}

async function becomeBroker() {
    isBroker = true;
    updateRoleStatus('Broker');
    console.log('This tab is the BROKER');

    addMessage('system', 'This tab is acting as the MQTT broker', 'system');
}

async function becomeClient(channelName) {
    isBroker = false;
    updateRoleStatus('Client');
    console.log('This tab is a CLIENT');

    try {
        await client.connect_broadcast_channel(channelName);
        console.log('Connected to broadcast channel:', channelName);
    } catch (error) {
        throw new Error(`Failed to connect to channel: ${error}`);
    }
}

async function handleConnect(e) {
    e.preventDefault();

    const channelName = document.getElementById('channel-name').value.trim();
    if (!channelName) {
        showError('Please enter a channel name');
        return;
    }

    let clientId = document.getElementById('client-id').value.trim();
    if (!clientId) {
        clientId = generateClientId();
        document.getElementById('client-id').value = clientId;
    }

    try {
        updateChannelStatus('connecting');
        console.log('Connecting to channel:', channelName);

        client = new WasmMqttClient(clientId);

        controlChannel = new BroadcastChannel(`${channelName}-control`);
        controlChannel.addEventListener('message', handleControlMessage);

        await new Promise(resolve => setTimeout(resolve, 100));

        if (activeTabs.size === 0) {
            await becomeBroker();
        } else {
            const hasBroker = Array.from(activeTabs.values()).some(tab => tab.isBroker);
            if (!hasBroker) {
                await becomeBroker();
            } else {
                await becomeClient(channelName);
            }
        }

        announcePresence();
        setInterval(announcePresence, 5000);

        updateChannelStatus('connected');
        toggleControls(true);

        addMessage('system', `Connected to channel "${channelName}" as ${isBroker ? 'BROKER' : 'CLIENT'}`, 'system');

    } catch (error) {
        console.error('Connection error:', error);
        updateChannelStatus('disconnected');
        showError(`Connection failed: ${error}`);
        client = null;
    }
}

async function handleDisconnect() {
    if (!client) return;

    try {
        if (controlChannel) {
            controlChannel.postMessage({
                type: 'GOODBYE',
                tabId
            });
            controlChannel.close();
            controlChannel = null;
        }

        if (!isBroker && client) {
            await client.disconnect();
        }

        updateChannelStatus('disconnected');
        toggleControls(false);
        subscriptions.clear();
        updateSubscriptionsList();

        addMessage('system', 'Disconnected from channel', 'system');

        isBroker = false;
        updateRoleStatus('Disconnected');

    } catch (error) {
        showError(`Disconnect failed: ${error}`);
    } finally {
        client = null;
    }
}

async function handleSubscribe(e) {
    e.preventDefault();

    if (!isConnected) {
        showError('Not connected to channel');
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
        if (isBroker) {
            if (!brokerState.subscriptions.has(topic)) {
                brokerState.subscriptions.set(topic, new Set());
            }
            brokerState.subscriptions.get(topic).add(tabId);
        } else {
            const packetId = await client.subscribe(topic);
            console.log(`Subscribed to ${topic}, packet_id: ${packetId}`);
        }

        addSubscription(topic);
        addMessage('system', `Subscribed to ${topic}`, 'system');
        document.getElementById('subscribe-topic').value = '';

    } catch (error) {
        showError(`Subscribe failed: ${error}`);
    }
}

async function handlePublish(e) {
    e.preventDefault();

    if (!isConnected) {
        showError('Not connected to channel');
        return;
    }

    const topic = document.getElementById('publish-topic').value.trim();
    const payload = document.getElementById('publish-payload').value;

    if (!topic) {
        showError('Please enter a topic');
        return;
    }

    try {
        if (isBroker) {
            addMessage(topic, payload, 'published (as broker)');
        } else {
            const encoder = new TextEncoder();
            const payloadBytes = encoder.encode(payload);
            await client.publish(topic, payloadBytes);
            addMessage(topic, payload, 'sent');
        }

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

        tabId = generateTabId();
        document.getElementById('tab-id').textContent = tabId;

        document.getElementById('connect-form').addEventListener('submit', handleConnect);
        document.getElementById('disconnect-btn').addEventListener('click', handleDisconnect);
        document.getElementById('subscribe-form').addEventListener('submit', handleSubscribe);
        document.getElementById('publish-form').addEventListener('submit', handlePublish);
        document.getElementById('clear-messages').addEventListener('click', clearMessages);

        const messagesEl = document.getElementById('messages');
        messagesEl.innerHTML = '<div class="empty-state">No messages yet</div>';

        updateSubscriptionsList();
        updateActiveTabsList();

        console.log('Application ready, Tab ID:', tabId);

    } catch (error) {
        console.error('Failed to initialize WASM:', error);
        showError(`Failed to initialize WASM module: ${error}`);
    }
}

initApp();
