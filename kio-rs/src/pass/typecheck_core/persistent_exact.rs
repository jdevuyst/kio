use std::sync::Arc;

/// Persistent exact immutable index for sparse compiler identity maps.
///
/// A key's route hash selects a bounded trie path, but equality always falls
/// back to the key's exact structural relation. Hash collisions therefore
/// affect work only; they can never change proof identity or an outcome.
pub(crate) trait PersistentExactKey: Clone {
    /// Exact-equal keys must return the same route hash. The hash selects only
    /// a trie path, but lookup deliberately compares exact identity only after
    /// that path has matched.
    fn persistent_route_hash(&self) -> u64;
    fn persistent_exact_eq(&self, other: &Self) -> bool;
}

#[derive(Clone)]
pub(crate) struct PersistentExactIndex<K, V> {
    root: Option<Arc<PersistentExactIndexNode<K, V>>>,
    len: usize,
}

enum PersistentExactIndexNode<K, V> {
    Branch {
        bitmap: u16,
        children: Arc<[Arc<PersistentExactIndexNode<K, V>>]>,
    },
    Bucket(Arc<[PersistentExactIndexEntry<K, V>]>),
}

#[derive(Clone)]
struct PersistentExactIndexEntry<K, V> {
    route_hash: u64,
    key: K,
    value: V,
}

impl<K, V> Default for PersistentExactIndex<K, V> {
    fn default() -> Self {
        Self { root: None, len: 0 }
    }
}

