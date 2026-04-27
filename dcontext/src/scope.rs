use std::collections::HashMap;

use crate::value::ContextValue;

/// Maximum number of entries in a scope chain to prevent unbounded growth.
pub(crate) const MAX_SCOPE_CHAIN_LEN: usize = 64;

/// A single scope layer in the context stack.
pub(crate) struct Scope {
    /// Optional human-readable name for this scope.
    /// Named scopes appear in the scope chain; unnamed (`None`) are invisible.
    pub(crate) name: Option<String>,
    pub(crate) values: HashMap<&'static str, Box<dyn ContextValue>>,
    /// Saved remote chain + base from the parent — restored when this scope is popped.
    /// Only set by `set_remote_chain()` to provide LIFO restore semantics.
    pub(crate) saved_remote_chain: Option<(Vec<String>, usize)>,
}

impl Scope {
    pub(crate) fn new() -> Self {
        Self {
            name: None,
            values: HashMap::new(),
            saved_remote_chain: None,
        }
    }

    pub(crate) fn named(name: String) -> Self {
        Self {
            name: Some(name),
            values: HashMap::new(),
            saved_remote_chain: None,
        }
    }
}

/// The stack of scopes. Lives in thread-local or task-local storage.
///
/// ## Read cache (S3 optimization)
///
/// Lookups walk the scope stack top-down, which is O(depth). Since reads are
/// expected to dominate writes, `ContextStack` maintains an **index cache** —
/// a `HashMap` mapping each effective key to the scope index that holds its
/// current value (topmost wins).
///
/// The cache stores only `(&'static str, usize)` pairs — no user values are
/// cloned. This avoids running user `Clone` code during cache operations,
/// which is critical for C3 re-entrancy safety.
///
/// Cache maintenance:
/// - `push_scope`: no update needed (new scope is empty, doesn't shadow anything)
/// - `pop_scope`: full rebuild (popped scope may have been shadowing parent values)
/// - `set`: O(1) incremental update (insert/overwrite the key's index)
/// - `lookup`: O(1) via `HashMap::get` + direct scope access
pub(crate) struct ContextStack {
    pub(crate) scopes: Vec<Scope>,
    next_scope_id: u64,
    /// Index cache: maps each effective key to the scope index holding its value.
    /// Eagerly maintained — always valid. Stores only indices, never user data.
    read_cache: HashMap<&'static str, usize>,
    /// Scope chain received from a remote caller (or snapshot restore).
    /// Represents the sender's full scope chain at the time of serialization.
    pub(crate) remote_chain: Vec<String>,
    /// The scope index from which local names should be collected.
    /// Scopes below this index are "covered" by remote_chain.
    remote_chain_base: usize,
}

impl ContextStack {
    pub(crate) fn new() -> Self {
        Self {
            scopes: vec![Scope::new()], // root scope always present
            next_scope_id: 1,
            read_cache: HashMap::new(),
            remote_chain: Vec::new(),
            remote_chain_base: 0,
        }
    }

    pub(crate) fn push_scope(&mut self) -> (u64, usize) {
        let id = self.next_scope_id;
        self.next_scope_id += 1;
        self.scopes.push(Scope::new());
        // Cache stays valid — new scope is empty, doesn't shadow anything.
        let depth = self.scopes.len();
        (id, depth)
    }

    pub(crate) fn push_named_scope(&mut self, name: String) -> (u64, usize) {
        let id = self.next_scope_id;
        self.next_scope_id += 1;
        self.scopes.push(Scope::named(name));
        let depth = self.scopes.len();
        (id, depth)
    }

    pub(crate) fn pop_scope(&mut self, expected_depth: usize) {
        assert_eq!(
            self.scopes.len(),
            expected_depth,
            "ScopeGuard dropped out of order: expected depth {}, got {}. \
             Scopes must be exited in LIFO order.",
            expected_depth,
            self.scopes.len()
        );
        // Never pop the root scope
        if self.scopes.len() > 1 {
            if let Some(popped) = self.scopes.pop() {
                // Restore remote_chain + base if this scope saved one.
                if let Some((saved_chain, saved_base)) = popped.saved_remote_chain {
                    self.remote_chain = saved_chain;
                    self.remote_chain_base = saved_base;
                }
            }
        }
        self.rebuild_cache();
    }

    /// Look up a value by key. O(1) via the index cache.
    pub(crate) fn lookup(&self, key: &str) -> Option<&dyn ContextValue> {
        let &scope_idx = self.read_cache.get(key)?;
        self.scopes[scope_idx].values.get(key).map(|v| v.as_ref())
    }

