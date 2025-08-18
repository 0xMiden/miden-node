use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Display};

#[cfg(test)]
mod tests;

// DEPENDENCY GRAPH
// ================================================================================================

/// A dependency graph structure where nodes are inserted, and then made available for processing
/// once all parent nodes have been processed.
///
/// Forms the basis of our transaction and batch dependency graphs.
///
/// # Node lifecycle
/// ```text
///                                    │
///                                    │
///                      insert_pending│
///                              ┌─────▼─────┐
///                              │  pending  │────┐
///                              └─────┬─────┘    │
///                                    │          │
///                     promote_pending│          │
///                              ┌─────▼─────┐    │
///                   ┌──────────► in queue  │────│
///                   │          └─────┬─────┘    │
///   revert_processed│                │          │
///                   │    process_root│          │
///                   │          ┌─────▼─────┐    │
///                   └──────────┼ processed │────│
///                              └─────┬─────┘    │
///                                    │          │
///                     prune_processed│          │purge_subgraphs
///                              ┌─────▼─────┐    │
///                              │  <null>   ◄────┘
///                              └───────────┘
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct DependencyGraph<K, V> {
    /// Node's who's data is still pending.
    pending: BTreeSet<K>,

    /// Each node's data.
    vertices: BTreeMap<K, V>,

    /// Each node's parents. This is redundant with `children`,
    /// but we require both for efficient lookups.
    parents: BTreeMap<K, BTreeSet<K>>,

    /// Each node's children. This is redundant with `parents`,
    /// but we require both for efficient lookups.
    children: BTreeMap<K, BTreeSet<K>>,

    /// Nodes that are available to process next.
    ///
    /// Effectively this is the set of nodes which are
    /// unprocessed and whose parent's _are_ all processed.
    roots: BTreeSet<K>,

    /// Set of nodes that are already processed.
    processed: BTreeSet<K>,
}

impl<K, V> Debug for DependencyGraph<K, V>
where
    K: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DependencyGraph")
            .field("pending", &self.pending)
            .field("vertices", &self.vertices.keys())
            .field("processed", &self.processed)
            .field("roots", &self.roots)
            .field("parents", &self.parents)
            .field("children", &self.children)
            .finish()
    }
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum GraphError<K> {
    #[error("node {0} already exists")]
    DuplicateKey(K),

    #[error("parents not found: {0:?}")]
    MissingParents(BTreeSet<K>),

    #[error("nodes not found: {0:?}")]
    UnknownNodes(BTreeSet<K>),

    #[error("nodes were not yet processed: {0:?}")]
    UnprocessedNodes(BTreeSet<K>),

    #[error("nodes would be left dangling: {0:?}")]
    DanglingNodes(BTreeSet<K>),

    #[error("node {0} is not a root node")]
    InvalidRootNode(K),

    #[error("node {0} is not a pending node")]
    InvalidPendingNode(K),
}

/// This cannot be derived without enforcing `Default` bounds on K and V.
impl<K, V> Default for DependencyGraph<K, V> {
    fn default() -> Self {
        Self {
            vertices: BTreeMap::default(),
            pending: BTreeSet::default(),
            parents: BTreeMap::default(),
            children: BTreeMap::default(),
            roots: BTreeSet::default(),
            processed: BTreeSet::default(),
        }
    }
}

impl<K: Ord + Copy + Display + Debug, V: Clone> DependencyGraph<K, V> {
    /// Inserts a new pending node into the graph.
    ///
    /// # Errors
    ///
    /// Errors if the node already exists, or if any of the parents are not part of the graph.
    ///
    /// This method is atomic.
    pub fn insert_pending(&mut self, key: K, parents: BTreeSet<K>) -> Result<(), GraphError<K>> {
        if self.contains(&key) {
            return Err(GraphError::DuplicateKey(key));
        }

        let missing_parents = parents
            .iter()
            .filter(|parent| !self.contains(parent))
            .copied()
            .collect::<BTreeSet<_>>();
        if !missing_parents.is_empty() {
            return Err(GraphError::MissingParents(missing_parents));
        }

        // Inform parents of their new child.
        for parent in &parents {
            self.children.entry(*parent).or_default().insert(key);
        }
        self.pending.insert(key);
        self.parents.insert(key, parents);
        self.children.insert(key, BTreeSet::default());

        Ok(())
    }

