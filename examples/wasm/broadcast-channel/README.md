# MQTT BroadcastChannel Example

This example demonstrates peer-to-peer MQTT messaging across browser tabs using the BroadcastChannel API, with one tab acting as an in-memory broker.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         Browser                              │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         BroadcastChannel "mqtt-demo"                 │   │
│  └──────────────────────────────────────────────────────┘   │
│              ↑           ↑           ↑           ↑          │
│              │           │           │           │          │
│         ┌────┴────┐ ┌───┴────┐ ┌───┴────┐ ┌───┴────┐      │
│         │  Tab 1  │ │ Tab 2  │ │ Tab 3  │ │ Tab 4  │      │
│         │ 👑Broker│ │ Client │ │ Client │ │ Client │      │
│         │(MQTT    │ │        │ │        │ │        │      │
│         │ routing)│ │        │ │        │ │        │      │
│         └─────────┘ └────────┘ └────────┘ └────────┘      │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

## Key Features

- **Broker Election**: First tab to connect becomes the in-memory MQTT broker
- **Peer-to-Peer**: All tabs communicate directly via BroadcastChannel
- **No External Network**: Completely browser-based, works offline
- **Tab Discovery**: Tabs announce themselves and discover each other
- **MQTT Semantics**: Publish/subscribe pattern with topic routing
- **Same-Origin Only**: BroadcastChannel requires same origin for security

## How It Works

### Broker Election

1. **First Tab**: When the first tab connects, it becomes the broker
   - Maintains routing table of subscriptions
   - Routes publish messages to subscribed topics
   - Handles retained messages (optional)

2. **Subsequent Tabs**: Connect as clients via BroadcastChannel transport
   - Use `WasmMqttClient.connect_broadcast_channel(channel_name)`
   - Send MQTT packets over the channel
   - Broker routes messages to matching subscribers

### Discovery Protocol

Tabs use a separate control channel (`${channelName}-control`) for:
- Announcing presence every 5 seconds
- Declaring role (broker or client)
- Detecting when tabs close
- Broker failover (if broker tab closes)

## Why Use This Pattern?

### Advantages

1. **No External Broker**: Works completely offline, no network dependency
2. **Low Latency**: Direct browser communication, no network round-trip
3. **Simple Setup**: No broker configuration or deployment
4. **Multi-Tab Sync**: Perfect for syncing state across tabs
5. **Privacy**: Messages never leave the browser

### Use Cases

- Local collaboration tools (shared clipboard, task list)
- Multi-tab application state synchronization
- Offline-first applications
- Development and testing without broker
- Educational demonstrations of MQTT

### Limitations

