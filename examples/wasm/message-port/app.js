let serviceWorker = null;
let messageChannel = null;
let isConnected = false;
let tabId = null;
const subscriptions = new Set();

function generateTabId() {
    return `tab-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

function generateClientId() {
    return `mqtt5-mp-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

function updateSWStatus(status) {
    const statusEl = document.getElementById('sw-status');
    statusEl.className = `status ${status}`;
    statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1).replace('-', ' ');
}

function updateStatus(status) {
    const statusEl = document.getElementById('connection-status');
    statusEl.className = `status ${status}`;
    statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
}

function toggleControls(connected) {
    isConnected = connected;

    document.getElementById('broker-url').disabled = connected;
    document.getElementById('client-id').disabled = connected;
    document.getElementById('connect-btn').disabled = connected || !serviceWorker;
    document.getElementById('disconnect-btn').disabled = !connected;

    document.getElementById('subscribe-topic').disabled = !connected;
    document.querySelectorAll('#subscribe-form button').forEach(btn => btn.disabled = !connected);

    document.getElementById('publish-topic').disabled = !connected;
    document.getElementById('publish-payload').disabled = !connected;
    document.querySelectorAll('#publish-form button').forEach(btn => btn.disabled = !connected);
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

    const payloadStr = typeof payload === 'string' ? payload : new TextDecoder().decode(payload);

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
            removeSubscription(topic);
        });
    });
}

function showError(message) {
    alert(`Error: ${message}`);
    console.error(message);
}

function handleSWMessage(event) {
    const { type, ...data } = event.data;

    console.log('Received from SW:', type, data);

    switch (type) {
        case 'WASM_READY':
            console.log('WASM module ready in Service Worker');
            document.getElementById('connect-btn').disabled = false;
            break;

        case 'CONNECTED':
            updateStatus('connected');
            toggleControls(true);
            addMessage('system', `Connected to broker (Tab ${tabId})`, 'system');
            break;

        case 'PUBLISHED':
            addMessage(data.topic, document.getElementById('publish-payload').value, 'sent');
            document.getElementById('publish-payload').value = '';
            break;

        case 'SUBSCRIBED':
            addSubscription(data.topic);
            addMessage('system', `Subscribed to ${data.topic} (packet_id: ${data.packetId})`, 'system');
            document.getElementById('subscribe-topic').value = '';
            break;

        case 'ERROR':
            console.error('Service Worker error:', data.error);
            showError(data.error);
            updateStatus('error');
            break;
    }
}

async function registerServiceWorker() {
    try {
        const registration = await navigator.serviceWorker.register('./service-worker.js', {
            type: 'module'
        });

        console.log('Service Worker registered:', registration);
        updateSWStatus('registered');

        await navigator.serviceWorker.ready;
        serviceWorker = registration.active || registration.waiting || registration.installing;

        console.log('Service Worker ready');
        updateSWStatus('ready');

        const channel = new MessageChannel();
        messageChannel = channel.port1;
        messageChannel.addEventListener('message', handleSWMessage);
        messageChannel.start();

        serviceWorker.postMessage({
            type: 'INIT_WASM',
            data: {
                wasmUrl: './pkg/mqtt5_bg.wasm'
            }
        }, [channel.port2]);

    } catch (error) {
        console.error('Service Worker registration failed:', error);
        updateSWStatus('error');
        showError(`Service Worker registration failed: ${error.message}`);
    }
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

    if (!serviceWorker) {
        showError('Service Worker not ready');
        return;
    }

    try {
        updateStatus('connecting');
        console.log('Connecting tab to broker:', brokerUrl);

        const channel = new MessageChannel();
        messageChannel = channel.port1;
        messageChannel.addEventListener('message', handleSWMessage);
        messageChannel.start();

        serviceWorker.postMessage({
            type: 'CONNECT_TAB',
            data: {
                tabId,
                clientId,
                brokerUrl
            }
        }, [channel.port2]);

    } catch (error) {
        console.error('Connection error:', error);
        updateStatus('disconnected');
        showError(`Connection failed: ${error.message}`);
    }
}

async function handleDisconnect() {
    if (!serviceWorker) return;

    try {
        serviceWorker.postMessage({
            type: 'DISCONNECT_TAB',
            data: { tabId }
        });

        updateStatus('disconnected');
        toggleControls(false);
        subscriptions.clear();
        updateSubscriptionsList();

        addMessage('system', 'Disconnected from broker', 'system');

    } catch (error) {
        showError(`Disconnect failed: ${error.message}`);
    }
}

async function handleSubscribe(e) {
    e.preventDefault();

    if (!isConnected || !messageChannel) {
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
        messageChannel.postMessage({
            type: 'SUBSCRIBE',
            data: { topic }
        });
    } catch (error) {
        showError(`Subscribe failed: ${error.message}`);
    }
}

async function handlePublish(e) {
    e.preventDefault();

    if (!isConnected || !messageChannel) {
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
        messageChannel.postMessage({
            type: 'PUBLISH',
            data: { topic, payload }
        });
    } catch (error) {
        showError(`Publish failed: ${error.message}`);
    }
}

function clearMessages() {
    const messagesEl = document.getElementById('messages');
    messagesEl.innerHTML = '<div class="empty-state">No messages yet</div>';
}

async function initApp() {
    try {
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

        await registerServiceWorker();

        console.log('Application ready, Tab ID:', tabId);

    } catch (error) {
        console.error('Failed to initialize application:', error);
        showError(`Failed to initialize application: ${error.message}`);
    }
}

initApp();
