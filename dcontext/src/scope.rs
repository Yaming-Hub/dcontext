use std::collections::HashMap;

use crate::value::ContextValue;

/// A single scope layer in the context stack.
pub(crate) struct Scope {
    pub(crate) values: HashMap<&'static str, Box<dyn ContextValue>>,
}

impl Scope {
    pub(crate) fn new() -> Self {
        Self {
            values: HashMap::new(),
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
}

impl ContextStack {
    pub(crate) fn new() -> Self {
        Self {
            scopes: vec![Scope::new()], // root scope always present
            next_scope_id: 1,
            read_cache: HashMap::new(),
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
            self.scopes.pop();
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
    pub(crate) fn from_values(values: HashMap<&'static str, Box<dyn ContextValue>>) -> Self {
        let read_cache: HashMap<&'static str, usize> = values.keys().map(|&k| (k, 0)).collect();
        Self {
            scopes: vec![Scope { values }],
            next_scope_id: 1,
            read_cache,
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