impl<K: PersistentExactKey, V: Clone> PersistentExactIndex<K, V> {
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.get_with_route_hash(key, key.persistent_route_hash())
    }

    fn get_with_route_hash(&self, key: &K, route_hash: u64) -> Option<&V> {
        Self::get_node(self.root.as_deref()?, route_hash, key, 0)
    }

    fn get_node<'a>(
        node: &'a PersistentExactIndexNode<K, V>,
        route_hash: u64,
        key: &K,
        shift: u32,
    ) -> Option<&'a V> {
        match node {
            PersistentExactIndexNode::Bucket(entries) => entries
                .iter()
                .find(|entry| entry.route_hash == route_hash && entry.key.persistent_exact_eq(key))
                .map(|entry| &entry.value),
            PersistentExactIndexNode::Branch { bitmap, children } => {
                let bit = 1_u16 << ((route_hash >> shift) & 0x0f);
                if bitmap & bit == 0 {
                    return None;
                }
                let index = (bitmap & (bit - 1)).count_ones() as usize;
                Self::get_node(&children[index], route_hash, key, shift + 4)
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[cfg(any(feature = "surface", test))]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn for_each(&self, mut visit: impl FnMut(&K, &V)) {
        if let Some(root) = &self.root {
            Self::for_each_node(root, &mut visit);
        }
    }

    fn for_each_node(node: &PersistentExactIndexNode<K, V>, visit: &mut impl FnMut(&K, &V)) {
        match node {
            PersistentExactIndexNode::Branch { children, .. } => {
                for child in children.iter() {
                    Self::for_each_node(child, visit);
                }
            }
            PersistentExactIndexNode::Bucket(entries) => {
                for entry in entries.iter() {
                    visit(&entry.key, &entry.value);
                }
            }
        }
    }

    /// Whether every exact entry in `self` is present in `superset`.
    ///
    /// Both maps normally descend from one immutable root. Shared subtries are
    /// accepted by pointer identity, so a lexical one-binding extension walks
    /// only its changed route rather than rescanning the ambient scope.
    #[cfg(any(feature = "surface", test))]
    pub(crate) fn is_submap_of(
        &self,
        superset: &Self,
        values_equal: impl Fn(&V, &V) -> bool,
    ) -> bool {
        if self.len > superset.len {
            return false;
        }
        match (&self.root, &superset.root) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(subset), Some(superset)) => {
                Self::is_node_submap_of(subset, superset, 0, &values_equal)
            }
        }
    }

    #[cfg(any(feature = "surface", test))]
    fn is_node_submap_of(
        subset: &Arc<PersistentExactIndexNode<K, V>>,
        superset: &Arc<PersistentExactIndexNode<K, V>>,
        shift: u32,
        values_equal: &impl Fn(&V, &V) -> bool,
    ) -> bool {
        if Arc::ptr_eq(subset, superset) {
            return true;
        }
        #[cfg(test)]
        PERSISTENT_EXACT_SUBMAP_CHANGED_PAIRS.with(|count| count.set(count.get() + 1));
        match (subset.as_ref(), superset.as_ref()) {
            (PersistentExactIndexNode::Bucket(entries), _) => entries.iter().all(|entry| {
                Self::get_node(superset, entry.route_hash, &entry.key, shift)
                    .is_some_and(|value| values_equal(&entry.value, value))
            }),
            (
                PersistentExactIndexNode::Branch {
                    bitmap: subset_bitmap,
                    children: subset_children,
                },
                PersistentExactIndexNode::Branch {
                    bitmap: superset_bitmap,
                    children: superset_children,
                },
            ) => {
                let mut subset_index = 0;
                for nibble in 0..16 {
                    let bit = 1_u16 << nibble;
                    if subset_bitmap & bit == 0 {
                        continue;
                    }
                    if superset_bitmap & bit == 0 {
                        return false;
                    }
                    let superset_index = (superset_bitmap & (bit - 1)).count_ones() as usize;
                    if !Self::is_node_submap_of(
                        &subset_children[subset_index],
                        &superset_children[superset_index],
                        shift + 4,
                        values_equal,
                    ) {
                        return false;
                    }
                    subset_index += 1;
                }
                true
            }
            (PersistentExactIndexNode::Branch { .. }, PersistentExactIndexNode::Bucket(_)) => {
                let mut matches = true;
                Self::for_each_node(subset, &mut |key, value| {
                    if matches {
                        matches = Self::get_node(superset, key.persistent_route_hash(), key, shift)
                            .is_some_and(|other| values_equal(value, other));
                    }
                });
                matches
            }
        }
    }

    pub(crate) fn insert_with(
        &self,
        key: K,
        value: V,
        values_equal: impl Fn(&V, &V) -> bool,
    ) -> Result<Self, ()> {
        self.insert_entry_with_route_hash(
            key.clone(),
            value,
            key.persistent_route_hash(),
            values_equal,
        )
    }

    /// Replace the value for one existing exact key without mutating the
    /// retained root. Returns `None` when the key is absent.
    pub(crate) fn replace(&self, key: &K, value: V) -> Option<Self> {
        let route_hash = key.persistent_route_hash();
        let root = Self::replace_node(self.root.as_ref()?, route_hash, key, value, 0)?;
        Some(Self {
            root: Some(root),
            len: self.len,
        })
    }

    fn replace_node(
        node: &Arc<PersistentExactIndexNode<K, V>>,
        route_hash: u64,
        key: &K,
        value: V,
        shift: u32,
    ) -> Option<Arc<PersistentExactIndexNode<K, V>>> {
        #[cfg(test)]
        PERSISTENT_EXACT_CHANGED_PATH_VISITS.with(|count| count.set(count.get() + 1));
        match node.as_ref() {
            PersistentExactIndexNode::Bucket(entries) => {
                let index = entries.iter().position(|entry| {
                    entry.route_hash == route_hash && entry.key.persistent_exact_eq(key)
                })?;
                let mut updated = entries.to_vec();
                updated[index].value = value;
                Some(Arc::new(PersistentExactIndexNode::Bucket(updated.into())))
            }
            PersistentExactIndexNode::Branch { bitmap, children } => {
                let bit = 1_u16 << ((route_hash >> shift) & 0x0f);
                if bitmap & bit == 0 {
                    return None;
                }
                let index = (bitmap & (bit - 1)).count_ones() as usize;
                let mut updated = children.to_vec();
                updated[index] =
                    Self::replace_node(&children[index], route_hash, key, value, shift + 4)?;
                Some(Arc::new(PersistentExactIndexNode::Branch {
                    bitmap: *bitmap,
                    children: updated.into(),
                }))
            }
        }
    }

    fn insert_entry_with_route_hash(
        &self,
        key: K,
        value: V,
        route_hash: u64,
        values_equal: impl Fn(&V, &V) -> bool,
    ) -> Result<Self, ()> {
        let entry = PersistentExactIndexEntry {
            route_hash,
            key,
            value,
        };
        let (root, inserted) = Self::insert_node(self.root.as_ref(), entry, 0, &values_equal)?;
        Ok(Self {
            root: Some(root),
            len: self.len + usize::from(inserted),
        })
    }

    fn insert_node(
        node: Option<&Arc<PersistentExactIndexNode<K, V>>>,
        entry: PersistentExactIndexEntry<K, V>,
        shift: u32,
        values_equal: &impl Fn(&V, &V) -> bool,
    ) -> Result<(Arc<PersistentExactIndexNode<K, V>>, bool), ()> {
        #[cfg(test)]
        PERSISTENT_EXACT_CHANGED_PATH_VISITS.with(|count| count.set(count.get() + 1));
        let Some(node) = node else {
            return Ok((
                Arc::new(PersistentExactIndexNode::Bucket(Arc::from([entry]))),
                true,
            ));
        };
        match node.as_ref() {
            PersistentExactIndexNode::Bucket(entries) => {
                if let Some(existing) = entries.iter().find(|existing| {
                    existing.route_hash == entry.route_hash
                        && existing.key.persistent_exact_eq(&entry.key)
                }) {
                    return values_equal(&existing.value, &entry.value)
                        .then(|| (node.clone(), false))
                        .ok_or(());
                }
                if shift >= 64 {
                    let mut extended = entries.to_vec();
                    extended.push(entry);
                    return Ok((
                        Arc::new(PersistentExactIndexNode::Bucket(extended.into())),
                        true,
                    ));
                }
                let mut extended = entries.to_vec();
                extended.push(entry);
                Ok((Self::build_node(extended, shift), true))
            }
            PersistentExactIndexNode::Branch { bitmap, children } => {
                let nibble = ((entry.route_hash >> shift) & 0x0f) as u16;
                let bit = 1_u16 << nibble;
                let index = (bitmap & (bit - 1)).count_ones() as usize;
                let mut updated = children.to_vec();
                let inserted;
                let updated_bitmap;
                if bitmap & bit == 0 {
                    updated.insert(
                        index,
                        Arc::new(PersistentExactIndexNode::Bucket(Arc::from([entry]))),
                    );
                    inserted = true;
                    updated_bitmap = bitmap | bit;
                } else {
                    let (child, did_insert) =
                        Self::insert_node(Some(&children[index]), entry, shift + 4, values_equal)?;
                    updated[index] = child;
                    inserted = did_insert;
                    updated_bitmap = *bitmap;
                }
                Ok((
                    Arc::new(PersistentExactIndexNode::Branch {
                        bitmap: updated_bitmap,
                        children: updated.into(),
                    }),
                    inserted,
                ))
            }
        }
    }

    pub(crate) fn remove(&self, key: &K) -> Self {
        let Some(root) = &self.root else {
            return self.clone();
        };
        let (root, removed) = Self::remove_node(root, key.persistent_route_hash(), key, 0);
        if !removed {
            return self.clone();
        }
        Self {
            root,
            len: self
                .len
                .checked_sub(1)
                .expect("a removed proof was present in the index"),
        }
    }

    fn remove_node(
        node: &Arc<PersistentExactIndexNode<K, V>>,
        route_hash: u64,
        key: &K,
        shift: u32,
    ) -> (Option<Arc<PersistentExactIndexNode<K, V>>>, bool) {
        match node.as_ref() {
            PersistentExactIndexNode::Bucket(entries) => {
                let Some(index) = entries.iter().position(|entry| {
                    entry.route_hash == route_hash && entry.key.persistent_exact_eq(key)
                }) else {
                    return (Some(node.clone()), false);
                };
                if entries.len() == 1 {
                    return (None, true);
                }
                let mut retained = entries.to_vec();
                retained.remove(index);
                (
                    Some(Arc::new(PersistentExactIndexNode::Bucket(retained.into()))),
                    true,
                )
            }
            PersistentExactIndexNode::Branch { bitmap, children } => {
                let bit = 1_u16 << ((route_hash >> shift) & 0x0f);
                if bitmap & bit == 0 {
                    return (Some(node.clone()), false);
                }
                let index = (bitmap & (bit - 1)).count_ones() as usize;
                let (child, removed) =
                    Self::remove_node(&children[index], route_hash, key, shift + 4);
                if !removed {
                    return (Some(node.clone()), false);
                }
                let mut updated = children.to_vec();
                let updated_bitmap = if let Some(child) = child {
                    updated[index] = child;
                    *bitmap
                } else {
                    updated.remove(index);
                    bitmap & !bit
                };
                if updated.is_empty() {
                    (None, true)
                } else {
                    (
                        Some(Arc::new(PersistentExactIndexNode::Branch {
                            bitmap: updated_bitmap,
                            children: updated.into(),
                        })),
                        true,
                    )
                }
            }
        }
    }

    fn build_node(
        entries: Vec<PersistentExactIndexEntry<K, V>>,
        shift: u32,
    ) -> Arc<PersistentExactIndexNode<K, V>> {
        if entries.len() == 1 || shift >= 64 {
            return Arc::new(PersistentExactIndexNode::Bucket(entries.into()));
        }
        let mut groups: [Vec<PersistentExactIndexEntry<K, V>>; 16] = Default::default();
        for entry in entries {
            let nibble = ((entry.route_hash >> shift) & 0x0f) as usize;
            groups[nibble].push(entry);
        }
        let mut bitmap = 0_u16;
        let mut children = Vec::new();
        for (nibble, group) in groups.into_iter().enumerate() {
            if group.is_empty() {
                continue;
            }
            bitmap |= 1_u16 << nibble;
            children.push(Self::build_node(group, shift + 4));
        }
        Arc::new(PersistentExactIndexNode::Branch {
            bitmap,
            children: children.into(),
        })
    }
}

