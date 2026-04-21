use std::fmt;

use crate::types::node::NodeId;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Error returned when sending a message to an actor fails.
#[derive(Debug, Clone)]
pub struct ActorSendError(pub String);

impl fmt::Display for ActorSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "actor send failed: {}", self.0)
    }
}

impl std::error::Error for ActorSendError {}

/// Error returned by processing group operations.
#[derive(Debug, Clone)]
pub struct GroupError(pub String);

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "group error: {}", self.0)
    }
}

impl std::error::Error for GroupError {}

/// Error returned by `ClusterEvents` operations.
#[derive(Debug, Clone)]
pub struct ClusterError(pub String);

impl fmt::Display for ClusterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cluster error: {}", self.0)
    }
}

impl std::error::Error for ClusterError {}

// ---------------------------------------------------------------------------
// Cluster events
// ---------------------------------------------------------------------------

/// Events emitted by the cluster membership system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterEvent {
    NodeJoined(NodeId),
    NodeLeft(NodeId),
}

/// Opaque handle returned by [`ClusterEvents::subscribe`], used to cancel
/// a subscription via [`ClusterEvents::unsubscribe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub(crate) u64);

impl SubscriptionId {
    /// Create a new `SubscriptionId` from a raw integer.
    ///
    /// Intended for use by shell layers bridging dactor cluster events
    /// to dstate's cluster event model.
    pub fn from_raw(id: u64) -> Self {
        Self(id)
    }
}

// ---------------------------------------------------------------------------
// Abstract traits
// ---------------------------------------------------------------------------

/// Subscription to cluster membership events.
///
/// Implementations bridge the concrete actor framework's cluster discovery
/// (e.g., via `dactor::ClusterEvents`) into dstate's `ClusterEvent` model
/// with `NodeId(u64)`.
pub trait ClusterEvents: Send + Sync + 'static {
    /// Subscribe to cluster membership changes. The callback is invoked for
    /// each `NodeJoined` / `NodeLeft` event. Returns a [`SubscriptionId`]
    /// that can be used to cancel the subscription.
    fn subscribe(
        &self,
        on_event: Box<dyn Fn(ClusterEvent) + Send + Sync>,
    ) -> Result<SubscriptionId, ClusterError>;

    /// Remove a previously registered subscription. Idempotent.
    fn unsubscribe(&self, id: SubscriptionId) -> Result<(), ClusterError>;
}

/// A handle to a scheduled timer that can be cancelled.
pub trait TimerHandle: Send + 'static {
    /// Cancel the timer. Idempotent — calling cancel on an already-cancelled
    /// or fired timer is a no-op.
    fn cancel(self);
}