use std::collections::HashMap;
use std::sync::Arc;

use crate::value::ContextValue;

// ── Immutable scope node (frozen parent scopes) ────────────────

/// A frozen scope in the parent chain. Immutable after creation.
/// `Send + Sync` — safe inside `Arc`.
pub(crate) struct ScopeNode {
    /// Human-readable scope name. Named scopes appear in scope_chain().
    pub(crate) name: Option<String>,
    /// Values set in this scope (sparse — only keys modified here).
    pub(crate) values: HashMap<&'static str, Arc<dyn ContextValue>>,
    /// Link to parent scope.
    pub(crate) parent: Option<Arc<ScopeNode>>,
    /// Scope depth at the time this node was created.
    pub(crate) depth: usize,
    /// Remote chain state saved at scope entry.
    pub(crate) remote_chain: Arc<Vec<String>>,
    pub(crate) remote_chain_base_depth: usize,
}

// ── Mutable context store ──────────────────────────────────────

/// The active context state. Lives in `Cell<Option<ContextStore>>`.
///
/// ## Contention-free design
///
/// The store is accessed via the `Cell` take/set pattern:
/// 1. `cell.take()` — move store out (Cell becomes `None` = "busy")
/// 2. Modify store — only refcount bumps and pointer moves, no user code
/// 3. `cell.set(Some(store))` — move store back
///
/// User code (Clone, Drop, callbacks) runs **after** step 3, when the
/// Cell holds a valid store. Re-entrant access during user code succeeds.
/// Re-entrant access during steps 1–3 sees `None` and returns defaults.
pub(crate) struct ContextStore {
    /// Frozen parent scope chain (immutable linked list).
    pub(crate) scope_chain: Option<Arc<ScopeNode>>,
    /// Active scope's values (sparse — only keys set in this scope).
    /// For cached keys, the effective value is eagerly copied here on scope entry.
    /// For non-cached keys, only values explicitly set in this scope appear.
    pub(crate) current_values: HashMap<&'static str, Arc<dyn ContextValue>>,
    /// Name of the active scope (if named).
    pub(crate) current_name: Option<String>,
    /// Current scope depth (1 = root scope).
    pub(crate) depth: usize,
    /// Remote scope chain (from cross-process propagation).
    pub(crate) remote_chain: Arc<Vec<String>>,
    /// Scope depth at which remote_chain was installed.
    /// Local names at depth > this value are included in scope_chain().
    pub(crate) remote_chain_base_depth: usize,
}

impl ContextStore {
    /// Create a new store with an empty root scope.
    pub(crate) fn new() -> Self {
        Self {
            scope_chain: None,
            current_values: HashMap::new(),
            current_name: None,
            depth: 1,
            remote_chain: Arc::new(Vec::new()),
            remote_chain_base_depth: 0,
        }
    }

    /// Build a store from a set of values and a remote scope chain.
    /// Used by snapshot attach and task-local initialization.
    pub(crate) fn from_values_with_chain(
        values: HashMap<&'static str, Box<dyn ContextValue>>,
        remote_chain: Vec<String>,
    ) -> Self {
        let current_values: HashMap<&'static str, Arc<dyn ContextValue>> = values
            .into_iter()
            .map(|(k, v)| (k, Arc::from(v)))
            .collect();

        Self {
            scope_chain: None,
            current_values,
            current_name: None,
            depth: 1,
            remote_chain: Arc::new(remote_chain),
            remote_chain_base_depth: 1,
        }
    }