fn persistent_string_route_hash(name: &str) -> u64 {
    name.as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

impl PersistentExactKey for String {
    fn persistent_route_hash(&self) -> u64 {
        persistent_string_route_hash(self)
    }

    fn persistent_exact_eq(&self, other: &Self) -> bool {
        self == other
    }
}

impl<V: Clone> PersistentExactIndex<String, V> {
    /// Query an owned exact-name map without allocating a temporary `String`.
    pub(crate) fn get_str(&self, key: &str) -> Option<&V> {
        let route_hash = persistent_string_route_hash(key);
        Self::get_str_node(self.root.as_deref()?, route_hash, key, 0)
    }

    fn get_str_node<'a>(
        node: &'a PersistentExactIndexNode<String, V>,
        route_hash: u64,
        key: &str,
        shift: u32,
    ) -> Option<&'a V> {
        match node {
            PersistentExactIndexNode::Bucket(entries) => entries
                .iter()
                .find(|entry| entry.route_hash == route_hash && entry.key == key)
                .map(|entry| &entry.value),
            PersistentExactIndexNode::Branch { bitmap, children } => {
                let bit = 1_u16 << ((route_hash >> shift) & 0x0f);
                if bitmap & bit == 0 {
                    return None;
                }
                let index = (bitmap & (bit - 1)).count_ones() as usize;
                Self::get_str_node(&children[index], route_hash, key, shift + 4)
            }
        }
    }
}

