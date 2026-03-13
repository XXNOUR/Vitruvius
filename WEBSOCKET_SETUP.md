# WebSocket Integration - Build & Run Guide

## What Was Added

### Backend Changes
1. **New WebSocket Server Module** (`src/ws.rs`)
   - Listens on `ws://0.0.0.0:8080`
   - Accepts multiple GUI client connections
   - Broadcasts events to all connected clients
   - Handles peer discovery, transfers, and logging

2. **Updated Dependencies** (Cargo.toml)
   - `tokio-tungstenite` - WebSocket support
   - `serde_json` - JSON serialization
   - `axum` - HTTP framework (optional, for future features)
   - `futures-util` - Async utilities
   - Edition updated from "2024" to "2021"

3. **Integrated WebSocket into App** (src/app.rs)
   - Removed CLI input prompts (dialoguer)
   - Added WebSocket server initialization
   - Broadcasting peer discovery events to GUI
   - Broadcasting peer connection events to GUI
   - Ready for broadcasting transfer progress (next phase)

### GUI Status
The GUI in `gui/index.html` already has full WebSocket support:
- Connects to ws://127.0.0.1:8080
- Listens for all event types
- Sends commands to backend

## Build Instructions

### 1. Build the Backend
```bash
cd /home/light/Coding/Uni/Vitruvius
cargo build --release
# or for faster development:
cargo build
```

### 2. Run the Backend
```bash
# The backend will now:
# - Start libp2p swarm
# - Bind WebSocket server to 0.0.0.0:8080
# - Print your node ID
# - Wait for GUI connections
cargo run
```

Expected output:
```
--- Vitruvius Node ---
YOUR ID: 12D3KooWM5VNU75KCkBCttkiUXsvnRmamngVLjHKpYcJDXvy968U
Syncing folder: "/home/light/Coding/Uni/Vitruvius/sync"
Listening on WebSocket: ws://0.0.0.0:8080
Waiting for GUI connections and peer discovery...
```

### 3. Serve & Open the GUI
```bash
cd gui
python3 -m http.server 8000
# In browser: http://127.0.0.1:8000
```

## Configuration

### Environment Variables
- `VITRUVIUS_SYNC_DIR` - Override default sync folder (default: `./sync`)
  ```bash
  VITRUVIUS_SYNC_DIR=/path/to/sync cargo run
  ```

### GUI Settings (in the browser)
- WebSocket URL: `ws://127.0.0.1:8080` (default)
- Sync Folder: You can set via the Settings modal
- Auto-Connect: Toggle auto-reconnect behavior

## Event Flow

### Peer Discovery (working)
1. Backend discovers peer via mDNS
2. Backend sends `PeerDiscovered` event to GUI via WebSocket
3. GUI shows peer in the peers list
4. User can click "DIAL" to connect

### File Transfer (ready to implement)
- Backend sends `TransferStarted` when file sync begins
- Backend sends `ChunkReceived` for each chunk downloaded
- Backend sends `TransferComplete` when done
- GUI updates progress bars in real-time

## Debugging

### Check WebSocket Connection
In browser console (F12):
```javascript
new WebSocket('ws://127.0.0.1:8080')
// Should show "OPEN" status if backend is running
```

### Check Backend Events
Backend logs will show:
```
WebSocket client connected: 127.0.0.1:xxxxx
```

### Common Issues

**"Cannot connect to WebSocket"**
- Make sure backend is running (`cargo run`)
- Check if port 8080 is in use: `lsof -i :8080`
- Check backend logs for errors

**"Peers not showing up"**
- Ensure mDNS is working (libp2p should log discoveries)
- Try manually dialing with peer IDs
- Check firewall settings

**"GUI shows OFFLINE"**
- Verify WebSocket URL in Settings
- Ensure you're using http:// not file:// for the GUI
- Check browser network tab (F12)

## Next Steps

### Immediate (to make it fully functional)
1. Add transfer progress broadcasting to app.rs
2. Handle GUI commands (dial, sync requests) from WebSocket messages
3. Broadcast file list when requested
4. Broadcast folder changes

### Future Enhancements
- Remote folder browsing
- Bandwidth throttling
- Pause/resume transfers
- Conflict resolution UI
- Delta visualization
- Peer statistics

## File Structure
```
Vitruvius/
├── src/
│   ├── app.rs          (✅ Updated with WebSocket)
│   ├── ws.rs           (✅ New WebSocket server)
│   ├── lib.rs          (✅ Added ws module)
│   ├── network/
│   ├── storage/
│   └── sync/
├── gui/
│   ├── index.html      (✅ WebSocket client ready)
│   └── README.md       (✅ GUI documentation)
├── Cargo.toml          (✅ Updated with deps)
└── ...
```

## Status Matrix

| Feature | Backend | GUI | Status |
|---------|---------|-----|--------|
| Peer Discovery | ✅ Sending | ✅ Receiving | Working |
| Peer Connection | ✅ Sending | ✅ Receiving | Working |
| Transfer Progress | ⏳ Ready | ✅ Ready | Needs broadcast code |
| Dial Commands | ⏳ Ready | ✅ Sending | Needs handler |
| Sync Commands | ⏳ Ready | ✅ Sending | Needs handler |
| File List | ⏳ Ready | ✅ Showing | Needs broadcast |

**Legend:** ✅ = Done, ⏳ = Ready to implement, ❌ = Not done

## Testing Checklist

- [ ] Backend compiles with `cargo build`
- [ ] Backend runs without errors
- [ ] "WebSocket server listening on ws://0.0.0.0:8080" appears in logs
- [ ] GUI loads at http://127.0.0.1:8000
- [ ] GUI shows "ONLINE" after a moment
- [ ] Node ID appears in header
- [ ] Status dot is green/animated
- [ ] mDNS discovery works (peers appear)
- [ ] Can manually dial peers