    /// Set a value in the topmost scope. Returns the old value if any.
    /// O(1) incremental cache update.
    pub(crate) fn set(&mut self, key: &'static str, value: Box<dyn ContextValue>) -> Option<Box<dyn ContextValue>> {
        let scope_idx = self.scopes.len() - 1;
        self.read_cache.insert(key, scope_idx);
        if let Some(scope) = self.scopes.last_mut() {
            scope.values.insert(key, value)
        } else {
            None
        }
    }

    /// Rebuild the index cache from scratch.
    /// Only copies `(&'static str, usize)` pairs — no user data cloned.
    fn rebuild_cache(&mut self) {
        self.read_cache.clear();
        for (idx, scope) in self.scopes.iter().enumerate() {
            for key in scope.values.keys() {
                self.read_cache.insert(key, idx);
            }
        }
    }

    /// Collect all effective values (merged view, topmost wins).
    /// Uses the index cache to iterate only effective keys.
    pub(crate) fn merged_values(&self) -> HashMap<&'static str, &dyn ContextValue> {
        self.read_cache
            .iter()
            .filter_map(|(&key, &scope_idx)| {
                self.scopes[scope_idx]
                    .values
                    .get(key)
                    .map(|v| (key, v.as_ref()))
            })
            .collect()
    }

    /// Build a new ContextStack from a set of cloned values (for snapshots).
    #[allow(dead_code)]
    pub(crate) fn from_values(values: HashMap<&'static str, Box<dyn ContextValue>>) -> Self {
        Self::from_values_with_chain(values, Vec::new())
    }

    /// Build a new ContextStack from values and a remote scope chain.
    pub(crate) fn from_values_with_chain(
        values: HashMap<&'static str, Box<dyn ContextValue>>,
        remote_chain: Vec<String>,
    ) -> Self {
        let read_cache: HashMap<&'static str, usize> = values.keys().map(|&k| (k, 0)).collect();
        // The root scope (index 0) is the only scope; local names start after it.
        // Since from_values creates a fresh stack, the base should be at 1
        // (past the root scope) so no local names bleed into the remote chain.
        Self {
            scopes: vec![Scope { name: None, values, saved_remote_chain: None }],
            next_scope_id: 1,
            read_cache,
            remote_chain,
            remote_chain_base: 1, // local names start after root
        }
    }

    /// Set the remote scope chain. Saves the previous chain + base on the
    /// topmost scope so they can be restored when that scope is popped (LIFO safety).
    pub(crate) fn set_remote_chain(&mut self, chain: Vec<String>) {
        if let Some(top) = self.scopes.last_mut() {
            if top.saved_remote_chain.is_none() {
                top.saved_remote_chain = Some((
                    std::mem::take(&mut self.remote_chain),
                    self.remote_chain_base,
                ));
            }
        }
        self.remote_chain = chain;
        // Local names start from the CURRENT scope count (above existing scopes)
        self.remote_chain_base = self.scopes.len();
    }

    /// Collect the full scope chain: remote prefix + local named scope names.
    /// Only collects local names from scopes at or above `remote_chain_base`,
    /// since scopes below that are "covered" by the remote chain.
    pub(crate) fn scope_chain(&self) -> Vec<String> {
        let start = self.remote_chain_base.max(1); // always skip root (index 0)
        let local_names = self.scopes.iter()
            .skip(start)
            .filter_map(|s| s.name.as_ref().cloned())
            .collect::<Vec<_>>();

        let total = self.remote_chain.len() + local_names.len();
        if total <= MAX_SCOPE_CHAIN_LEN {
            let mut chain = self.remote_chain.clone();
            chain.extend(local_names);
            chain
        } else {
            // Truncate: keep the most recent entries (drop oldest from remote prefix).
            let mut chain = self.remote_chain.clone();
            chain.extend(local_names);
            let start = chain.len().saturating_sub(MAX_SCOPE_CHAIN_LEN);
            chain.drain(..start);
            chain
        }
    }
}

/// RAII guard that reverts a scope on drop.
/// Not Send/Sync — scopes are bound to their storage context.
pub struct ScopeGuard {
    #[allow(dead_code)]
    scope_id: u64,
    expected_depth: usize,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ScopeGuard {
    pub(crate) fn new(scope_id: u64, expected_depth: usize) -> Self {
        Self {
            scope_id,
            expected_depth,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        crate::storage::leave_scope(self.expected_depth);
    }
}
