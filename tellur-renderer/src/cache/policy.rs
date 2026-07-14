use std::collections::HashMap;
use std::hash::Hash;

use lru::LruCache;

/// Policy-visible metadata for one resident value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryMeta<C> {
    pub(crate) class: C,
    pub(crate) weight: usize,
}

impl<C> EntryMeta<C> {
    pub(crate) const fn new(class: C, weight: usize) -> Self {
        Self { class, weight }
    }
}

/// An owned proof that a lookup missed.
///
/// Keeping the key in a ticket prevents admission from borrowing the cache
/// across the work needed to create a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MissTicket<K> {
    key: K,
}

impl<K> MissTicket<K> {
    pub(super) fn new(key: K) -> Self {
        Self { key }
    }

    pub(crate) fn key(&self) -> &K {
        &self.key
    }
}

/// A candidate that may be committed after its value has been created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdmissionCandidate<K, C> {
    key: K,
    meta: EntryMeta<C>,
}

impl<K, C> AdmissionCandidate<K, C> {
    pub(crate) fn key(&self) -> &K {
        &self.key
    }

    pub(crate) fn meta(&self) -> EntryMeta<C>
    where
        C: Copy,
    {
        self.meta
    }
}

/// A non-mutating admission decision.
///
/// Dropping a plan has no effect. Its victims and candidate only become
/// resident-state changes when the adapter explicitly removes and commits
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdmissionPlan<K, C> {
    candidate: AdmissionCandidate<K, C>,
    victims: Vec<K>,
}

impl<K, C> AdmissionPlan<K, C> {
    #[cfg(test)]
    pub(crate) fn victims(&self) -> &[K] {
        &self.victims
    }

    #[cfg(test)]
    pub(crate) fn into_candidate(self) -> AdmissionCandidate<K, C> {
        self.candidate
    }