    /// Freeze the current scope into an immutable ScopeNode and push it
    /// onto the scope chain. The new scope starts with only cached keys
    /// pre-populated (their effective values are Arc::cloned in).
    /// Non-cached keys are absent — reads walk the parent chain.
    ///
    /// Returns the new depth.
    pub(crate) fn push_scope(&mut self, name: Option<String>) -> usize {
        // For cached keys, look up their effective value BEFORE freezing.
        // Since cached keys always exist in current_values (by invariant),
        // this is just a HashMap lookup + Arc::clone.
        let cached = crate::registry::cached_keys();
        let mut cached_values: Vec<(&'static str, Arc<dyn ContextValue>)> = Vec::new();
        for &key in &cached {
            if let Some(val) = self.get_value(key) {
                cached_values.push((key, val));
            }
        }

        let frozen_values = std::mem::take(&mut self.current_values);

        let node = Arc::new(ScopeNode {
            name: self.current_name.take(),
            values: frozen_values,
            parent: self.scope_chain.take(),
            depth: self.depth,
            remote_chain: Arc::clone(&self.remote_chain),
            remote_chain_base_depth: self.remote_chain_base_depth,
        });

        self.scope_chain = Some(node);
        self.current_name = name;
        self.depth += 1;

        // Pre-populate cached keys in the new scope for O(1) reads.
        for (key, val) in cached_values {
            self.current_values.insert(key, val);
        }

        self.depth
    }

    /// Pop the current scope, restoring state from the frozen ScopeNode.
    ///
    /// Returns the garbage (old current_values) to be dropped OUTSIDE
    /// the Cell window.
    pub(crate) fn pop_scope(
        &mut self,
        expected_depth: usize,
    ) -> Option<ScopeGarbage> {
        if self.depth != expected_depth || self.depth <= 1 {
            return None;
        }

        let node = self.scope_chain.take()?;

        // Take current values first — must happen BEFORE restoring from node.
        let old_current = std::mem::take(&mut self.current_values);

        // Restore from popped node. Arc::try_unwrap for zero-copy when possible.
        match Arc::try_unwrap(node) {
            Ok(owned) => {
                self.scope_chain = owned.parent;
                self.current_name = owned.name;
                self.current_values = owned.values;
                self.depth = owned.depth;
                self.remote_chain = owned.remote_chain;
                self.remote_chain_base_depth = owned.remote_chain_base_depth;
            }
            Err(shared) => {
                self.scope_chain = shared.parent.clone();
                self.current_name = shared.name.clone();
                self.current_values = shared.values.iter()
                    .map(|(&k, v)| (k, Arc::clone(v)))
                    .collect();
                self.depth = shared.depth;
                self.remote_chain = Arc::clone(&shared.remote_chain);
                self.remote_chain_base_depth = shared.remote_chain_base_depth;
            }
        }

        Some(ScopeGarbage {
            _old_values: old_current,
        })
    }

    /// Set a value in the current scope.
    pub(crate) fn set_value(
        &mut self,
        key: &'static str,
        value: Arc<dyn ContextValue>,
    ) -> Option<Arc<dyn ContextValue>> {
        self.current_values.insert(key, value)
    }

    /// Look up the effective value for a key.
    /// Checks current_values first, then walks the parent scope chain.
    /// For cached keys, the value is always in current_values (O(1)).
    /// For non-cached keys, this is O(depth).
    pub(crate) fn get_value(&self, key: &str) -> Option<Arc<dyn ContextValue>> {
        // Check current scope first.
        if let Some(v) = self.current_values.get(key) {
            return Some(Arc::clone(v));
        }
        // Walk parent chain.
        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            if let Some(v) = n.values.get(key) {
                return Some(Arc::clone(v));
            }
            node = n.parent.as_ref();
        }
        None
    }

    /// Collect all effective values for snapshot/serialization.
    /// Walks the full scope chain to build a merged view (topmost wins).
    pub(crate) fn collect_values(&self) -> HashMap<&'static str, Arc<dyn ContextValue>> {
        let mut result: HashMap<&'static str, Arc<dyn ContextValue>> = HashMap::new();

        // Current scope values (highest priority).
        for (&k, v) in &self.current_values {
            result.insert(k, Arc::clone(v));
        }

        // Walk parent chain; only insert if not already shadowed.
        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            for (&k, v) in &n.values {
                result.entry(k).or_insert_with(|| Arc::clone(v));
            }
            node = n.parent.as_ref();
        }

        result
    }

    /// Set the remote scope chain.
    pub(crate) fn set_remote_chain(&mut self, chain: Vec<String>) {
        self.remote_chain = Arc::new(chain);
        self.remote_chain_base_depth = self.depth;
    }

    /// Build the full scope chain: remote prefix + local named scope names.
    pub(crate) fn scope_chain(&self) -> Vec<String> {
        let mut local_names = Vec::new();

        // Collect current scope name (if named).
        if let Some(name) = &self.current_name {
            if self.depth > self.remote_chain_base_depth {
                local_names.push(name.clone());
            }
        }

        // Walk parent chain, collecting names above remote_chain_base_depth.
        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            if n.depth > self.remote_chain_base_depth {
                if let Some(name) = &n.name {
                    local_names.push(name.clone());
                }
            }
            node = n.parent.as_ref();
        }

        local_names.reverse();

        let max_len = crate::config::max_scope_chain_len();
        let mut chain: Vec<String> = (*self.remote_chain).clone();
        chain.extend(local_names);

        if max_len > 0 && chain.len() > max_len {
            let start = chain.len() - max_len;
            chain.drain(..start);
        }

        chain
    }
}

// ── Garbage bag ────────────────────────────────────────────────

/// Holds old values that should be dropped outside the Cell window.
/// When this struct is dropped, Arc refcounts are decremented, potentially
/// running user Drop code. By that time, the Cell has been restored.
pub(crate) struct ScopeGarbage {
    _old_values: HashMap<&'static str, Arc<dyn ContextValue>>,
}

// ── Scope guard ────────────────────────────────────────────────

/// RAII guard that reverts a scope on drop.
/// Not Send/Sync — scopes are bound to their storage context.
pub struct ScopeGuard {
    expected_depth: usize,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ScopeGuard {
    pub(crate) fn new(expected_depth: usize) -> Self {
        Self {
            expected_depth,
            _not_send: std::marker::PhantomData,
        }
    }

    /// Create a no-op guard that does nothing on drop.
    /// Used when a scope cannot be pushed (re-entrant access / busy store).
    pub(crate) fn noop() -> Self {
        Self {
            expected_depth: usize::MAX,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        crate::storage::leave_scope(self.expected_depth);
    }
}