    /// Promotes a pending node, associating it with the provided value and allowing it to be
    /// considered for processing.
    ///
    /// # Errors
    ///
    /// Errors if the given node is not pending.
    ///
    /// This method is atomic.
    pub fn promote_pending(&mut self, key: K, value: V) -> Result<(), GraphError<K>> {
        if !self.pending.remove(&key) {
            return Err(GraphError::InvalidPendingNode(key));
        }

        self.vertices.insert(key, value);
        self.try_make_root(key);

        Ok(())
    }

    /// Reverts the nodes __and their descendents__, requeuing them for processing.
    ///
    /// Descendents which are pending remain unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the given nodes:
    ///
    ///   - are not part of the graph, or
    ///   - were not previously processed
    ///
    /// This method is atomic.
    pub fn revert_subgraphs(&mut self, keys: BTreeSet<K>) -> Result<(), GraphError<K>> {
        let missing_nodes = keys
            .iter()
            .filter(|key| !self.vertices.contains_key(key))
            .copied()
            .collect::<BTreeSet<_>>();
        if !missing_nodes.is_empty() {
            return Err(GraphError::UnknownNodes(missing_nodes));
        }
        let unprocessed = keys.difference(&self.processed).copied().collect::<BTreeSet<_>>();
        if !unprocessed.is_empty() {
            return Err(GraphError::UnprocessedNodes(unprocessed));
        }

        let mut reverted = BTreeSet::new();
        let mut to_revert = keys.clone();

        while let Some(key) = to_revert.pop_first() {
            self.processed.remove(&key);

            let unprocessed_children = self
                .children
                .get(&key)
                .map(|children| children.difference(&reverted))
                .into_iter()
                .flatten()
                // We should not revert children which are pending.
                .filter(|child| self.vertices.contains_key(child))
                .copied();

            to_revert.extend(unprocessed_children);

            reverted.insert(key);
        }

        // Only the original keys and the current roots need to be considered as roots.
        //
        // The children of the input keys are disqualified by definition (they're descendents),
        // and current roots must be re-evaluated since their parents may have been requeued.
        std::mem::take(&mut self.roots)
            .into_iter()
            .chain(keys)
            .for_each(|key| self.try_make_root(key));

        Ok(())
    }

    /// Removes a set of previously processed nodes from the graph.
    ///
    /// This is used to bound the size of the graph by removing nodes once they are no longer
    /// required.
    ///
    /// # Errors
    ///
    /// Errors if
    ///   - any node is unknown
    ///   - any node is __not__ processed
    ///   - any parent node would be left unpruned
    ///
    /// The last point implies that all parents of the given nodes must either be part of the set,
    /// or already been pruned.
    ///
    /// This method is atomic.
    pub fn prune_processed(&mut self, keys: BTreeSet<K>) -> Result<Vec<V>, GraphError<K>> {
        let missing_nodes =
            keys.iter().filter(|key| !self.contains(key)).copied().collect::<BTreeSet<_>>();
        if !missing_nodes.is_empty() {
            return Err(GraphError::UnknownNodes(missing_nodes));
        }

        let unprocessed = keys.difference(&self.processed).copied().collect::<BTreeSet<_>>();
        if !unprocessed.is_empty() {
            return Err(GraphError::UnprocessedNodes(unprocessed));
        }

        // No parent may be left dangling i.e. all parents must be part of this prune set.
        let dangling = keys
            .iter()
            .filter_map(|key| self.parents.get(key))
            .flatten()
            .filter(|parent| !keys.contains(parent))
            .copied()
            .collect::<BTreeSet<_>>();
        if !dangling.is_empty() {
            return Err(GraphError::DanglingNodes(dangling));
        }

        let mut pruned = Vec::with_capacity(keys.len());

        for key in keys {
            let value = self.vertices.remove(&key).expect("Checked in precondition");
            pruned.push(value);
            self.processed.remove(&key);
            self.parents.remove(&key);

            let children = self.children.remove(&key).unwrap_or_default();

            // Remove edges from children to this node.
            for child in children {
                if let Some(child) = self.parents.get_mut(&child) {
                    child.remove(&key);
                }
            }
        }

        Ok(pruned)
    }

