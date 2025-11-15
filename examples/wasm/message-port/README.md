# MQTT MessagePort Example

This example demonstrates using Service Workers with MessagePort to create a browser-based MQTT client where each tab maintains its own connection through a centralized Service Worker.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                         Browser                               │
│                                                               │
│  ┌─────────────┐                    ┌─────────────┐          │
│  │   Tab 1     │────MessagePort────▶│  Service    │          │
│  │  (Client)   │                    │  Worker     │          │
│  └─────────────┘                    │             │          │
│                                     │  WebSocket  │◀─────────┼──▶ MQTT Broker
│  ┌─────────────┐                    │  Connection │          │
│  │   Tab 2     │────MessagePort────▶│  for Tab 1  │          │
│  │  (Client)   │                    │             │          │
│  └─────────────┘                    │  WebSocket  │◀─────────┼──▶ MQTT Broker
│                                     │  Connection │          │
│  ┌─────────────┐                    │  for Tab 2  │          │
│  │   Tab 3     │────MessagePort────▶│             │          │
│  │  (Client)   │                    │  ...etc...  │          │
│  └─────────────┘                    └─────────────┘          │
│                                                               │
└──────────────────────────────────────────────────────────────┘
```

## Key Features

- **Service Worker Integration**: Uses Service Workers to manage MQTT connections
- **Per-Tab Connections**: Each tab gets its own WebSocket connection to the broker
- **Persistent Background**: Service Worker persists even when tabs are closed
- **MessagePort Communication**: Bidirectional communication between tabs and Service Worker
- **Tab Isolation**: Each tab's connection is independent

## Why Use This Pattern?

### Advantages

1. **Connection Management**: Service Worker can manage connections independently of tab lifecycle
2. **Background Processing**: Service Worker continues running even if tabs are closed
3. **Offline Support**: Service Workers enable offline caching and background sync
4. **Resource Coordination**: Central point to coordinate multiple connections

### Use Cases

- Applications that need background MQTT message processing
- Multi-tab applications where each tab needs independent broker connection
- PWAs (Progressive Web Apps) requiring offline capabilities
- Scenarios where connection state should persist across tab reloads

## Prerequisites

- Modern web browser with Service Worker support (Chrome, Firefox, Edge, Safari)
- Web server (Service Workers require HTTPS in production, or localhost for development)
- `wasm-pack` installed for building the WASM module
- MQTT broker accessible via WebSocket

## Building

From the repository root:

```bash
# Build the WASM package
cd examples/wasm
./build.sh

# Or manually:
wasm-pack build --target web --features wasm-client
cp -r pkg examples/wasm/message-port/
```

## Running

```bash
cd examples/wasm/message-port

# Option 1: Python
python3 -m http.server 8000

# Option 2: Node.js
npx serve -p 8000

# Option 3: npm
npm run serve
```

Then open http://localhost:8000 in your browser.

## Testing Multi-Tab Behavior

1. Open first tab at http://localhost:8000
2. Wait for Service Worker to register (status should show "Ready")
3. Enter broker URL and connect
4. Open second tab at http://localhost:8000 (Ctrl/Cmd + T)
5. Notice it has a different Tab ID
6. Connect from second tab
7. Both tabs now have independent connections through the Service Worker

You can:
- Subscribe to topics in each tab independently
- Publish from one tab and see it arrive in the other (if both subscribed to the same topic)
- Close tabs and reopen them - Service Worker persists

## How It Works

### 1. Service Worker Registration

```javascript
const registration = await navigator.serviceWorker.register('./service-worker.js', {
    type: 'module'
});
```

The Service Worker is registered as an ES module, allowing it to import the WASM module.

### 2. MessageChannel Creation

```javascript
const channel = new MessageChannel();
messageChannel = channel.port1;  // Tab keeps port1
// Transfer port2 to Service Worker
serviceWorker.postMessage({...}, [channel.port2]);
```

A MessageChannel creates two linked ports for bidirectional communication.

### 3. Connection Per Tab

When a tab connects:
```javascript
// Tab sends connect request with its unique ID
serviceWorker.postMessage({
    type: 'CONNECT_TAB',
    data: { tabId, clientId, brokerUrl }
}, [port2]);

