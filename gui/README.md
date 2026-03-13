# Vitruvius GUI

A modern, real-time web interface for the Vitruvius delta-sync network.

## Features

### 🎯 Core Functionality
- **Peer Discovery & Connection** — Real-time mDNS discovery with manual dial support
- **Live Transfer Visualization** — Progress bars, chunk status grid, and real-time stats
- **Queue Management** — Track multiple transfers with individual progress
- **File Browser** — View your sync folder contents with file sizes
- **Real-time Logs** — Event stream with pause/clear controls
- **Bandwidth Chart** — Live download/upload speed visualization

### ⚡ User-Friendly Features
- **Auto-Reconnect** — Automatically reconnects on backend disconnect (configurable)
- **Persistent Settings** — Saves WebSocket URL, sync folder, and preferences to localStorage
- **Copy Node ID** — Click node status to copy your ID to clipboard
- **Responsive Design** — Adapts to different screen sizes
- **Dark Theme** — Easy on the eyes with cyan/blue accent colors
- **Toast Notifications** — Real-time feedback for all actions
- **Offline Banner** — Clear indication when backend is unreachable

### 🎨 Design Highlights
- **Monospace Typography** — Tech-forward look with Share Tech Mono for data
- **Animated Grid Background** — Subtle cyan grid with scanline effect
- **Glow Effects** — Accent colors with box-shadow glows
- **Interactive Feedback** — Hover states, animations, and transitions
- **Well-Organized Layout** — Header, 3-column main area, footer stats panel

## Usage

### Start the Backend
```bash
cd /path/to/Vitruvius
cargo run
# Backend starts on ws://127.0.0.1:8080 by default
```

### Open the GUI
Open the GUI in your browser:
```
file:///path/to/Vitruvius/gui/index.html
```

**Note**: The GUI must be served over HTTP/HTTPS for WebSocket to work properly. If you get WebSocket errors, serve the GUI locally:

```bash
# Using Python 3
cd gui
python3 -m http.server 8000

# Then visit: http://127.0.0.1:8000
```

### Basic Workflow

1. **Set Sync Folder** (Settings → Sync Folder Path)
2. **Discover Peers** (Wait for mDNS or manually dial)
3. **Start Sync** (Click SYNC on a peer or use the + SYNC button)
4. **Monitor Progress** (Watch the transfer card and bandwidth chart)

## Configuration

All settings are stored in browser localStorage:
- `vitruv_wsUrl` — WebSocket backend URL (default: `ws://127.0.0.1:8080`)
- `vitruv_folder` — Your sync folder path
- `vitruv_autoConnect` — Auto-reconnect toggle

## Keyboard Shortcuts

- Delete completed transfers from queue: Click ✕ on the queue item

## Message Types

The GUI expects these WebSocket message types from the backend:

### From Backend
- `Identity` — Your node ID and name
- `PeerDiscovered` — New peer found via mDNS
- `PeerConnected` — Successfully connected to peer
- `PeerDisconnected` — Peer connection lost
- `TransferStarted` — New file sync started
- `ChunkReceived` — Chunk downloaded (includes `verified` flag)
- `TransferComplete` — File fully synced
- `FileList` — List of local files
- `Log` — Event log message
- `Error` — Error message

### To Backend
- `DialPeer` — `{type, peer_id}`
- `DialAddr` — `{type, addr}`
- `Disconnect` — `{type, peer_id}`
- `SetFolder` — `{type, path}`
- `RequestSync` — `{type, peer_id}`
- `GetFileList` — `{type}`

## Architecture

- **Single HTML File** — No build process or external dependencies needed
- **Vanilla JavaScript** — Framework-free, ~2000 lines of code
- **Canvas-Based Charts** — Real-time bandwidth visualization
- **Local Storage** — Persists user preferences
- **WebSocket** — Bidirectional communication with backend

## Customization

### Change Colors
Edit the CSS variables at the top (`--bg`, `--accent`, `--success`, etc.):

```css
:root {
  --accent: #00d9ff;  /* Cyan accent */
  --success: #00ff7f; /* Green success */
  /* ... more colors */
}
```

### Change Font
Replace Google Fonts links in the `<head>` and update CSS vars.

### Disable Auto-Reconnect
In Settings modal, uncheck "Auto-Connect" or set `STATE.autoConnect = false`.

## Browser Support

- Chrome 85+
- Firefox 78+
- Safari 13+
- Edge 85+

**Note**: File explorer button (`📁`) requires the File System Access API (Chrome/Edge). Firefox falls back to a directory picker.

## Troubleshooting

### "Disconnected from Backend"
- Make sure `cargo run` is executing your backend
- Check WebSocket URL in Settings (default: `ws://127.0.0.1:8080`)
- Check backend logs for errors

### WebSocket Connection Fails
- Serve GUI over HTTP, not `file://`
- Browser blocks WebSocket for file:// protocol
- Use `python3 -m http.server` or similar

### Node ID Shows "Not Connected"
- Wait for backend to send `Identity` message
- Check browser console (F12) for errors

## License

Same as Vitruvius parent project.
