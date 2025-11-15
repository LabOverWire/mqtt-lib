import init, { WasmMqttClient } from './pkg/mqtt5.js';

let initialized = false;
const tabConnections = new Map();

self.addEventListener('install', (event) => {
    console.log('Service Worker installing...');
    self.skipWaiting();
});

self.addEventListener('activate', (event) => {
    console.log('Service Worker activating...');
    event.waitUntil(self.clients.claim());
});

self.addEventListener('message', async (event) => {
    const { type, data } = event.data;

    if (type === 'INIT_WASM') {
        if (!initialized) {
            try {
                await init(data.wasmUrl);
                initialized = true;
                event.ports[0].postMessage({ type: 'WASM_READY' });
            } catch (error) {
                event.ports[0].postMessage({
                    type: 'ERROR',
                    error: `WASM initialization failed: ${error.message}`
                });
            }
        } else {
            event.ports[0].postMessage({ type: 'WASM_READY' });
        }
        return;
    }

    if (type === 'CONNECT_TAB') {
        const { tabId, clientId, brokerUrl } = data;
        const port = event.ports[0];

        if (!initialized) {
            port.postMessage({
                type: 'ERROR',
                error: 'WASM not initialized'
            });
            return;
        }

        try {
            const client = new WasmMqttClient(clientId);
            await client.connect(brokerUrl);

            tabConnections.set(tabId, {
                client,
                port,
                connected: true
            });

            port.addEventListener('message', async (msgEvent) => {
                await handleTabMessage(tabId, msgEvent.data);
            });

            port.start();

            port.postMessage({
                type: 'CONNECTED',
                tabId
            });

            console.log(`Tab ${tabId} connected to ${brokerUrl}`);

        } catch (error) {
            port.postMessage({
                type: 'ERROR',
                error: `Connection failed: ${error}`
            });
        }
    }

    if (type === 'DISCONNECT_TAB') {
        const { tabId } = data;
        const connection = tabConnections.get(tabId);

        if (connection) {
            try {
                await connection.client.disconnect();
                tabConnections.delete(tabId);
                console.log(`Tab ${tabId} disconnected`);
            } catch (error) {
                console.error(`Error disconnecting tab ${tabId}:`, error);
            }
        }
    }
});

async function handleTabMessage(tabId, message) {
    const connection = tabConnections.get(tabId);
    if (!connection) {
        console.error(`No connection found for tab ${tabId}`);
        return;
    }

    const { type, data } = message;
    const { client, port } = connection;

    try {
        switch (type) {
            case 'PUBLISH':
                const { topic, payload } = data;
                const payloadBytes = new TextEncoder().encode(payload);
                await client.publish(topic, payloadBytes);
                port.postMessage({
                    type: 'PUBLISHED',
                    topic
                });
                break;

            case 'SUBSCRIBE':
                const packetId = await client.subscribe(data.topic);
                port.postMessage({
                    type: 'SUBSCRIBED',
                    topic: data.topic,
                    packetId
                });
                break;

            default:
                console.warn(`Unknown message type: ${type}`);
        }
    } catch (error) {
        port.postMessage({
            type: 'ERROR',
            error: `${type} failed: ${error}`
        });
    }
}
