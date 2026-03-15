# Vitruvius Sync - Technical Flow Diagrams

## 1. Initial File Sync (Full Transfer)

```
Peer A (Server) ← → Peer B (Client)

Peer B: ListFiles
└─→ Peer A: [file1.txt, file2.pdf, doc/notes.md]

Peer B: MetaDataRequest { file1.txt }
└─→ Peer A: VersionedMetadata {
     file_name: "file1.txt",
     file_hash: ABC123...,
     parent_hash: None,                    // Initial version
     total_chunks: 4,
     chunk_hashes: [H1, H2, H3, H4],
     changed_chunks: [0, 1, 2, 3],        // All chunks = new file
     timestamp: 1234567890,
     author: "peer_a"
   }

Peer B: Transfer State
  current_version = abc123...
  required_chunks = {0, 1, 2, 3}           // All 4 chunks needed
  
Peer B: ChunkRequest { chunk_index: 0 }
Peer B: ChunkRequest { chunk_index: 1 }
Peer B: ChunkRequest { chunk_index: 2 }
Peer B: ChunkRequest { chunk_index: 3 }
└─→ Peer A: ChunkResponse { chunk_index: 0, data: [...], hash: H1 }
           ChunkResponse { chunk_index: 1, data: [...], hash: H2 }
           ChunkResponse { chunk_index: 2, data: [...], hash: H3 }
           ChunkResponse { chunk_index: 3, data: [...], hash: H4 }

Peer B: Reassemble
  - Load all 4 chunks in order
  - Write to file1.txt.tmp
  - Verify all chunk hashes match
  - Rename to file1.txt
  - Store local_file_versions["file1.txt"] = version(hash=ABC123)
```

---

## 2. Delta File Sync (100MB File, 1KB Changed)

### Scenario
- Original file: 100MB = 6.25 chunks (16MB each)
  - Chunk 0: bytes 0-16M, hash=H0
  - Chunk 1: bytes 16-32M, hash=H1
  - Chunk 2: bytes 32-48M, hash=H2 ← **MODIFIED**
  - Chunk 3: bytes 48-64M, hash=H3
  - Chunk 4: bytes 64-80M, hash=H4
  - Chunk 5: bytes 80-100M, hash=H5

- Edit: Change 1KB in chunk 2 (at byte offset 40M)
- User saves file

### Sync Flow

```
Peer A (Changed File Locally):

1. File Watcher: Detect write to file1.txt
2. Chunk: Re-chunk file
   - Compute new chunk hashes
   - Compare with local_file_versions["file1.txt"].metadata.chunk_hashes
   
   Old hashes: [H0, H1, H2_old, H3, H4, H5]
   New hashes: [H0, H1, H2_new, H3, H4, H5]
                        ↑ Different!
   
   changed_chunks = [2]  // Only chunk 2 changed

3. Create new version:
   FileVersion {
     file_hash: NEW_ABC123,      // Hash of entire modified file
     parent_hash: ABC123,        // Link to previous version
     changed_chunks: [2],        // Only chunk 2
     timestamp: 1234567991,
     author: "peer_a",
     ...
   }

4. Update local_file_versions["file1.txt"] = new version

5. Broadcast to peers:
   FileChangedWithVersion {
     file_name: "file1.txt",
     file_hash: NEW_ABC123,      // Send hash, not full file!
     changed_chunk_count: 1,
     timestamp: 1234567991
   }

━━━━━━━━━━━━━ NETWORK ━━━━━━━━━━━━━

Peer B (Receives Notification):

1. Receive: FileChangedWithVersion { file_hash: NEW_ABC123, changed_chunk_count: 1 }

2. Check cache:
   if remote_file_versions["file1.txt"] && 
      remote_file_versions["file1.txt"].file_hash == NEW_ABC123 {
       Skip download (already have this version)
   } else {
       Continue...
   }

3. Request metadata:
   MetaDataRequest { file_name: "file1.txt" }

4. Receive:
   VersionedMetadata {
     file_hash: NEW_ABC123,
     parent_hash: ABC123,
     chunk_hashes: [H0, H1, H2_new, H3, H4, H5],
     changed_chunks: [2],           // ← KEY: Only chunk 2!
     ...
   }

5. Create transfer state:
   FileTransferState {
     current_version = version(file_hash=NEW_ABC123),
     previous_version = version(file_hash=ABC123),   // ← Delta mode
     required_chunks = {2},          // ← Sparse set!
     chunks_expected = 1,
     ...
   }

6. Request only 1 chunk:
   ChunkRequest { chunk_index: 2 }    ← Only this!

7. Receive:
   ChunkResponse { chunk_index: 2, data: [16MB new data], hash: H2_new }

8. Process chunk:
   - Verify hash H2_new
   - Store received_chunks[2] = [16MB new data]
   - Transfer complete? Yes, required_chunks.len() == received_chunks.len()

9. Reassemble:
   Output file setup:
   - Check if file1.txt exists
   - Load existing file → chunks = [H0, H1, H2_old, H3, H4, H5]
   - Prepare new version with changed chunks
   
   Write process:
   - Byte 0-16M:   Use chunk 0 from disk (unchanged)
   - Byte 16-32M:  Use chunk 1 from disk (unchanged)
   - Byte 32-48M:  Use NEW chunk 2 (transferred)
   - Byte 48-64M:  Use chunk 3 from disk (unchanged)
   - Byte 64-80M:  Use chunk 4 from disk (unchanged)
   - Byte 80-100M: Use chunk 5 from disk (unchanged)
   
   Result: 100MB file reassembled, only 16MB disk reads needed!

10. Complete:
    local_file_versions["file1.txt"] = version(NEW_ABC123)
    Verify file hash matches metadata
    File ready for use

█══════════════════════════════════════════════════════════█
█ Result: 16MB transferred, not 100MB!                      █
█ Savings: 84MB, ~84% bandwidth reduction                   █
█═════════════════════════════════════════════════════════ █
```

