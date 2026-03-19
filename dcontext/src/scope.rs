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
pub(crate) struct ContextStack {
    pub(crate) scopes: Vec<Scope>,
    next_scope_id: u64,
}

impl ContextStack {
    pub(crate) fn new() -> Self {
        Self {
            scopes: vec![Scope::new()], // root scope always present
            next_scope_id: 1,
        }
    }

    pub(crate) fn push_scope(&mut self) -> (u64, usize) {
        let id = self.next_scope_id;
        self.next_scope_id += 1;
        self.scopes.push(Scope::new());
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
    }

    /// Look up a value by key, walking scopes top-down.
    pub(crate) fn lookup(&self, key: &str) -> Option<&dyn ContextValue> {
        for scope in self.scopes.iter().rev() {
            if let Some(val) = scope.values.get(key) {
                return Some(val.as_ref());
            }
        }
        None
    }

    /// Set a value in the topmost scope.
    pub(crate) fn set(&mut self, key: &'static str, value: Box<dyn ContextValue>) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.values.insert(key, value);
        }
    }

    /// Collect all effective values (merged view, topmost wins).
    pub(crate) fn merged_values(&self) -> HashMap<&'static str, &dyn ContextValue> {
        let mut merged: HashMap<&'static str, &dyn ContextValue> = HashMap::new();
        for scope in &self.scopes {
            for (key, val) in &scope.values {
                merged.insert(key, val.as_ref());
            }
        }
        merged
    }

    /// Build a new ContextStack from a set of cloned values (for snapshots).
    pub(crate) fn from_values(values: HashMap<&'static str, Box<dyn ContextValue>>) -> Self {
        Self {
            scopes: vec![Scope { values }],
            next_scope_id: 1,
        }
    }
}

/// RAII guard that reverts a scope on drop.
pub struct ScopeGuard {
    #[allow(dead_code)]
    scope_id: u64,
    expected_depth: usize,
}

impl ScopeGuard {
    pub(crate) fn new(scope_id: u64, expected_depth: usize) -> Self {
        Self {
            scope_id,
            expected_depth,
        }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        crate::storage::leave_scope(self.expected_depth);
    }
}
