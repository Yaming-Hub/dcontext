//! Shared context store type used by both async (task-local) and sync (thread-local) contexts.
//!
//! `ContextStore` is the mutable state that lives inside a `Cell<Option<ContextStore>>`.
//! It is accessed via the take/set pattern for contention-free interior mutability.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::registry::Registry;
use crate::scope::ScopeNode;
use crate::value::ContextValue;

/// Maximum number of real scope levels. Beyond this, push_scope creates
/// "dead" scopes that only bump the depth counter without allocating a
/// ScopeNode. This prevents runaway recursion from exhausting memory.
pub(crate) const MAX_SCOPE_DEPTH: usize = 1024;

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
pub struct ContextStore {
    /// Frozen parent scope chain (immutable linked list).
    pub(crate) scope_chain: Option<Arc<ScopeNode>>,
    /// Active scope's values (sparse — only keys set in this scope).
    /// For cached keys, the effective value is eagerly copied here on scope entry.
    /// For non-cached keys, only values explicitly set in this scope appear.
    pub(crate) current_values: HashMap<&'static str, Arc<dyn ContextValue>>,
    /// Name of the active scope (if named).
    pub(crate) current_name: Option<String>,
    /// Current scope depth (1 = root scope).
    /// Also serves as a unique identifier for each scope: depth increments on
    /// push and is never reused, so it uniquely identifies the active scope
    /// within this store instance.
    pub(crate) depth: usize,
    /// Remote scope chain (from cross-process propagation).
    pub(crate) remote_chain: Arc<Vec<String>>,
    /// Scope depth at which remote_chain was installed.
    /// Local names at depth > this value are included in scope_chain().
    pub(crate) remote_chain_base_depth: usize,
    /// Frozen parent from a fork — a read-only ancestor scope that this
    /// store inherits from. Value lookups fall through to this parent if
    /// not found in the local scope chain. This allows forked contexts to
    /// start a fresh root scope while still seeing parent values.
    pub(crate) frozen_parent: Option<Arc<ScopeNode>>,
    /// When set, value lookups and scope_chain() stop at scopes with
    /// depth <= this value. Used by `deserialize_context` to hide the
    /// local scope tree behind a restored remote context. The barrier
    /// is saved into `ScopeNode` on push and restored on pop, so dropping
    /// the deserialize guard automatically makes parent scopes visible.
    pub(crate) scope_barrier: Option<usize>,
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
            frozen_parent: None,
            scope_barrier: None,
        }
    }

    /// Build a store from a set of values and a remote scope chain.
    /// Used by snapshot attach and task-local initialization.
    pub(crate) fn from_values_with_chain(
        values: HashMap<&'static str, Arc<dyn ContextValue>>,
        remote_chain: Vec<String>,
    ) -> Self {
        Self {
            scope_chain: None,
            current_values: values,
            current_name: None,
            depth: 1,
            remote_chain: Arc::new(remote_chain),
            remote_chain_base_depth: 1,
            frozen_parent: None,
            scope_barrier: None,
        }
    }

    /// Create a child store that inherits from this store via fork.
    ///
    /// Freezes the current scope into an immutable `ScopeNode` and creates
    /// a new root-level store whose `frozen_parent` points to it. Value
    /// lookups in the child fall through to the frozen parent chain.
    pub(crate) fn fork_child(&self) -> Self {
        // Freeze the current scope into a new ScopeNode (Arc-shared with parent).
        let frozen_values: HashMap<&'static str, Arc<dyn ContextValue>> = self
            .current_values
            .iter()
            .map(|(&k, v)| (k, Arc::clone(v)))
            .collect();

        let frozen = Arc::new(ScopeNode {
            name: self.current_name.clone(),
            values: frozen_values,
            parent: self.scope_chain.clone(),
            depth: self.depth,
            remote_chain: Arc::clone(&self.remote_chain),
            remote_chain_base_depth: self.remote_chain_base_depth,
            saved_scope_barrier: self.scope_barrier,
        });

        Self {
            scope_chain: None,
            current_values: HashMap::new(),
            current_name: None,
            depth: 1,
            remote_chain: Arc::clone(&self.remote_chain),
            remote_chain_base_depth: self.remote_chain_base_depth,
            frozen_parent: Some(frozen),
            scope_barrier: None,
        }
    }

    /// Freeze the current scope into an immutable ScopeNode and push it
    /// onto the scope chain. The new scope starts with only cached keys
    /// pre-populated (their effective values are Arc::cloned in).
    /// Non-cached keys are absent — reads walk the parent chain.
    ///
    /// If the depth exceeds [`MAX_SCOPE_DEPTH`], creates a "dead" scope
    /// that only increments the depth counter without allocating a node.
    ///
    /// Returns the new depth.
    pub(crate) fn push_scope(&mut self, registry: &Registry<'_>, name: Option<String>) -> usize {
        self.depth += 1;

        // Beyond the limit: dead scope — just bump depth, no real work.
        if self.depth > MAX_SCOPE_DEPTH + 1 {
            return self.depth;
        }

        let cached = registry.cached_keys();
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
            depth: self.depth - 1,
            remote_chain: Arc::clone(&self.remote_chain),
            remote_chain_base_depth: self.remote_chain_base_depth,
            saved_scope_barrier: self.scope_barrier,
        });

        self.scope_chain = Some(node);
        self.current_name = name;

        for (key, val) in cached_values {
            self.current_values.insert(key, val);
        }

        self.depth
    }

    /// Pop scopes down to the expected depth, restoring state from frozen ScopeNodes.
    ///
    /// - If `self.depth > expected_depth`: pops repeatedly until the expected
    ///   depth is reached. This recovers from out-of-order guard drops.
    /// - If `self.depth == expected_depth`: pops once (normal case).
    /// - If `self.depth < expected_depth`: no-op (already popped).
    ///
    /// If the current depth is above [`MAX_SCOPE_DEPTH`] + 1 (a dead scope),
    /// just decrements the counter without touching the scope chain.
    ///
    /// Returns the garbage (old current_values) to be dropped OUTSIDE
    /// the Cell window.
    pub(crate) fn pop_scope(
        &mut self,
        expected_depth: usize,
    ) -> Option<crate::scope::ScopeGarbage> {
        if self.depth < expected_depth || expected_depth <= 1 {
            return None;
        }

        let mut all_old: Option<HashMap<&'static str, Arc<dyn ContextValue>>> = None;

        while self.depth >= expected_depth && self.depth > 1 {
            // Dead scope: just decrement the counter.
            if self.depth > MAX_SCOPE_DEPTH + 1 {
                self.depth -= 1;
                continue;
            }

            let node = match self.scope_chain.take() {
                Some(n) => n,
                None => break,
            };

            let old_current = std::mem::take(&mut self.current_values);
            // Accumulate garbage from all popped scopes.
            match &mut all_old {
                Some(existing) => existing.extend(old_current),
                None => all_old = Some(old_current),
            }

            match Arc::try_unwrap(node) {
                Ok(owned) => {
                    self.scope_chain = owned.parent;
                    self.current_name = owned.name;
                    self.current_values = owned.values;
                    self.depth = owned.depth;
                    self.remote_chain = owned.remote_chain;
                    self.remote_chain_base_depth = owned.remote_chain_base_depth;
                    self.scope_barrier = owned.saved_scope_barrier;
                }
                Err(shared) => {
                    self.scope_chain = shared.parent.clone();
                    self.current_name = shared.name.clone();
                    self.current_values = shared
                        .values
                        .iter()
                        .map(|(&k, v)| (k, Arc::clone(v)))
                        .collect();
                    self.depth = shared.depth;
                    self.remote_chain = Arc::clone(&shared.remote_chain);
                    self.remote_chain_base_depth = shared.remote_chain_base_depth;
                    self.scope_barrier = shared.saved_scope_barrier;
                }
            }
        }

        all_old.map(|old| crate::scope::ScopeGarbage { _old_values: old })
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
    /// Checks current_values first, then walks the parent scope chain
    /// (stopping at the scope_barrier), then falls through to the frozen
    /// parent (if forked and no barrier is active).
    /// For cached keys, the value is always in current_values (O(1)).
    /// For non-cached keys, this is O(depth).
    pub(crate) fn get_value(&self, key: &str) -> Option<Arc<dyn ContextValue>> {
        if let Some(v) = self.current_values.get(key) {
            return Some(Arc::clone(v));
        }
        let barrier = self.scope_barrier.unwrap_or(0);
        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            if n.depth <= barrier {
                break;
            }
            if let Some(v) = n.values.get(key) {
                return Some(Arc::clone(v));
            }
            node = n.parent.as_ref();
        }
        // Fall through to frozen parent from fork (only if no barrier is active)
        if self.scope_barrier.is_none() {
            let mut node = self.frozen_parent.as_ref();
            while let Some(n) = node {
                if let Some(v) = n.values.get(key) {
                    return Some(Arc::clone(v));
                }
                node = n.parent.as_ref();
            }
        }
        None
    }

    /// Collect all effective values for snapshot/serialization.
    /// Walks the scope chain down to the barrier (or bottom), then
    /// includes frozen parent values only if no barrier is active.
    pub(crate) fn collect_values(&self) -> HashMap<&'static str, Arc<dyn ContextValue>> {
        let mut result: HashMap<&'static str, Arc<dyn ContextValue>> = HashMap::new();

        for (&k, v) in &self.current_values {
            result.insert(k, Arc::clone(v));
        }

        let barrier = self.scope_barrier.unwrap_or(0);
        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            if n.depth <= barrier {
                break;
            }
            for (&k, v) in &n.values {
                result.entry(k).or_insert_with(|| Arc::clone(v));
            }
            node = n.parent.as_ref();
        }

        // Include frozen parent values only if no barrier is active
        if self.scope_barrier.is_none() {
            let mut node = self.frozen_parent.as_ref();
            while let Some(n) = node {
                for (&k, v) in &n.values {
                    result.entry(k).or_insert_with(|| Arc::clone(v));
                }
                node = n.parent.as_ref();
            }
        }

        result
    }

    /// Build the full scope chain: frozen parent names + remote prefix + local named scope names.
    /// Stops at the scope_barrier — hidden scopes are excluded.
    pub(crate) fn scope_chain(&self) -> Vec<String> {
        let mut local_names = Vec::new();

        if let Some(name) = &self.current_name {
            if self.depth > self.remote_chain_base_depth {
                local_names.push(name.clone());
            }
        }

        let barrier = self.scope_barrier.unwrap_or(0);
        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            if n.depth <= barrier {
                break;
            }
            if n.depth > self.remote_chain_base_depth {
                if let Some(name) = &n.name {
                    local_names.push(name.clone());
                }
            }
            node = n.parent.as_ref();
        }

        local_names.reverse();

        // Include frozen parent scope names only if no barrier is active
        let mut parent_names = Vec::new();
        if self.scope_barrier.is_none() {
            let mut node = self.frozen_parent.as_ref();
            while let Some(n) = node {
                if let Some(name) = &n.name {
                    parent_names.push(name.clone());
                }
                node = n.parent.as_ref();
            }
            parent_names.reverse();
        }

        let max_len = crate::config::max_scope_chain_len();
        let mut chain: Vec<String> = (*self.remote_chain).clone();
        chain.extend(parent_names);
        chain.extend(local_names);

        if max_len > 0 && chain.len() > max_len {
            let start = chain.len() - max_len;
            chain.drain(..start);
        }

        chain
    }
}