    /// Removes the set of nodes __and all descendents__ from the graph, returning all removed
    /// nodes. This __includes__ pending nodes.
    ///
    /// # Returns
    ///
    /// All nodes removed.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the given nodes does not exist.
    ///
    /// This method is atomic.
    pub fn purge_subgraphs(&mut self, keys: BTreeSet<K>) -> Result<BTreeSet<K>, GraphError<K>> {
        let missing_nodes =
            keys.iter().filter(|key| !self.contains(key)).copied().collect::<BTreeSet<_>>();
        if !missing_nodes.is_empty() {
            return Err(GraphError::UnknownNodes(missing_nodes));
        }

        let visited = keys.clone();
        let mut to_remove = keys;
        let mut removed = BTreeSet::new();

        while let Some(key) = to_remove.pop_first() {
            self.vertices.remove(&key);
            self.pending.remove(&key);
            removed.insert(key);

            self.processed.remove(&key);
            self.roots.remove(&key);

            // Children must also be purged. Take care not to visit them twice which is
            // possible since children can have multiple purged parents.
            let unvisited_children = self.children.remove(&key).unwrap_or_default();
            let unvisited_children = unvisited_children.difference(&visited);
            to_remove.extend(unvisited_children);

            // Inform parents that this child no longer exists.
            let parents = self.parents.remove(&key).unwrap_or_default();
            for parent in parents {
                if let Some(parent) = self.children.get_mut(&parent) {
                    parent.remove(&key);
                }
            }
        }

        Ok(removed)
    }

    /// Adds the node to the `roots` list _IFF_ all of its parents are processed.
    ///
    /// # SAFETY
    ///
    /// This method assumes the node exists. Caller is responsible for ensuring this is true.
    fn try_make_root(&mut self, key: K) {
        if self.pending.contains(&key) {
            return;
        }
        debug_assert!(
            self.vertices.contains_key(&key),
            "Potential root {key} must exist in the graph"
        );
        debug_assert!(
            !self.processed.contains(&key),
            "Potential root {key} cannot already be processed"
        );

        let all_parents_processed = self
            .parents
            .get(&key)
            .into_iter()
            .flatten()
            .all(|parent| self.processed.contains(parent));

        if all_parents_processed {
            self.roots.insert(key);
        }
    }

    /// Returns the set of nodes that are ready for processing.
    ///
    /// Nodes can be selected from here and marked as processed using [`Self::process_root`].
    pub fn roots(&self) -> &BTreeSet<K> {
        &self.roots
    }

    /// Marks a root node as processed, removing it from the roots list.
    ///
    /// The node's children are [evaluated](Self::try_make_root) as possible roots.
    ///
    /// # Error
    ///
    /// Errors if the node is not in the roots list.
    ///
    /// This method is atomic.
    pub fn process_root(&mut self, key: K) -> Result<(), GraphError<K>> {
        if !self.roots.remove(&key) {
            return Err(GraphError::InvalidRootNode(key));
        }

        self.processed.insert(key);

        self.children
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .for_each(|child| self.try_make_root(child));

        Ok(())
    }

    /// Returns the value of a node.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.vertices.get(key)
    }

    /// Returns the parents of the node, or `None` if the node does not exist.
    pub fn parents(&self, key: &K) -> Option<&BTreeSet<K>> {
        self.parents.get(key)
    }

    /// Returns true if the node exists, in either the pending or non-pending sets.
    fn contains(&self, key: &K) -> bool {
        self.pending.contains(key) || self.vertices.contains_key(key)
    }

    /// The amount of nodes which are in the pending state.
    pub fn num_pending(&self) -> usize {
        self.pending.len()
    }

    /// The amount of nodes which are in the processed state.
    pub fn num_processed(&self) -> usize {
        self.processed.len()
    }

    /// The total amount of nodes in the graph.
    pub fn len(&self) -> usize {
        self.vertices.len()
    }

    /// The amount of root nodes.
    pub fn num_roots(&self) -> usize {
        self.roots.len()
    }
}