---

## 3. Concurrent Edits & Conflict Resolution

### Scenario
- Both peers edit same file at same time
- Peer A saves at T1 (earlier)
- Peer B saves at T2 (later)

```
Timeline:

Peer A: file1.txt v1 (hash=ABC)
├─ Edit at T1050
└─ Chunk & create v2 (hash=XYZ, changed_chunks=[1,3])

Peer B: file1.txt v1 (hash=ABC)
├─ Edit at T1055  (5ms later!)
└─ Chunk & create v2 (hash=DEF, changed_chunks=[2,4])

━━━━━━━━━━━━━ Peer A broadcasts first ━━━━━━━━━━━━━

Peer B receives: 
  FileChangedWithVersion { 
    file_hash: XYZ, 
    timestamp: T1050,  ← Peer A's v2
    ...
  }

Peer B's state:
  local_file_versions["file1.txt"] = v2(DEF, timestamp=T1055)
  
Decision:
  Remote timestamp (T1050) < Local timestamp (T1055)
  → Keep local version (T1055 is later)
  → Peer A will eventually receive our broadcast and reverse

Peer B broadcasts:
  FileChangedWithVersion {
    file_hash: DEF,
    timestamp: T1055,  ← Peer B's v2
    ...
  }

Peer A receives:
  FileChangedWithVersion {
    file_hash: DEF,
    timestamp: T1055
  }
  
  Peer A's state:
    local_file_versions["file1.txt"] = v2(XYZ, timestamp=T1050)
    
  Decision:
    Remote timestamp (T1055) > Local timestamp (T1050)
    → Accept Peer B's version (T1055 is later)
    → Request delta transfer
    → Download chunks [2,4]
    → Update local to v2(DEF, T1055)
    → Discard our v2(XYZ, T1050)

Result:
  Both peers converge to v2(DEF, timestamp=T1055)
  Version history preserved: v1(ABC) → v2(XYZ) → v2(DEF)
  Can audit the conflict and recover if needed
```

---

## 4. File Deletion

```
Peer A: Delete file1.txt

Event: EventKind::Remove(RemoveKind::File)
Action:
  1. cached_chunks.remove("file1.txt")
  2. local_file_versions.remove("file1.txt")  // No version for deleted file
  3. Broadcast to peers:
     SyncMessage::FileDeleted { file_name: "file1.txt" }

Peer B: Receive FileDeleted

Action:
  1. cached_chunks.remove("file1.txt")
  2. remote_file_versions.remove("file1.txt")
  3. Delete local file
  4. Recently_remote.insert("file1.txt")  // Don't re-sync our own deletion
```

---

## 5. Reconnection Scenario