/// Immutable exact-name map that retains the prior same-spelled binding at
/// every shadowing step.
///
/// The visible map remains a single persistent trie. A lexical child replaces
/// one visible value with a frame pointing at its predecessor; retaining the
/// parent map restores the complete outer prefix without mutation or copying.
pub(crate) struct PersistentExactNameMap<V> {
    visible: PersistentExactIndex<String, Arc<PersistentExactNameFrame<V>>>,
}

impl<V> Clone for PersistentExactNameMap<V> {
    fn clone(&self) -> Self {
        Self {
            visible: self.visible.clone(),
        }
    }
}

impl<V> Default for PersistentExactNameMap<V> {
    fn default() -> Self {
        Self {
            visible: PersistentExactIndex::default(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PersistentExactNameFrame<V> {
    value: V,
    #[cfg(any(feature = "surface", test))]
    shadowed: Option<Arc<PersistentExactNameFrame<V>>>,
}

#[cfg(any(feature = "surface", test))]
type PersistentExactNameNode<V> =
    Arc<PersistentExactIndexNode<String, Arc<PersistentExactNameFrame<V>>>>;
#[cfg(any(feature = "surface", test))]
type PersistentExactNameMerge<V> = Result<(PersistentExactNameNode<V>, usize), ()>;

impl<V> PersistentExactNameFrame<V> {
    pub(crate) fn value(&self) -> &V {
        &self.value
    }

    #[cfg(test)]
    pub(crate) fn shadowed(&self) -> Option<&Self> {
        self.shadowed.as_deref()
    }

    #[cfg(any(feature = "surface", test))]
    fn contains(&self, expected: &Self, values_equal: &impl Fn(&V, &V) -> bool) -> bool {
        let mut current = Some(self);
        while let Some(frame) = current {
            if std::ptr::eq(frame, expected) || values_equal(&frame.value, &expected.value) {
                return true;
            }
            current = frame.shadowed.as_deref();
        }
        false
    }
}

impl<V> PersistentExactNameMap<V> {
    /// Whether two views retain the identical persistent shadow history.
    #[cfg(feature = "surface")]
    pub(crate) fn shares_root_with(&self, other: &Self) -> bool {
        match (&self.visible.root, &other.visible.root) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    #[cfg(any(feature = "surface", test))]
    pub(crate) fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    #[cfg(any(feature = "surface", test))]
    pub(crate) fn len(&self) -> usize {
        self.visible.len()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&V> {
        self.visible.get_str(name).map(|frame| frame.value())
    }

    #[cfg(test)]
    pub(crate) fn frame(&self, name: &str) -> Option<&PersistentExactNameFrame<V>> {
        self.visible.get_str(name).map(Arc::as_ref)
    }

    pub(crate) fn for_each(&self, mut visit: impl FnMut(&str, &V)) {
        self.visible
            .for_each(|name, frame| visit(name, frame.value()));
    }

    pub(crate) fn push(
        &self,
        name: String,
        value: V,
        values_equal: impl Fn(&V, &V) -> bool,
    ) -> Self {
        let visible = self.visible.get_str(&name);
        if visible
            .map(Arc::as_ref)
            .is_some_and(|frame| values_equal(frame.value(), &value))
        {
            return self.clone();
        }
        #[cfg(any(feature = "surface", test))]
        let shadowed = visible.cloned();
        let frame = Arc::new(PersistentExactNameFrame {
            value,
            #[cfg(any(feature = "surface", test))]
            shadowed,
        });
        let visible = if self.visible.get_str(&name).is_some() {
            self.visible
                .replace(&name, frame)
                .expect("the exact visible name was just observed")
        } else {
            self.visible
                .insert_with(name, frame, |left, right| {
                    values_equal(left.value(), right.value())
                })
                .expect("a new exact name has no conflicting value")
        };
        Self { visible }
    }

    /// Remove the innermost exact binding and restore its same-spelled outer
    /// frame, if any. This is the persistent counterpart of leaving one
    /// lexical binder scope.
    #[cfg(feature = "surface")]
    pub(crate) fn remove(&mut self, name: &str) -> Option<V>
    where
        V: Clone,
    {
        let key = name.to_owned();
        let current = self.visible.get_str(name)?.clone();
        self.visible = match &current.shadowed {
            Some(shadowed) => self
                .visible
                .replace(&key, shadowed.clone())
                .expect("the visible exact-name frame was just observed"),
            None => self.visible.remove(&key),
        };
        Some(current.value.clone())
    }

    #[cfg(any(feature = "surface", test))]
    pub(crate) fn extends(&self, parent: &Self, values_equal: impl Fn(&V, &V) -> bool) -> bool {
        parent.visible.is_submap_of(&self.visible, |parent, child| {
            child.contains(parent, &values_equal)
        })
    }

    #[cfg(feature = "surface")]
    pub(crate) fn visible_eq(&self, other: &Self, values_equal: impl Fn(&V, &V) -> bool) -> bool {
        self.len() == other.len()
            && self.visible.is_submap_of(&other.visible, |left, right| {
                values_equal(left.value(), right.value())
            })
    }

    #[cfg(any(feature = "surface", test))]
    pub(crate) fn merged(
        &self,
        other: &Self,
        values_equal: impl Fn(&V, &V) -> bool,
    ) -> Result<Self, ()> {
        match (&self.visible.root, &other.visible.root) {
            (None, _) => Ok(other.clone()),
            (_, None) => Ok(self.clone()),
            (Some(left), Some(right)) => {
                let (root, added) = Self::merge_visible_nodes(left, right, 0, &values_equal)?;
                Ok(Self {
                    visible: PersistentExactIndex {
                        root: Some(root),
                        len: self.visible.len + added,
                    },
                })
            }
        }
    }

    #[cfg(any(feature = "surface", test))]
    fn merge_visible_nodes(
        left: &PersistentExactNameNode<V>,
        right: &PersistentExactNameNode<V>,
        shift: u32,
        values_equal: &impl Fn(&V, &V) -> bool,
    ) -> PersistentExactNameMerge<V> {
        if Arc::ptr_eq(left, right) {
            return Ok((left.clone(), 0));
        }
        #[cfg(test)]
        PERSISTENT_EXACT_MERGE_NODE_PAIRS.with(|count| count.set(count.get() + 1));
        match (left.as_ref(), right.as_ref()) {
            (
                PersistentExactIndexNode::Bucket(left_entries),
                PersistentExactIndexNode::Bucket(right_entries),
            ) => {
                let mut merged = left_entries.to_vec();
                let mut added = 0;
                let mut changed = false;
                for right_entry in right_entries.iter() {
                    let existing = merged.iter_mut().find(|left_entry| {
                        left_entry.route_hash == right_entry.route_hash
                            && left_entry.key == right_entry.key
                    });
                    let Some(left_entry) = existing else {
                        merged.push(right_entry.clone());
                        added += 1;
                        #[cfg(test)]
                        PERSISTENT_EXACT_MERGE_ADDED_ENTRIES
                            .with(|count| count.set(count.get() + 1));
                        changed = true;
                        continue;
                    };
                    if Arc::ptr_eq(&left_entry.value, &right_entry.value)
                        || left_entry.value.contains(&right_entry.value, values_equal)
                    {
                        continue;
                    }
                    if right_entry.value.contains(&left_entry.value, values_equal) {
                        left_entry.value = right_entry.value.clone();
                        changed = true;
                        continue;
                    }
                    return Err(());
                }
                if !changed {
                    return Ok((left.clone(), 0));
                }
                Ok((PersistentExactIndex::build_node(merged, shift), added))
            }
            (
                PersistentExactIndexNode::Branch {
                    bitmap: left_bitmap,
                    children: left_children,
                },
                PersistentExactIndexNode::Branch {
                    bitmap: right_bitmap,
                    children: right_children,
                },
            ) => {
                let bitmap = left_bitmap | right_bitmap;
                let mut children = Vec::with_capacity(bitmap.count_ones() as usize);
                let mut added = 0;
                let mut changed = false;
                for nibble in 0..16 {
                    let bit = 1_u16 << nibble;
                    if bitmap & bit == 0 {
                        continue;
                    }
                    let left_child = (left_bitmap & bit != 0)
                        .then(|| &left_children[(left_bitmap & (bit - 1)).count_ones() as usize]);
                    let right_child = (right_bitmap & bit != 0)
                        .then(|| &right_children[(right_bitmap & (bit - 1)).count_ones() as usize]);
                    match (left_child, right_child) {
                        (Some(left_child), Some(right_child)) => {
                            let (child, child_added) = Self::merge_visible_nodes(
                                left_child,
                                right_child,
                                shift + 4,
                                values_equal,
                            )?;
                            changed |= !Arc::ptr_eq(&child, left_child);
                            added += child_added;
                            children.push(child);
                        }
                        (Some(left_child), None) => children.push(left_child.clone()),
                        (None, Some(right_child)) => {
                            let child_added = Self::visible_entry_count(right_child);
                            #[cfg(test)]
                            PERSISTENT_EXACT_MERGE_ADDED_ENTRIES
                                .with(|count| count.set(count.get() + child_added));
                            added += child_added;
                            changed = true;
                            children.push(right_child.clone());
                        }
                        (None, None) => unreachable!("the merged bitmap contains this nibble"),
                    }
                }
                if !changed && bitmap == *left_bitmap {
                    return Ok((left.clone(), 0));
                }
                Ok((
                    Arc::new(PersistentExactIndexNode::Branch {
                        bitmap,
                        children: children.into(),
                    }),
                    added,
                ))
            }
            (
                PersistentExactIndexNode::Bucket(left_entries),
                PersistentExactIndexNode::Branch { .. },
            ) => {
                debug_assert!(shift < 64 && !left_entries.is_empty());
                let nibble = ((left_entries[0].route_hash >> shift) & 0x0f) as u16;
                let left_branch = Arc::new(PersistentExactIndexNode::Branch {
                    bitmap: 1_u16 << nibble,
                    children: Arc::from([left.clone()]),
                });
                Self::merge_visible_nodes(&left_branch, right, shift, values_equal)
            }
            (
                PersistentExactIndexNode::Branch { .. },
                PersistentExactIndexNode::Bucket(right_entries),
            ) => {
                debug_assert!(shift < 64 && !right_entries.is_empty());
                let nibble = ((right_entries[0].route_hash >> shift) & 0x0f) as u16;
                let right_branch = Arc::new(PersistentExactIndexNode::Branch {
                    bitmap: 1_u16 << nibble,
                    children: Arc::from([right.clone()]),
                });
                Self::merge_visible_nodes(left, &right_branch, shift, values_equal)
            }
        }
    }

    #[cfg(any(feature = "surface", test))]
    fn visible_entry_count(
        node: &PersistentExactIndexNode<String, Arc<PersistentExactNameFrame<V>>>,
    ) -> usize {
        match node {
            PersistentExactIndexNode::Branch { children, .. } => children
                .iter()
                .map(|child| Self::visible_entry_count(child))
                .sum(),
            PersistentExactIndexNode::Bucket(entries) => entries.len(),
        }
    }
}

#[cfg(test)]
thread_local! {
    static PERSISTENT_EXACT_CHANGED_PATH_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PERSISTENT_EXACT_SUBMAP_CHANGED_PAIRS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PERSISTENT_EXACT_MERGE_NODE_PAIRS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PERSISTENT_EXACT_MERGE_ADDED_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_persistent_exact_work() {
    PERSISTENT_EXACT_CHANGED_PATH_VISITS.with(|count| count.set(0));
    PERSISTENT_EXACT_SUBMAP_CHANGED_PAIRS.with(|count| count.set(0));
    PERSISTENT_EXACT_MERGE_NODE_PAIRS.with(|count| count.set(0));
    PERSISTENT_EXACT_MERGE_ADDED_ENTRIES.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn persistent_exact_work() -> (usize, usize, usize, usize) {
    (
        PERSISTENT_EXACT_CHANGED_PATH_VISITS.with(std::cell::Cell::get),
        PERSISTENT_EXACT_SUBMAP_CHANGED_PAIRS.with(std::cell::Cell::get),
        PERSISTENT_EXACT_MERGE_NODE_PAIRS.with(std::cell::Cell::get),
        PERSISTENT_EXACT_MERGE_ADDED_ENTRIES.with(std::cell::Cell::get),
    )
}

#[derive(Clone)]
pub(crate) struct PersistentExactSet<K>(PersistentExactIndex<K, ()>);

impl<K> Default for PersistentExactSet<K> {
    fn default() -> Self {
        Self(PersistentExactIndex::default())
    }
}

impl<K: PersistentExactKey> PersistentExactSet<K> {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn insert(&self, key: K) -> Self {
        Self(
            self.0
                .insert_with(key, (), |(), ()| true)
                .expect("an exact set cannot contain conflicting values"),
        )
    }

    pub(crate) fn contains(&self, key: &K) -> bool {
        self.0.get(key).is_some()
    }
}

impl<K> std::fmt::Debug for PersistentExactSet<K> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PersistentExactSet")
            .field(&self.0.len)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct CollisionKey(u8);

    impl PersistentExactKey for CollisionKey {
        fn persistent_route_hash(&self) -> u64 {
            0x1234_5678_9abc_def0
        }

        fn persistent_exact_eq(&self, other: &Self) -> bool {
            self.0 == other.0
        }
    }

    #[test]
    fn exact_identity_disambiguates_route_hash_collisions() {
        let empty = PersistentExactIndex::default();
        let one = empty
            .insert_with(CollisionKey(1), "one", |left, right| left == right)
            .expect("first exact key");
        let both = one
            .insert_with(CollisionKey(2), "two", |left, right| left == right)
            .expect("colliding exact key");

        assert_eq!(both.get(&CollisionKey(1)), Some(&"one"));
        assert_eq!(both.get(&CollisionKey(2)), Some(&"two"));
        assert_eq!(both.get(&CollisionKey(3)), None);

        let retained = both.remove(&CollisionKey(1));
        assert_eq!(retained.get(&CollisionKey(1)), None);
        assert_eq!(retained.get(&CollisionKey(2)), Some(&"two"));
    }

    #[test]
    fn duplicate_exact_keys_require_one_value_and_reuse_storage() {
        let one = PersistentExactIndex::default()
            .insert_with(CollisionKey(1), 7_u8, |left, right| left == right)
            .expect("first exact value");
        let same = one
            .insert_with(CollisionKey(1), 7_u8, |left, right| left == right)
            .expect("same exact value");
        assert!(
            matches!((&one.root, &same.root), (Some(left), Some(right)) if Arc::ptr_eq(left, right))
        );
        assert!(
            one.insert_with(CollisionKey(1), 8_u8, |left, right| left == right)
                .is_err(),
            "one exact key cannot acquire a conflicting value"
        );
    }

    #[test]
    fn owned_string_keys_support_borrowed_lookup_and_immutable_replacement() {
        let empty = PersistentExactIndex::default();
        let one = empty
            .insert_with("A".to_owned(), 1_u8, |left, right| left == right)
            .expect("first exact string key");
        let replaced = one
            .replace(&"A".to_owned(), 2_u8)
            .expect("existing exact string key");

        assert_eq!(one.get_str("A"), Some(&1));
        assert_eq!(replaced.get_str("A"), Some(&2));
        assert_eq!(replaced.get_str("B"), None);
        assert!(replaced.replace(&"B".to_owned(), 3).is_none());
    }

    #[test]
    fn persistent_submap_checks_share_unchanged_routes() {
        let empty = PersistentExactIndex::default();
        let a = empty
            .insert_with("A".to_owned(), 1_u8, |left, right| left == right)
            .expect("A binding");
        let ab = a
            .insert_with("B".to_owned(), 2_u8, |left, right| left == right)
            .expect("B binding");
        let changed = ab
            .replace(&"A".to_owned(), 3_u8)
            .expect("replace A binding");

        assert!(a.is_submap_of(&ab, |left, right| left == right));
        assert!(!ab.is_submap_of(&a, |left, right| left == right));
        assert!(!ab.is_submap_of(&changed, |left, right| left == right));
        assert_eq!(ab.len(), 2);
        assert!(!ab.is_empty());

        let mut entries = Vec::new();
        ab.for_each(|key, value| entries.push((key.clone(), *value)));
        entries.sort();
        assert_eq!(entries, vec![("A".to_owned(), 1), ("B".to_owned(), 2)]);
    }

    #[test]
    fn exact_name_map_retains_shadow_frames_and_merges_siblings() {
        let root = PersistentExactNameMap::default()
            .push("A".to_owned(), 1_u8, |left, right| left == right);
        let shadow = root.push("A".to_owned(), 2_u8, |left, right| left == right);
        let sibling = root.push("B".to_owned(), 3_u8, |left, right| left == right);

        assert_eq!(root.get("A"), Some(&1));
        assert_eq!(shadow.get("A"), Some(&2));
        assert_eq!(
            shadow
                .frame("A")
                .and_then(PersistentExactNameFrame::shadowed)
                .map(PersistentExactNameFrame::value),
            Some(&1)
        );
        assert!(shadow.extends(&root, |left, right| left == right));
        assert!(!root.extends(&shadow, |left, right| left == right));

        let merged = shadow
            .merged(&sibling, |left, right| left == right)
            .expect("compatible sibling names merge");
        assert_eq!(merged.get("A"), Some(&2));
        assert_eq!(merged.get("B"), Some(&3));
        assert!(merged.extends(&shadow, |left, right| left == right));
        assert!(merged.extends(&sibling, |left, right| left == right));

        let conflict = root.push("B".to_owned(), 4_u8, |left, right| left == right);
        assert!(
            sibling
                .merged(&conflict, |left, right| left == right)
                .is_err()
        );
    }

    #[test]
    fn sibling_merge_visits_shared_routes_not_the_whole_ambient_map() {
        const DEPTH: usize = 512;
        let mut root = PersistentExactNameMap::default();
        for index in 0..DEPTH {
            root = root.push(format!("T{index}"), index, |left, right| left == right);
        }
        let left = root.push("Left".to_owned(), DEPTH, |left, right| left == right);
        let right = root.push("Right".to_owned(), DEPTH + 1, |left, right| left == right);

        reset_persistent_exact_work();
        let merged = left
            .merged(&right, |left, right| left == right)
            .expect("siblings add disjoint exact names");
        let (_, _, node_pairs, added_entries) = persistent_exact_work();

        assert_eq!(merged.len(), DEPTH + 2);
        assert_eq!(merged.get("Left"), Some(&DEPTH));
        assert_eq!(merged.get("Right"), Some(&(DEPTH + 1)));
        assert_eq!(added_entries, 1);
        assert!(
            node_pairs <= 64,
            "persistent sibling merge visited {node_pairs} node pairs"
        );
    }

    #[test]
    fn depth_n_children_and_ancestry_visit_only_changed_trie_paths() {
        const DEPTH: usize = 512;
        let mut parent = PersistentExactNameMap::default();
        reset_persistent_exact_work();

        for index in 0..DEPTH {
            let child = parent.push(format!("T{index}"), index, |left, right| left == right);
            assert!(child.extends(&parent, |left, right| left == right));
            parent = child;
        }

        let (changed_path_visits, submap_changed_pairs, _, _) = persistent_exact_work();
        assert!(
            changed_path_visits <= DEPTH * 18,
            "persistent insertion visited {changed_path_visits} changed-path nodes"
        );
        assert!(
            submap_changed_pairs <= DEPTH * 18,
            "persistent ancestry visited {submap_changed_pairs} changed node pairs"
        );
    }
}