// Service Worker creates WebSocket for this tab
const client = new WasmMqttClient(clientId);
await client.connect(brokerUrl);
tabConnections.set(tabId, { client, port });
```

### 4. Message Routing

Service Worker maintains a map of tab connections:
```javascript
const tabConnections = new Map();
// Each entry: tabId -> { client, port, connected }
```

## Configuration

Default broker: `ws://broker.hivemq.com:8000/mqtt`

Alternative brokers:
- `ws://test.mosquitto.org:8080`
- `ws://broker.emqx.io:8083/mqtt`
- `ws://mqtt.eclipseprojects.io:80/mqtt`

## File Structure

```
message-port/
├── index.html              # UI for MQTT client
├── app.js                  # Tab-side application logic
├── service-worker.js       # Service Worker managing connections
├── styles.css              # Styling
├── README.md               # This file
├── package.json            # npm scripts
└── pkg/                    # Generated WASM package
    ├── mqtt5.js
    ├── mqtt5_bg.wasm
    └── ...
```

## Troubleshooting

### Service Worker Not Registering

- **Check HTTPS**: Service Workers require HTTPS in production (localhost is exempt)
- **Check browser console**: Look for registration errors
- **Clear registration**: In DevTools → Application → Service Workers → Unregister

### WASM Module Not Loading

- **Check path**: Ensure `./pkg/mqtt5_bg.wasm` is accessible
- **Check MIME type**: Server must serve `.wasm` files with `application/wasm` MIME type
- **Check network**: Look in DevTools Network tab for 404 errors

### Connections Not Working

- **Check broker URL**: Ensure it's a WebSocket URL (ws:// or wss://)
- **Check CORS**: Broker must allow connections from your origin
- **Check firewall**: Ensure WebSocket port is accessible

### Multiple Tabs Not Connecting

- **Check Service Worker status**: All tabs should show same Service Worker
- **Check tab IDs**: Each tab should have unique ID
- **Check console**: Look for errors in both tab and Service Worker consoles

## Browser Support

- ✅ Chrome/Edge 90+
- ✅ Firefox 88+
- ✅ Safari 15.4+
- ❌ IE 11 (no Service Worker support)

## Security Considerations

- Service Workers have significant privileges - use HTTPS in production
- Each tab gets its own connection - consider resource limits
- Service Worker persists across page loads - consider memory usage
- MessagePorts transfer full ownership - handle lifecycle carefully

## Performance Notes

- Service Worker startup adds ~100-200ms latency on first load
- Each tab maintains independent WebSocket connection
- Memory usage scales with number of tabs (each has own connection)
- Consider implementing connection pooling if you need tab sharing

## Differences from WebSocket Example

| Feature | WebSocket Example | MessagePort Example |
|---------|-------------------|---------------------|
| Architecture | Direct connection | Service Worker proxy |
| Lifecycle | Tab-bound | Persistent |
| Offline | No | Yes (with SW) |
| Multi-tab | Independent | Coordinated via SW |
| Complexity | Simple | Moderate |

## Next Steps

- Try the BroadcastChannel example for peer-to-peer multi-tab messaging
- Implement message persistence in Service Worker
- Add offline queue for messages sent while disconnected
- Implement shared subscriptions across tabs

## Learn More

- [Service Worker API](https://developer.mozilla.org/en-US/docs/Web/API/Service_Worker_API)
- [MessageChannel API](https://developer.mozilla.org/en-US/docs/Web/API/MessageChannel)
- [MQTT over WebSocket](https://www.hivemq.com/blog/mqtt-essentials-special-mqtt-over-websockets/)
- [mqtt5 Documentation](https://docs.rs/mqtt5)