```
Initial State:
  Peer A has: file1.txt v3(hash=ABC, timestamp=T100)
  Peer B offline

Peer A edits file1.txt:
  v4(hash=XYZ, changed_chunks=[1,5], timestamp=T150)
  Broadcasts → Peer B offline (message lost)

Peer A edits again:
  v5(hash=DEF, changed_chunks=[0], timestamp=T200)
  Broadcasts → Peer B still offline

Peer B comes online:

Connection established
Peer B: ListFiles
  Result: [file1.txt, file2.pdf]

Peer B: MetaDataRequest { file1.txt }
  Receives: VersionedMetadata {
    file_hash: DEF,           // Latest
    parent_hash: XYZ,         // Previous
    changed_chunks: [0],
    timestamp: T200,
    ...
  }

Peer B knows:
  Last version I have: v3(ABC, T100)
  Latest remote: v5(DEF, T200)
  
  Can detect entire chain: v3 → v4 → v5
  Can optionally audit "what changed since I was offline"

Transfer:
  required_chunks = [0]  (only chunk 0 changed in v5)
  Blend existing chunks [1,2,3,4,5] with new chunk [0]

Result: Fast reconnection, minimal transfer!
```

---

## 6. Protocol Message Sequence

### Full Initial Sync
```
Client                          Server
  │                               │
  ├─ ListFiles ────────────────→  │
  │                               │
  │ ← [file1, file2, file3] ──────┤
  │                               │
  ├─ MetaDataRequest(file1) ─────→ │
  │                               │
  │ ← VersionedMetadata(file1) ───┤
  │                               │
  ├─ ChunkRequest(0) ────────────→ │
  ├─ ChunkRequest(1) ────────────→ │
  │                               │
  │ ← ChunkResponse(0) ───────────┤
  │ ← ChunkResponse(1) ───────────┤
  │                               │
  ├─ (reassemble, next meta) ────→ │
  │                               │
  └─ (repeat for file2, file3) ───→
```

### Delta Update
```
Server                          Client
  │                               │
  ├─ FileChangedWithVersion ─────→ │
  │    (file_hash=NEW, count=3)    │
  │                               │
  │                     (check cache)
  │                   (not found, request)
  │                               │
  │ ← MetaDataRequest ────────────┤
  │                               │
  ├─ VersionedMetadata ──────────→ │
  │    (changed_chunks: [2,5,7])   │
  │                               │
  │ ← ChunkRequest(2) ────────────┤
  │ ← ChunkRequest(5) ────────────┤
  │ ← ChunkRequest(7) ────────────┤
  │                               │
  ├─ ChunkResponse(2) ───────────→ │
  ├─ ChunkResponse(5) ───────────→ │
  ├─ ChunkResponse(7) ───────────→ │
  │                               │
  │                     (reassemble)
  │                   (blend + write)
  │                               │
  │ ← TransferComplete ───────────┤
```

---

## 7. Storage Layout Example

```
Sync folder: /home/user/sync/

Local version cache (in-memory, can be persistent):
  file1.txt:
    hash: ABC123...
    parent_hash: ABC012...
    chunks: [H0, H1, H2, H3]
    timestamp: 1234567890
    author: peer_a
    
  file2.pdf:
    hash: XYZ456...
    parent_hash: None
    chunks: [H0, H1, H2, H3, H4, H5]
    timestamp: 1234567800
    author: peer_b

Remote version cache (from peers):
  file1.txt @ peer_b:
    hash: DEF789...
    timestamp: 1234567950
    
Actual files:
  file1.txt ........................... (latest synced version)
  file2.pdf ........................... (latest synced version)
  doc/notes.md ........................ (synced)
  images/photo.jpg .................... (synced)
```

---

## 8. Performance Characteristics

### Bandwidth Usage
```
Scenario: 1GB file, 0.1% change (1MB)

Before (full sync):
  File size: 1 GB
  Transfer: 1 GB
  Chunks: 64 full chunks

After (delta sync):
  Changed size: 1 MB  
  Transfer: ~2 MB  (1 partial chunk ~ 1 MB + metadata)
  Chunks: 1 partial chunk
  Savings: 99.8%
```

### Latency
```
Small file (1MB):
  Full sync: 20ms (1 chunk request + response)
  Delta sync: 20ms (1 chunk request + response)
  Same!

Large file (100MB) with 1% change:
  Full sync: 50 requests × 2ms latency = 100ms
  Delta sync: 2 requests × 2ms latency = 4ms
  Savings: 96% latency
```

### Storage
```
Per file version metadata: ~100 bytes
Version history per file: ~1KB per 10 versions
Average overhead: <1% of total storage
```