    pub(crate) fn into_parts(self) -> (AdmissionCandidate<K, C>, Vec<K>) {
        (self.candidate, self.victims)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionRejectReason {
    /// The miss became resident before its ticket was planned.
    AlreadyResident,
    /// A value cannot fit even in an otherwise-empty class.
    Overweight { weight: usize, capacity: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionDecision<K, C> {
    Reject(AdmissionRejectReason),
    Admit(AdmissionPlan<K, C>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitRejectReason {
    AlreadyResident,
    Overweight {
        weight: usize,
        capacity: usize,
    },
    /// The plan's same-class victims have not all been removed, or the class
    /// changed after planning.
    NeedsReclaim {
        weight: usize,
        available: usize,
    },
}

/// Pure residency strategy used by the value-owning cache layer.
///
/// Implementations only observe keys, classes, weights, and accesses. They do
/// not know what a cached value is or how its resources are allocated.
pub(crate) trait ResidencyPolicy<K, C> {
    fn contains_key(&self, key: &K) -> bool;
    fn record_hit(&mut self, key: &K);
    fn record_miss(&mut self, key: &K);
    fn plan_admission(&self, ticket: MissTicket<K>, meta: EntryMeta<C>) -> AdmissionDecision<K, C>;
    fn validate_candidate(
        &self,
        candidate: &AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason>;
    fn commit_candidate(
        &mut self,
        candidate: AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason>;
    fn remove(&mut self, key: &K) -> Option<(K, EntryMeta<C>)>;
    fn victim_key(&self, class: C) -> Option<K>;
    fn set_capacity(&mut self, class: C, capacity: usize);
    fn clear(&mut self);
    fn total_weight(&self) -> usize;
    fn class_weight(&self, class: C) -> usize;
    fn class_capacity(&self, class: C) -> usize;

    #[cfg(test)]
    fn total_capacity(&self) -> usize;

    #[cfg(test)]
    fn len(&self) -> usize;
}

/// Immediate admission with byte-weighted, per-class LRU eviction.
///
/// This type owns strategy metadata only. It never sees or stores cache values.
pub(crate) struct ImmediateLruPolicy<K, C> {
    residents: LruCache<K, EntryMeta<C>>,
    capacities: HashMap<C, usize>,
    class_weights: HashMap<C, usize>,
    total_weight: usize,
}

impl<K, C> ImmediateLruPolicy<K, C>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    pub(super) fn new() -> Self {
        Self {
            residents: LruCache::unbounded(),
            capacities: HashMap::new(),
            class_weights: HashMap::new(),
            total_weight: 0,
        }
    }

    fn subtract_weight(&mut self, meta: EntryMeta<C>) {
        self.total_weight = self.total_weight.saturating_sub(meta.weight);
        if let Some(class_weight) = self.class_weights.get_mut(&meta.class) {
            *class_weight = class_weight.saturating_sub(meta.weight);
            if *class_weight == 0 {
                self.class_weights.remove(&meta.class);
            }
        }
    }
}

impl<K, C> ResidencyPolicy<K, C> for ImmediateLruPolicy<K, C>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    fn contains_key(&self, key: &K) -> bool {
        self.residents.contains(key)
    }

    fn record_hit(&mut self, key: &K) {
        let resident = self.residents.get(key);
        debug_assert!(resident.is_some(), "a cache hit must be resident");
    }

    fn record_miss(&mut self, _key: &K) {}

    fn plan_admission(&self, ticket: MissTicket<K>, meta: EntryMeta<C>) -> AdmissionDecision<K, C> {
        if self.contains_key(ticket.key()) {
            return AdmissionDecision::Reject(AdmissionRejectReason::AlreadyResident);
        }

        let capacity = self.class_capacity(meta.class);
        if meta.weight > capacity {
            return AdmissionDecision::Reject(AdmissionRejectReason::Overweight {
                weight: meta.weight,
                capacity,
            });
        }

        let mut projected_weight = self.class_weight(meta.class);
        let mut victims = Vec::new();
        if meta.weight > capacity.saturating_sub(projected_weight) {
            for (key, resident) in self.residents.iter().rev() {
                if resident.class != meta.class {
                    continue;
                }
                victims.push(key.clone());
                projected_weight = projected_weight.saturating_sub(resident.weight);
                if meta.weight <= capacity.saturating_sub(projected_weight) {
                    break;
                }
            }
        }

        debug_assert!(
            meta.weight <= capacity.saturating_sub(projected_weight),
            "resident class weight must equal the sum of resident metadata"
        );
        AdmissionDecision::Admit(AdmissionPlan {
            candidate: AdmissionCandidate {
                key: ticket.key,
                meta,
            },
            victims,
        })
    }

    fn validate_candidate(
        &self,
        candidate: &AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason> {
        if self.contains_key(candidate.key()) {
            return Err(CommitRejectReason::AlreadyResident);
        }

        let meta = candidate.meta();
        let capacity = self.class_capacity(meta.class);
        if meta.weight > capacity {
            return Err(CommitRejectReason::Overweight {
                weight: meta.weight,
                capacity,
            });
        }

        let available = capacity.saturating_sub(self.class_weight(meta.class));
        if meta.weight > available {
            return Err(CommitRejectReason::NeedsReclaim {
                weight: meta.weight,
                available,
            });
        }
        Ok(())
    }

    fn commit_candidate(
        &mut self,
        candidate: AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason> {
        self.validate_candidate(&candidate)?;
        let AdmissionCandidate { key, meta } = candidate;
        let replaced = self.residents.put(key, meta);
        debug_assert!(
            replaced.is_none(),
            "a candidate must not replace a resident"
        );

        let class_weight = self.class_weights.entry(meta.class).or_default();
        *class_weight = class_weight
            .checked_add(meta.weight)
            .expect("cache class weight overflow");
        self.total_weight = self
            .total_weight
            .checked_add(meta.weight)
            .expect("cache total weight overflow");
        Ok(())
    }

    fn remove(&mut self, key: &K) -> Option<(K, EntryMeta<C>)> {
        let meta = self.residents.pop(key)?;
        self.subtract_weight(meta);
        Some((key.clone(), meta))
    }

    fn victim_key(&self, class: C) -> Option<K> {
        self.residents
            .iter()
            .rev()
            .find_map(|(key, meta)| (meta.class == class).then(|| key.clone()))
    }

    fn set_capacity(&mut self, class: C, capacity: usize) {
        if capacity == 0 {
            self.capacities.remove(&class);
        } else {
            self.capacities.insert(class, capacity);
        }
    }

    fn clear(&mut self) {
        self.residents.clear();
        self.class_weights.clear();
        self.total_weight = 0;
    }

    fn total_weight(&self) -> usize {
        self.total_weight
    }

    fn class_weight(&self, class: C) -> usize {
        self.class_weights.get(&class).copied().unwrap_or(0)
    }

    #[cfg(test)]
    fn total_capacity(&self) -> usize {
        self.capacities
            .values()
            .copied()
            .fold(0, usize::saturating_add)
    }

    fn class_capacity(&self, class: C) -> usize {
        self.capacities.get(&class).copied().unwrap_or(0)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.residents.len()
    }
}

impl<K, C> Default for ImmediateLruPolicy<K, C>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}