- Same-origin only (can't cross domains)
- No persistence (messages lost on browser close)
- Limited to single browser instance
- No external clients
- Memory-based only (not durable)

## Prerequisites

- Modern web browser with BroadcastChannel support
- Web server (for serving files)
- `wasm-pack` installed

## Browser Support

- ✅ Chrome/Edge 54+
- ✅ Firefox 38+
- ✅ Safari 15.4+
- ❌ IE 11 (no BroadcastChannel support)

## Building

From the repository root:

```bash
# Build the WASM package
cd examples/wasm
./build.sh

# Or manually:
wasm-pack build --target web --features wasm-client
cp -r pkg examples/wasm/broadcast-channel/
```

## Running

```bash
cd examples/wasm/broadcast-channel

# Option 1: Python
python3 -m http.server 8000

# Option 2: Node.js
npx serve -p 8000

# Option 3: npm
npm run serve
```

Then open http://localhost:8000 in your browser.

## Testing Multi-Tab Behavior

### Basic Test

1. Open http://localhost:8000 in first tab
2. Connect to channel (default: "mqtt-demo")
3. Note status shows "Role: Broker"
4. Open second tab (Ctrl/Cmd + T)
5. Connect to same channel name
6. Note second tab shows "Role: Client"

### Publish/Subscribe Test

1. In Tab 1 (broker): Subscribe to "test/topic"
2. In Tab 2 (client): Subscribe to "test/topic"
3. In Tab 2: Publish message "Hello from Tab 2" to "test/topic"
4. Verify both tabs receive the message

### Multi-Client Test

1. Open 4 tabs
2. Connect all to same channel
3. Tab 1 becomes broker, others are clients
4. Have all tabs subscribe to "sensors/+"
5. Publish from any tab to "sensors/temperature"
6. All subscribed tabs should receive

### Broker Failover Test (Future Enhancement)

1. Open 3 tabs, all connected
2. Close broker tab (Tab 1)
3. Observe which tab becomes new broker
4. Verify messaging continues

## Configuration

**Channel Name**: Logical group for tabs to communicate (default: "mqtt-demo")

All tabs with the same channel name will communicate. Different channel names create isolated groups.

## File Structure

```
broadcast-channel/
├── index.html          # UI with role indicator
├── app.js              # Broker election and client logic
├── styles.css          # Styling with role badges
├── README.md           # This file
├── package.json        # npm scripts
└── pkg/                # Generated WASM package
    ├── mqtt5.js
    ├── mqtt5_bg.wasm
    └── ...
```

## Code Explanation

### Broker Role

When tab becomes broker:
```javascript
isBroker = true;
brokerState = {
    subscriptions: new Map(),  // topic -> Set<tabIds>
    retained: new Map()        // topic -> message
};
```

### Client Role

When tab becomes client:
```javascript
isBroker = false;
await client.connect_broadcast_channel(channelName);
// Now can publish/subscribe via MQTT protocol
```

### Discovery

Control channel for tab coordination:
```javascript
const controlChannel = new BroadcastChannel(`${channelName}-control`);

// Announce presence
controlChannel.postMessage({
    type: 'ANNOUNCE',
    tabId,
    isBroker,
    timestamp: Date.now()
});

// Listen for others
controlChannel.addEventListener('message', handleControlMessage);
```

## Troubleshooting

### Tabs Not Discovering Each Other

- **Check channel name**: Must be identical across tabs
- **Check console**: Look for announcement messages
- **Check timing**: Wait a few seconds for discovery

### Messages Not Routing

- **Verify broker**: One tab should show "Role: Broker"
- **Check subscriptions**: Ensure topics match (use wildcards carefully)
- **Check console**: Look for routing debug messages

### Performance Issues

- **Too many tabs**: Each message broadcasts to all tabs
- **Large payloads**: BroadcastChannel has memory limits
- **Message frequency**: High-frequency updates may cause lag

## Implementation Details

### Current Limitations

This example is a **basic implementation**. Production use would need:

- **Broker failover**: Auto-elect new broker if broker tab closes
- **QoS support**: Currently only QoS 0 (at most once)
- **Retained messages**: Store last message per topic
- **Session persistence**: Remember subscriptions across reconnects
- **Wildcard matching**: Full MQTT wildcard support (+, #)
- **Access control**: Per-tab permissions (who can publish where)

### Message Flow

1. Client publishes to topic
2. Message sent over BroadcastChannel
3. Broker receives message
4. Broker checks subscription routing table
5. Broker broadcasts to matching subscribers
6. Subscribed clients receive message

### Memory Considerations

- Each tab maintains its own state
- Broker maintains subscription routing table
- Messages not persisted (ephemeral)
- Closing broker tab loses all state

## Differences from Other Examples

| Feature | WebSocket | MessagePort | BroadcastChannel |
|---------|-----------|-------------|------------------|
| External Broker | Required | Required | No (in-memory) |
| Network | Yes | Yes | No |
| Offline | No | Partially | Yes |
| Multi-Tab | Independent | Shared conn | Peer-to-peer |
| Complexity | Simple | Moderate | Moderate |
| Use Case | Production | PWA/offline | Local/demo |

## Next Steps

- Implement broker failover election
- Add retained message support
- Implement QoS 1 and 2
- Add authentication/authorization
- Persist state to IndexedDB
- Add message history/replay

## Learn More

- [BroadcastChannel API](https://developer.mozilla.org/en-US/docs/Web/API/BroadcastChannel)
- [MQTT Specification](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html)
- [mqtt5 Documentation](https://docs.rs/mqtt5)
