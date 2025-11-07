# Feature Gates for hft-ipc

## Overview

The `hft-ipc` crate now supports conditional compilation through feature gates to allow flexible usage patterns.

## Features

### `ipc` (Optional)

Enables full IPC functionality including:
- Unix Domain Socket server and client
- MessagePack serialization/deserialization
- UUID-based request correlation
- Async runtime support

**Dependencies enabled**:
- `tokio` - Async runtime and networking
- `rmp-serde` - MessagePack serialization
- `uuid` - Unique request IDs
- `async-trait` - Async trait support

### `default` (Empty)

By default, only message type definitions are available. This allows:
- Type-level programming without runtime overhead
- Configuration structures without IPC dependencies
- Minimal dependency footprint for applications that only need message types

## Usage Examples

### Minimal Usage (Message Types Only)

```toml
[dependencies]
hft-ipc = { path = "...", default-features = false }
```

This configuration:
- ✅ Provides all message type definitions
- ✅ Zero async runtime overhead
- ✅ No socket or serialization dependencies
- ❌ Cannot create IPC server/client

### Full IPC Functionality

```toml
[dependencies]
hft-ipc = { path = "...", features = ["ipc"] }
```

This configuration:
- ✅ All message type definitions
- ✅ IPC server and client
- ✅ MessagePack serialization
- ✅ UUID request correlation

## Implementation Details

### Conditional Compilation Structure

```
lib.rs
├── messages (always compiled)
│   ├── Request types
│   ├── Response types
│   └── Status updates
└── #[cfg(feature = "ipc")]
    ├── client (Unix socket client)
    ├── server (Unix socket server)
    └── handlers (command handlers)
```

### Request ID Type Adaptation

The `RequestId` type adapts based on feature flags:

- **With `ipc` feature**: `type RequestId = uuid::Uuid`
- **Without `ipc` feature**: `type RequestId = String`

This allows message structures to remain serializable in both configurations.

### Error Type Adaptation

The `IPCError` enum conditionally includes serialization errors:

```rust
pub enum IPCError {
    Io(std::io::Error),              // Always available

    #[cfg(feature = "ipc")]
    Serialization(rmp_serde::Error),  // Only with IPC feature

    #[cfg(feature = "ipc")]
    Deserialization(rmp_serde::Error), // Only with IPC feature

    Handler(String),                  // Always available
    Timeout,                          // Always available
}
```

## Integration with Runtime

The `hft-runtime` crate includes IPC support through its `infra-ipc` feature:

```toml
[features]
infra-ipc = ["dep:infra-ipc", "infra-ipc/ipc"]
```

This ensures:
1. Runtime can optionally depend on IPC
2. IPC feature is transitively enabled when needed
3. Applications without control plane needs can exclude IPC entirely

## Verification Commands

```bash
# Check with no features (messages only)
cargo check -p hft-ipc --no-default-features

# Check with default features (currently empty)
cargo check -p hft-ipc

# Check with full IPC support
cargo check -p hft-ipc --features ipc

# Build and inspect dependencies
cargo tree -p hft-ipc --no-default-features
cargo tree -p hft-ipc --features ipc
```

## Compilation Results

### Without Features
```
Dependencies:
├── serde (serialization framework)
├── thiserror (error types)
├── tracing (logging)
└── rust_decimal (numeric types)

Total: ~4 direct dependencies
```

### With IPC Feature
```
Additional Dependencies:
├── tokio (async runtime + networking)
├── rmp-serde (MessagePack)
├── uuid (unique IDs)
└── async-trait (async traits)

Total: ~8 direct dependencies
```

## Benefits

1. **Flexible Deployment**: Applications can choose their level of IPC integration
2. **Reduced Dependencies**: Message-only usage avoids async runtime and networking dependencies
3. **Build Time**: Faster compilation for applications without control plane needs
4. **Binary Size**: Smaller binaries when IPC functionality is not needed
5. **Type Safety**: Message types remain available for configuration without runtime overhead

## Migration Guide

### From Always-IPC to Feature-Gated

**Before**:
```rust
use hft_ipc::{IPCServer, IPCClient};  // Always available
```

**After**:
```rust
// Messages always available
use hft_ipc::{Command, Response};

// Server/client behind feature gate
#[cfg(feature = "ipc")]
use hft_ipc::{IPCServer, IPCClient};
```

### For Library Authors

If your library only needs message types for configuration:

```toml
[dependencies]
hft-ipc = { path = "...", default-features = false }
```

If your library provides IPC services:

```toml
[dependencies]
hft-ipc = { path = "...", features = ["ipc"] }
```

## Related Issues

- **P1**: IPC feature gates implementation (this document)
- **Performance**: Zero-cost abstractions maintained
- **Compatibility**: All existing functionality preserved behind feature flag
