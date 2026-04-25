use std::any::Any;

use dactor::HeaderValue;
use dcontext::ContextSnapshot;

/// Header carrying serialized context bytes for wire transport.
///
/// Attached by [`ContextOutboundInterceptor`](crate::ContextOutboundInterceptor)
/// on the sender side. Deserialized on the receiver side by
/// [`ContextInboundInterceptor`](crate::ContextInboundInterceptor) or
/// the handler via [`extract_context`](crate::extract_context).
pub struct ContextHeader {
    /// Serialized dcontext bytes (bincode wire format).
    pub(crate) bytes: Vec<u8>,
}

impl HeaderValue for ContextHeader {
    fn header_name(&self) -> &'static str {
        "dcontext.wire"
    }

    /// Returns the serialized bytes for remote transport.
    fn to_bytes(&self) -> Option<Vec<u8>> {
        Some(self.bytes.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Header carrying a `ContextSnapshot` for in-process (local) propagation.
///
/// This preserves local-only context values that would be lost during
/// serialization. Preferred over [`ContextHeader`] for same-process actors.
pub struct ContextSnapshotHeader {
    pub(crate) snapshot: ContextSnapshot,
}

impl HeaderValue for ContextSnapshotHeader {
    fn header_name(&self) -> &'static str {
        "dcontext.snapshot"
    }

    /// Returns `None` — snapshots are local-only and not sent over the wire.
    fn to_bytes(&self) -> Option<Vec<u8>> {
        None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