// ── Thread-local storage ───────────────────────────────────────

thread_local! {
    pub(crate) static CONTEXT: Cell<Option<ContextStore>> =
        Cell::new(Some(ContextStore::new()));
}

/// Execute `f` with exclusive access to the thread-local context store.
pub(crate) fn try_apply<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    CONTEXT
        .try_with(|cell| {
            let mut store = cell.take()?;
            let result = f(&mut store);
            cell.set(Some(store));
            Some(result)
        })
        .unwrap_or(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_scope_depth_creates_dead_scopes() {
        let mut store = ContextStore::new();

        // Push up to the limit — all real scopes.
        for i in 0..MAX_SCOPE_DEPTH {
            let registry = crate::registry::Registry::empty();
            let depth = store.push_scope(&registry, Some(format!("scope_{}", i)));
            assert_eq!(depth, i + 2); // depth starts at 1, first push yields 2
        }
        assert_eq!(store.depth, MAX_SCOPE_DEPTH + 1);

        // Count real scope nodes in the chain.
        let real_node_count = {
            let mut count = 0usize;
            let mut node = store.scope_chain.as_ref();
            while let Some(n) = node {
                count += 1;
                node = n.parent.as_ref();
            }
            count
        };
        assert_eq!(real_node_count, MAX_SCOPE_DEPTH);

        // Push beyond the limit — dead scopes.
        let registry = crate::registry::Registry::empty();
        let dead_depth_1 = store.push_scope(&registry, Some("dead_1".to_string()));
        assert_eq!(dead_depth_1, MAX_SCOPE_DEPTH + 2);
        let dead_depth_2 = store.push_scope(&registry, Some("dead_2".to_string()));
        assert_eq!(dead_depth_2, MAX_SCOPE_DEPTH + 3);

        // Real node count should NOT grow — dead scopes are not real.
        let real_node_count_after = {
            let mut count = 0usize;
            let mut node = store.scope_chain.as_ref();
            while let Some(n) = node {
                count += 1;
                node = n.parent.as_ref();
            }
            count
        };
        assert_eq!(real_node_count_after, MAX_SCOPE_DEPTH);

        // Values set in dead scopes still work (they go in current_values).
        store.set_value("key", Arc::new("dead_val".to_string()));
        assert!(store.get_value("key").is_some());

        // Pop dead scopes — just decrements counter.
        let garbage = store.pop_scope(dead_depth_2);
        assert!(garbage.is_none()); // no real scope to pop
        assert_eq!(store.depth, MAX_SCOPE_DEPTH + 2);

        let garbage = store.pop_scope(dead_depth_1);
        assert!(garbage.is_none());
        assert_eq!(store.depth, MAX_SCOPE_DEPTH + 1);

        // Now at the limit — next pop should be a real pop.
        let real_depth = store.depth;
        let garbage = store.pop_scope(real_depth);
        assert!(garbage.is_some()); // real scope popped
        assert_eq!(store.depth, MAX_SCOPE_DEPTH);
    }

    #[test]
    fn test_max_scope_depth_values_survive_dead_pop() {
        let mut store = ContextStore::new();

        // Push one real scope and set a value.
        let registry = crate::registry::Registry::empty();
        store.push_scope(&registry, None);
        store.set_value("persistent", Arc::new(42u64));

        // Push up to and beyond the limit.
        for _ in 0..MAX_SCOPE_DEPTH + 5 {
            store.push_scope(&registry, None);
        }

        // The value set in the real scope should still be readable.
        let val = store.get_value("persistent");
        assert!(val.is_some());

        // Pop all dead scopes.
        for _ in 0..5 {
            let d = store.depth;
            store.pop_scope(d);
        }

        // Value should still be accessible after dead pops.
        assert!(store.get_value("persistent").is_some());
    }

    #[test]
    fn test_max_scope_depth_full_roundtrip() {
        let mut store = ContextStore::new();
        let mut depths = Vec::new();

        // Push well beyond the limit.
        let total = MAX_SCOPE_DEPTH + 10;
        for _ in 0..total {
            let registry = crate::registry::Registry::empty();
            let d = store.push_scope(&registry, None);
            depths.push(d);
        }
        assert_eq!(store.depth, total + 1);

        // Pop everything in reverse.
        for &d in depths.iter().rev() {
            store.pop_scope(d);
        }
        assert_eq!(store.depth, 1); // back to root
        assert!(store.scope_chain.is_none()); // no parent nodes
    }
}
