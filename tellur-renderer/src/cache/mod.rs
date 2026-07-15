mod policy;
mod store;

use std::hash::Hash;
use std::marker::PhantomData;

#[cfg(test)]
use policy::ImmediateLruPolicy;
use policy::ResidencyPolicy;
use policy::{AdmissionCandidate, AdmissionDecision, AdmissionPlan, FrequencyLruPolicy};
pub(crate) use policy::{AdmissionRejectReason, CommitRejectReason, EntryMeta, MissTicket};
use store::ValueStore;

/// An owned cache lookup result.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Lookup<T, K> {
    Hit(T),
    Miss(MissTicket<K>),
}

/// An admission whose policy-selected victims have already been removed.
///
/// The candidate is intentionally opaque to adapters. They may account and
/// drop the returned evictions, then either commit this value or discard it.
pub(crate) struct PreparedAdmission<K, C> {
    candidate: AdmissionCandidate<K, C>,
}

/// An admission decision whose victims have not been removed yet.
///
/// Dropping this value keeps every resident intact while retaining the
/// candidate's non-resident observation in frequency-aware policies.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PlannedAdmission<K, C> {
    plan: AdmissionPlan<K, C>,
}

/// First phase of admission, before policy-selected victims are removed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AdmissionPlanning<K, C> {
    Rejected(AdmissionRejectReason),
    Planned(PlannedAdmission<K, C>),
}

/// Stable boundary between a residency strategy and a value-owning adapter.
pub(crate) enum AdmissionPreparation<K, C, V> {
    Rejected(AdmissionRejectReason),
    Ready {
        admission: PreparedAdmission<K, C>,
        evicted: Vec<RemovedEntry<K, C, V>>,
    },
}

/// An entry removed from both policy metadata and value storage.
#[derive(Debug)]
pub(crate) struct RemovedEntry<K, C, V> {
    pub(crate) key: K,
    pub(crate) meta: EntryMeta<C>,
    pub(crate) value: V,
}

/// Failure from [`PolicyCache::commit_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommitWithError<E> {
    Policy(CommitRejectReason),
    Create(E),
}

/// A generic value cache backed by a pluggable residency policy.
///
/// Policy residency and values are updated through this type together.
/// Admission planning may update strategy history, but it leaves residents
/// intact so an adapter can discard a plan under external pressure without
/// sacrificing useful values or registering a phantom resident.
pub(crate) struct PolicyCache<K, C, V, P> {
    policy: P,
    store: ValueStore<K, V>,
    _class: PhantomData<fn() -> C>,
}

#[cfg(test)]
pub(crate) type ImmediateCache<K, C, V> = PolicyCache<K, C, V, ImmediateLruPolicy<K, C>>;
pub(crate) type FrequencyCache<K, C, V> = PolicyCache<K, C, V, FrequencyLruPolicy<K, C>>;

#[cfg(test)]
impl<K, C, V> PolicyCache<K, C, V, ImmediateLruPolicy<K, C>>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    pub(crate) fn new() -> Self {
        Self::with_policy(ImmediateLruPolicy::new())
    }
}

impl<K, C, V> PolicyCache<K, C, V, FrequencyLruPolicy<K, C>>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    pub(crate) fn new() -> Self {
        Self::with_policy(FrequencyLruPolicy::new())
    }
}

impl<K, C, V, P> PolicyCache<K, C, V, P>
where
    K: Eq + Hash,
{
    pub(crate) fn with_policy(policy: P) -> Self {
        Self {
            policy,
            store: ValueStore::new(),
            _class: PhantomData,
        }
    }
}

impl<K, C, V, P> PolicyCache<K, C, V, P>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
    P: ResidencyPolicy<K, C>,
{
    /// Looks up `key`, mapping a borrowed resident value into an owned result.
    ///
    /// The closure's result cannot borrow from this cache. A recursive adapter
    /// can therefore finish the lookup and release this borrow before doing the
    /// work represented by an owned [`MissTicket`].
    pub(crate) fn lookup<T>(&mut self, key: K, map: impl FnOnce(&V) -> T) -> Lookup<T, K> {
        if self.policy.contains_key(&key) {
            let hit = self
                .store
                .map(&key, map)
                .expect("cache policy/store residency mismatch");
            self.policy.record_hit(&key);
            Lookup::Hit(hit)
        } else {
            debug_assert!(
                !self.store.contains_key(&key),
                "cache value must have resident policy metadata"
            );
            let frequency = self.policy.record_miss(&key);
            Lookup::Miss(MissTicket::new(key, frequency))
        }
    }

    /// Returns an existing resident without recording a miss when absent.
    ///
    /// Representation-aware adapters use this to reuse the opposite CPU/GPU
    /// representation as a conversion source. An existing value is a real
    /// cache hit and updates its policy history; an absent alternative must not
    /// create ghost history because nobody requested that representation.
    pub(crate) fn get_if_resident<T>(&mut self, key: &K, map: impl FnOnce(&V) -> T) -> Option<T> {
        if !self.policy.contains_key(key) {
            debug_assert!(
                !self.store.contains_key(key),
                "cache value must have resident policy metadata"
            );
            return None;
        }

        let hit = self
            .store
            .map(key, map)
            .expect("cache policy/store residency mismatch");
        self.policy.record_hit(key);
        Some(hit)
    }

    /// Records the candidate's render cost and chooses victims without
    /// removing them. The adapter may drop the returned plan under memory
    /// pressure without sacrificing current residents.
    pub(crate) fn plan_admission(
        &mut self,
        ticket: MissTicket<K>,
        meta: EntryMeta<C>,
    ) -> AdmissionPlanning<K, C> {
        match self.policy.plan_admission(ticket, meta) {
            AdmissionDecision::Reject(reason) => AdmissionPlanning::Rejected(reason),
            AdmissionDecision::Admit(plan) => AdmissionPlanning::Planned(PlannedAdmission { plan }),
        }
    }

    /// Applies policy-selected evictions and returns an opaque candidate that
    /// can be committed after adapter-owned resource checks succeed.
    pub(crate) fn prepare_admission(
        &mut self,
        planned: PlannedAdmission<K, C>,
    ) -> AdmissionPreparation<K, C, V> {
        let (candidate, victims) = planned.plan.into_parts();
        if let Err(reason) = self.policy.validate_plan(&candidate, &victims) {
            return AdmissionPreparation::Rejected(reason);
        }
        let evicted = victims
            .into_iter()
            .map(|victim| {
                self.remove(&victim)
                    .expect("an opaque admission plan must retain its selected victims")
            })
            .collect();
        self.policy.retain_candidate(&candidate);
        AdmissionPreparation::Ready {
            admission: PreparedAdmission { candidate },
            evicted,
        }
    }

    /// Commits an already-created value.
    ///
    /// Callers must remove the plan's victims first. If they do not, validation
    /// fails without changing either policy metadata or the value store.
    pub(crate) fn commit(
        &mut self,
        candidate: AdmissionCandidate<K, C>,
        value: V,
    ) -> Result<(), CommitRejectReason> {
        self.policy.validate_candidate(&candidate)?;
        let key = candidate.key().clone();
        debug_assert!(
            !self.store.contains_key(&key),
            "a non-resident candidate must not have a stored value"
        );

        // HashMap insertion is infallible at the API level. Validate first,
        // then commit policy and value back-to-back to preserve the invariant.
        self.policy.commit_candidate(candidate)?;
        let replaced = self.store.insert(key, value);
        debug_assert!(replaced.is_none(), "a candidate must not replace a value");
        Ok(())
    }

    /// Creates and commits a value only when creation succeeds.
    pub(crate) fn commit_with<E>(
        &mut self,
        admission: PreparedAdmission<K, C>,
        create: impl FnOnce() -> Result<V, E>,
    ) -> Result<(), CommitWithError<E>> {
        let candidate = admission.candidate;
        self.policy
            .validate_candidate(&candidate)
            .map_err(CommitWithError::Policy)?;
        let value = create().map_err(CommitWithError::Create)?;
        self.commit(candidate, value)
            .map_err(CommitWithError::Policy)
    }

    /// Removes a key from both the policy and the value store.
    pub(crate) fn remove(&mut self, key: &K) -> Option<RemovedEntry<K, C, V>> {
        if !self.policy.contains_key(key) {
            debug_assert!(
                !self.store.contains_key(key),
                "cache value must have resident policy metadata"
            );
            return None;
        }

        let value = self
            .store
            .remove(key)
            .expect("cache policy/store residency mismatch");
        let (key, meta) = self
            .policy
            .remove(key)
            .expect("resident policy entry disappeared during removal");
        Some(RemovedEntry { key, meta, value })
    }

    /// Evicts the policy-selected victim in `class`.
    pub(crate) fn evict_one(&mut self, class: C) -> Option<RemovedEntry<K, C, V>> {
        let key = self.policy.victim_key(class)?;
        self.remove(&key)
    }

    /// Evicts same-class policy-selected entries until `incoming` bytes would fit.
    ///
    /// An overweight incoming value cannot fit even in an empty class, so this
    /// method leaves current residents intact in that case.
    pub(crate) fn reclaim_to_fit(
        &mut self,
        class: C,
        incoming: usize,
    ) -> Vec<RemovedEntry<K, C, V>> {
        let capacity = self.class_capacity(class);
        if incoming > capacity {
            return Vec::new();
        }

        let mut removed = Vec::new();
        while self.class_weight(class) > capacity.saturating_sub(incoming) {
            let Some(entry) = self.evict_one(class) else {
                break;
            };
            removed.push(entry);
        }
        removed
    }

    /// Changes one class's byte capacity and immediately restores the limit.
    pub(crate) fn set_capacity(&mut self, class: C, capacity: usize) -> Vec<RemovedEntry<K, C, V>> {
        self.policy.set_capacity(class, capacity);
        self.reclaim_to_fit(class, 0)
    }

    /// Drops all residents while preserving configured class capacities.
    pub(crate) fn clear(&mut self) {
        self.store.clear();
        self.policy.clear();
    }

    pub(crate) fn total_weight(&self) -> usize {
        self.policy.total_weight()
    }

    pub(crate) fn class_weight(&self, class: C) -> usize {
        self.policy.class_weight(class)
    }

    #[cfg(test)]
    fn total_capacity(&self) -> usize {
        self.policy.total_capacity()
    }

    pub(crate) fn class_capacity(&self, class: C) -> usize {
        self.policy.class_capacity(class)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        debug_assert_eq!(self.policy.len(), self.store.len());
        self.policy.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
impl<K, C, V> Default for PolicyCache<K, C, V, ImmediateLruPolicy<K, C>>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, C, V> Default for PolicyCache<K, C, V, FrequencyLruPolicy<K, C>>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Class {
        Cpu,
        Gpu,
    }

    fn miss(
        cache: &mut ImmediateCache<&'static str, Class, &'static str>,
        key: &'static str,
    ) -> MissTicket<&'static str> {
        match cache.lookup(key, |value| *value) {
            Lookup::Hit(_) => panic!("expected {key:?} to miss"),
            Lookup::Miss(ticket) => ticket,
        }
    }

    fn plan(
        cache: &mut ImmediateCache<&'static str, Class, &'static str>,
        ticket: MissTicket<&'static str>,
        class: Class,
        weight: usize,
    ) -> AdmissionPlan<&'static str, Class> {
        match cache.plan_admission(ticket, EntryMeta::new(class, weight)) {
            AdmissionPlanning::Planned(planned) => planned.plan,
            AdmissionPlanning::Rejected(reason) => panic!("admission rejected: {reason:?}"),
        }
    }

    fn admission(
        cache: &mut ImmediateCache<&'static str, Class, &'static str>,
        key: &'static str,
        class: Class,
        weight: usize,
    ) -> AdmissionPlan<&'static str, Class> {
        let ticket = miss(cache, key);
        plan(cache, ticket, class, weight)
    }

    fn apply(
        cache: &mut ImmediateCache<&'static str, Class, &'static str>,
        plan: AdmissionPlan<&'static str, Class>,
        value: &'static str,
    ) -> Vec<&'static str> {
        let (candidate, victims) = plan.into_parts();
        for victim in &victims {
            cache.remove(victim).expect("planned victim must exist");
        }
        cache.commit(candidate, value).unwrap();
        victims
    }

    type TestFrequencyCache = FrequencyCache<&'static str, Class, &'static str>;

    fn frequency_miss(
        cache: &mut TestFrequencyCache,
        key: &'static str,
    ) -> MissTicket<&'static str> {
        match cache.lookup(key, |value| *value) {
            Lookup::Hit(_) => panic!("expected {key:?} to miss"),
            Lookup::Miss(ticket) => ticket,
        }
    }

    fn frequency_plan(
        cache: &mut TestFrequencyCache,
        key: &'static str,
        class: Class,
        weight: usize,
        cost: u64,
    ) -> AdmissionPlanning<&'static str, Class> {
        let ticket = frequency_miss(cache, key);
        cache.plan_admission(ticket, EntryMeta::with_cost(class, weight, cost))
    }

    fn commit_frequency_plan(
        cache: &mut TestFrequencyCache,
        planned: PlannedAdmission<&'static str, Class>,
        value: &'static str,
    ) -> Vec<&'static str> {
        let (admission, evicted) = match cache.prepare_admission(planned) {
            AdmissionPreparation::Ready { admission, evicted } => (admission, evicted),
            AdmissionPreparation::Rejected(reason) => {
                panic!("planned admission became invalid: {reason:?}")
            }
        };
        let victims = evicted.iter().map(|entry| entry.key).collect();
        cache.commit_with(admission, || Ok::<_, ()>(value)).unwrap();
        victims
    }

    fn admit_frequency(
        cache: &mut TestFrequencyCache,
        key: &'static str,
        class: Class,
        weight: usize,
        cost: u64,
    ) {
        assert_eq!(
            frequency_plan(cache, key, class, weight, cost),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency {
                frequency: 1,
                required: 2,
            })
        );
        let AdmissionPlanning::Planned(planned) = frequency_plan(cache, key, class, weight, cost)
        else {
            panic!("second observation must be admitted into free capacity");
        };
        assert!(commit_frequency_plan(cache, planned, key).is_empty());
    }

    #[test]
    fn immediate_first_insertion_then_hit() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 10);

        let ticket = miss(&mut cache, "a");
        let plan = plan(&mut cache, ticket, Class::Cpu, 4);
        assert!(plan.victims().is_empty());
        apply(&mut cache, plan, "value-a");

        assert_eq!(
            cache.lookup("a", |value| value.to_string()),
            Lookup::Hit("value-a".to_owned())
        );
        assert_eq!(cache.class_weight(Class::Cpu), 4);
        assert_eq!(cache.total_weight(), 4);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn stale_miss_ticket_is_rejected_after_nested_admission() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 10);

        let stale = miss(&mut cache, "a");
        let nested = admission(&mut cache, "a", Class::Cpu, 4);
        apply(&mut cache, nested, "nested-value");

        assert_eq!(
            cache.plan_admission(stale, EntryMeta::new(Class::Cpu, 4)),
            AdmissionPlanning::Rejected(AdmissionRejectReason::AlreadyResident)
        );
        assert_eq!(
            cache.lookup("a", |value| *value),
            Lookup::Hit("nested-value")
        );
        assert_eq!(cache.class_weight(Class::Cpu), 4);
    }

    #[test]
    fn admission_evicts_same_class_by_byte_weight() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 10);
        let first = admission(&mut cache, "a", Class::Cpu, 6);
        apply(&mut cache, first, "value-a");

        let second = admission(&mut cache, "b", Class::Cpu, 6);
        assert_eq!(second.victims(), &["a"]);
        assert_eq!(apply(&mut cache, second, "value-b"), vec!["a"]);

        assert!(matches!(cache.lookup("a", |value| *value), Lookup::Miss(_)));
        assert_eq!(cache.lookup("b", |value| *value), Lookup::Hit("value-b"));
        assert_eq!(cache.class_weight(Class::Cpu), 6);
    }

    #[test]
    fn admission_never_evicts_across_classes() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 5);
        cache.set_capacity(Class::Gpu, 5);

        let gpu = admission(&mut cache, "gpu", Class::Gpu, 5);
        apply(&mut cache, gpu, "gpu-value");
        let cpu_a = admission(&mut cache, "cpu-a", Class::Cpu, 5);
        apply(&mut cache, cpu_a, "cpu-a-value");

        // Make the GPU entry globally older than cpu-a. A CPU admission must
        // still select only a CPU victim.
        let cpu_b = admission(&mut cache, "cpu-b", Class::Cpu, 5);
        assert_eq!(cpu_b.victims(), &["cpu-a"]);
        apply(&mut cache, cpu_b, "cpu-b-value");

        assert_eq!(
            cache.lookup("gpu", |value| *value),
            Lookup::Hit("gpu-value")
        );
        assert_eq!(cache.class_weight(Class::Gpu), 5);
        assert_eq!(cache.class_weight(Class::Cpu), 5);
        assert_eq!(cache.total_weight(), 10);
    }

    #[test]
    fn overweight_candidate_is_rejected_without_eviction() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 5);
        let resident = admission(&mut cache, "resident", Class::Cpu, 5);
        apply(&mut cache, resident, "resident-value");

        let ticket = miss(&mut cache, "oversize");
        assert_eq!(
            cache.plan_admission(ticket, EntryMeta::new(Class::Cpu, 6)),
            AdmissionPlanning::Rejected(AdmissionRejectReason::Overweight {
                weight: 6,
                capacity: 5,
            })
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.class_weight(Class::Cpu), 5);
        assert_eq!(
            cache.lookup("resident", |value| *value),
            Lookup::Hit("resident-value")
        );
    }

    #[test]
    fn set_capacity_and_reclaim_use_class_lru() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 12);
        for key in ["a", "b", "c"] {
            let admission = admission(&mut cache, key, Class::Cpu, 4);
            apply(&mut cache, admission, key);
        }

        // a becomes most recent, making b the next victim.
        assert!(matches!(
            cache.lookup("a", |value| *value),
            Lookup::Hit("a")
        ));
        let removed = cache.set_capacity(Class::Cpu, 8);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].key, "b");
        assert_eq!(removed[0].meta, EntryMeta::new(Class::Cpu, 4));
        assert_eq!(removed[0].value, "b");
        assert_eq!(cache.class_capacity(Class::Cpu), 8);

        let removed = cache.reclaim_to_fit(Class::Cpu, 5);
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].key, "c");
        assert_eq!(removed[1].key, "a");
        assert_eq!(cache.class_weight(Class::Cpu), 0);
        assert_eq!(cache.total_capacity(), 8);
        assert!(cache.is_empty());
    }

    #[test]
    fn cancelled_or_failed_plan_does_not_register_candidate() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 10);

        let cancelled = admission(&mut cache, "cancelled", Class::Cpu, 4);
        drop(cancelled);
        assert!(cache.is_empty());
        assert_eq!(cache.total_weight(), 0);

        let failed = admission(&mut cache, "failed", Class::Cpu, 4);
        let result = cache.commit_with(
            PreparedAdmission {
                candidate: failed.into_candidate(),
            },
            || Err::<&str, _>("no memory"),
        );
        assert_eq!(result, Err(CommitWithError::Create("no memory")));
        assert!(cache.is_empty());
        assert_eq!(cache.total_weight(), 0);
        assert!(matches!(
            cache.lookup("failed", |value| *value),
            Lookup::Miss(_)
        ));
    }

    #[test]
    fn commit_requires_planned_victims_to_be_removed() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 5);
        let first = admission(&mut cache, "a", Class::Cpu, 5);
        apply(&mut cache, first, "value-a");

        let second = admission(&mut cache, "b", Class::Cpu, 5);
        let candidate = second.into_candidate();
        assert_eq!(
            cache.commit(candidate, "value-b"),
            Err(CommitRejectReason::NeedsReclaim {
                weight: 5,
                available: 0,
            })
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.lookup("a", |value| *value), Lookup::Hit("value-a"));
    }

    #[test]
    fn clear_preserves_capacity_and_resets_residency() {
        let mut cache = ImmediateCache::new();
        cache.set_capacity(Class::Cpu, 8);
        let admission = admission(&mut cache, "a", Class::Cpu, 4);
        apply(&mut cache, admission, "value-a");

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.total_weight(), 0);
        assert_eq!(cache.class_capacity(Class::Cpu), 8);
    }

    #[test]
    fn frequency_cache_admits_on_second_miss_and_hits_on_third_lookup() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 10);

        assert_eq!(
            frequency_plan(&mut cache, "a", Class::Cpu, 4, 20),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency {
                frequency: 1,
                required: 2,
            })
        );
        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "a", Class::Cpu, 4, 20)
        else {
            panic!("second miss should produce an admission plan");
        };
        commit_frequency_plan(&mut cache, planned, "value-a");

        assert_eq!(cache.lookup("a", |value| *value), Lookup::Hit("value-a"));
        assert_eq!(cache.class_weight(Class::Cpu), 4);
    }

    #[test]
    fn frequency_cache_chooses_lowest_density_then_lru() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 10);
        admit_frequency(&mut cache, "dense", Class::Cpu, 4, 40);
        admit_frequency(&mut cache, "sparse-old", Class::Cpu, 3, 3);
        admit_frequency(&mut cache, "sparse-new", Class::Cpu, 3, 3);

        assert!(matches!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 3, 100),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));
        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "candidate", Class::Cpu, 3, 100)
        else {
            panic!("high-benefit candidate should replace a sparse resident");
        };
        assert_eq!(planned.plan.victims(), &["sparse-old"]);
        assert_eq!(
            commit_frequency_plan(&mut cache, planned, "candidate-value"),
            vec!["sparse-old"]
        );
        assert!(matches!(
            cache.lookup("sparse-old", |value| *value),
            Lookup::Miss(_)
        ));
        assert_eq!(cache.lookup("dense", |value| *value), Lookup::Hit("dense"));
    }

    #[test]
    fn external_frequency_eviction_uses_density_then_lru() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 6);
        admit_frequency(&mut cache, "dense", Class::Cpu, 2, 10);
        admit_frequency(&mut cache, "sparse-old", Class::Cpu, 2, 1);
        admit_frequency(&mut cache, "sparse-new", Class::Cpu, 2, 1);

        assert_eq!(cache.evict_one(Class::Cpu).unwrap().key, "sparse-old");
        assert_eq!(cache.evict_one(Class::Cpu).unwrap().key, "sparse-new");
        assert_eq!(cache.evict_one(Class::Cpu).unwrap().key, "dense");
    }

    #[test]
    fn frequency_cache_rejects_candidate_below_aggregate_victim_benefit() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 10);
        admit_frequency(&mut cache, "a", Class::Cpu, 5, 50);
        admit_frequency(&mut cache, "b", Class::Cpu, 5, 50);

        assert_eq!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 8, 75),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency {
                frequency: 1,
                required: 2,
            })
        );
        assert_eq!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 8, 75),
            AdmissionPlanning::Rejected(AdmissionRejectReason::NotWorthReplacing)
        );
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.lookup("a", |value| *value), Lookup::Hit("a"));
        assert_eq!(cache.lookup("b", |value| *value), Lookup::Hit("b"));
    }

    #[test]
    fn frequency_cache_uses_smaller_weight_to_break_equal_benefit() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 10);
        admit_frequency(&mut cache, "a", Class::Cpu, 5, 25);
        admit_frequency(&mut cache, "b", Class::Cpu, 5, 25);

        assert!(matches!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 8, 50),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));
        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "candidate", Class::Cpu, 8, 50)
        else {
            panic!("equal-benefit smaller candidate should be admitted");
        };
        assert_eq!(planned.plan.victims(), &["a", "b"]);
        assert_eq!(
            commit_frequency_plan(&mut cache, planned, "candidate"),
            vec!["a", "b"]
        );
    }

    #[test]
    fn frequency_cache_rejects_equal_benefit_equal_weight_churn() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 5);
        admit_frequency(&mut cache, "resident", Class::Cpu, 5, 10);

        assert!(matches!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 5, 10),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));
        assert_eq!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 5, 10),
            AdmissionPlanning::Rejected(AdmissionRejectReason::NotWorthReplacing)
        );
        assert_eq!(
            cache.lookup("resident", |value| *value),
            Lookup::Hit("resident")
        );
    }

    #[test]
    fn frequency_cache_never_replaces_across_classes() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 5);
        cache.set_capacity(Class::Gpu, 5);
        admit_frequency(&mut cache, "cpu", Class::Cpu, 5, 1);
        admit_frequency(&mut cache, "gpu", Class::Gpu, 5, 1);

        assert!(matches!(
            frequency_plan(&mut cache, "cpu-new", Class::Cpu, 5, 100),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));
        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "cpu-new", Class::Cpu, 5, 100)
        else {
            panic!("CPU candidate should replace the CPU resident");
        };
        assert_eq!(planned.plan.victims(), &["cpu"]);
        commit_frequency_plan(&mut cache, planned, "cpu-new");
        assert_eq!(cache.lookup("gpu", |value| *value), Lookup::Hit("gpu"));
    }

    #[test]
    fn evicted_frequency_history_allows_immediate_readmission() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 5);
        admit_frequency(&mut cache, "a", Class::Cpu, 5, 10);

        let removed = cache.evict_one(Class::Cpu).unwrap();
        assert_eq!(removed.key, "a");
        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "a", Class::Cpu, 5, 10)
        else {
            panic!("evicted resident history should survive as a ghost");
        };
        commit_frequency_plan(&mut cache, planned, "a-again");
        assert_eq!(cache.lookup("a", |value| *value), Lookup::Hit("a-again"));
    }

    #[test]
    fn capacity_reclaim_prefers_low_value_density_over_global_lru() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 10);
        admit_frequency(&mut cache, "valuable-old", Class::Cpu, 5, 100);
        admit_frequency(&mut cache, "cheap-new", Class::Cpu, 5, 1);

        let removed = cache.set_capacity(Class::Cpu, 5);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].key, "cheap-new");
        assert_eq!(
            cache.lookup("valuable-old", |value| *value),
            Lookup::Hit("valuable-old")
        );
    }

    #[test]
    fn dropped_plan_and_create_failure_keep_candidate_history_without_phantoms() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 5);
        assert!(matches!(
            frequency_plan(&mut cache, "a", Class::Cpu, 5, 10),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));

        let AdmissionPlanning::Planned(dropped) =
            frequency_plan(&mut cache, "a", Class::Cpu, 5, 10)
        else {
            panic!("second miss should plan admission");
        };
        drop(dropped);
        assert!(cache.is_empty());

        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "a", Class::Cpu, 5, 10)
        else {
            panic!("dropped plan must retain candidate history");
        };
        let (admission, evicted) = match cache.prepare_admission(planned) {
            AdmissionPreparation::Ready { admission, evicted } => (admission, evicted),
            AdmissionPreparation::Rejected(reason) => panic!("unexpected rejection: {reason:?}"),
        };
        assert!(evicted.is_empty());
        assert_eq!(
            cache.commit_with(admission, || Err::<&str, _>("allocation failed")),
            Err(CommitWithError::Create("allocation failed"))
        );
        assert!(cache.is_empty());

        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "a", Class::Cpu, 5, 10)
        else {
            panic!("failed creation must retain candidate history");
        };
        commit_frequency_plan(&mut cache, planned, "value-a");
    }

    #[test]
    fn dropping_replacement_plan_keeps_existing_residents() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 5);
        admit_frequency(&mut cache, "resident", Class::Cpu, 5, 1);

        assert!(matches!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 5, 100),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));
        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "candidate", Class::Cpu, 5, 100)
        else {
            panic!("valuable candidate should produce a replacement plan");
        };
        assert_eq!(planned.plan.victims(), &["resident"]);

        // This is the renderer's memory-pressure path: planning observes the
        // candidate, but dropping the opaque plan must not apply its victims.
        drop(planned);
        assert_eq!(cache.class_weight(Class::Cpu), 5);
        assert_eq!(cache.len(), 1);
        assert_eq!(
            cache.lookup("resident", |value| *value),
            Lookup::Hit("resident")
        );
    }

    #[test]
    fn victim_ghost_churn_cannot_discard_a_failed_candidate_observation() {
        let policy = FrequencyLruPolicy::with_test_ghost_capacity(2);
        let mut cache = PolicyCache::with_policy(policy);
        cache.set_capacity(Class::Cpu, 3);
        for key in ["a", "b", "c"] {
            admit_frequency(&mut cache, key, Class::Cpu, 1, 1);
        }

        assert!(matches!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 3, 10),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));
        let AdmissionPlanning::Planned(planned) =
            frequency_plan(&mut cache, "candidate", Class::Cpu, 3, 10)
        else {
            panic!("second candidate miss should plan admission");
        };
        assert_eq!(planned.plan.victims(), &["a", "b", "c"]);
        let (admission, evicted) = match cache.prepare_admission(planned) {
            AdmissionPreparation::Ready { admission, evicted } => (admission, evicted),
            AdmissionPreparation::Rejected(reason) => panic!("unexpected rejection: {reason:?}"),
        };
        assert_eq!(evicted.len(), 3);
        assert_eq!(
            cache.commit_with(admission, || Err::<&str, _>("allocation failed")),
            Err(CommitWithError::Create("allocation failed"))
        );
        assert_eq!(cache.policy.observation(&"candidate"), Some((2, 10)));

        assert!(matches!(
            frequency_plan(&mut cache, "candidate", Class::Cpu, 3, 10),
            AdmissionPlanning::Planned(_)
        ));
    }

    #[test]
    fn frequency_clear_removes_history_and_preserves_capacity() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 8);
        admit_frequency(&mut cache, "resident", Class::Cpu, 4, 10);
        assert!(matches!(
            frequency_plan(&mut cache, "ghost", Class::Cpu, 4, 10),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. })
        ));

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.class_capacity(Class::Cpu), 8);
        assert_eq!(cache.total_weight(), 0);
        assert_eq!(
            frequency_plan(&mut cache, "resident", Class::Cpu, 4, 10),
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency {
                frequency: 1,
                required: 2,
            })
        );

        let AdmissionPlanning::Planned(stale) =
            frequency_plan(&mut cache, "resident", Class::Cpu, 4, 10)
        else {
            panic!("second post-clear miss should produce a plan");
        };
        cache.clear();
        assert!(matches!(
            cache.prepare_admission(stale),
            AdmissionPreparation::Rejected(AdmissionRejectReason::StalePlan)
        ));
        assert_eq!(cache.policy.observation(&"resident"), None);
        assert_eq!(cache.class_capacity(Class::Cpu), 8);
    }

    #[test]
    fn frequency_overweight_rejection_precedes_threshold_and_keeps_history() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 5);
        for _ in 0..2 {
            assert_eq!(
                frequency_plan(&mut cache, "large", Class::Cpu, 6, 10),
                AdmissionPlanning::Rejected(AdmissionRejectReason::Overweight {
                    weight: 6,
                    capacity: 5,
                })
            );
        }

        cache.set_capacity(Class::Cpu, 6);
        assert!(matches!(
            frequency_plan(&mut cache, "large", Class::Cpu, 6, 10),
            AdmissionPlanning::Planned(_)
        ));
    }

    #[test]
    fn nested_stale_frequency_ticket_updates_cost_without_duplicate_residency() {
        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 5);
        let stale = frequency_miss(&mut cache, "a");
        let AdmissionPlanning::Planned(nested) = frequency_plan(&mut cache, "a", Class::Cpu, 5, 10)
        else {
            panic!("nested second miss should plan admission");
        };
        commit_frequency_plan(&mut cache, nested, "nested");

        assert_eq!(
            cache.plan_admission(stale, EntryMeta::with_cost(Class::Cpu, 5, 30)),
            AdmissionPlanning::Rejected(AdmissionRejectReason::AlreadyResident)
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.policy.observation(&"a"), Some((2, 20)));
        assert_eq!(cache.lookup("a", |value| *value), Lookup::Hit("nested"));
        assert_eq!(cache.policy.observation(&"a"), Some((3, 20)));
    }

    #[test]
    fn frequency_entry_meta_clamps_zero_cost_and_weight() {
        let meta = EntryMeta::with_cost(Class::Cpu, 0, 0);
        assert_eq!(meta.weight, 1);
        assert_eq!(meta.observed_cost, 1);

        let mut cache = FrequencyCache::new();
        cache.set_capacity(Class::Cpu, 1);
        admit_frequency(&mut cache, "zero", Class::Cpu, 0, 0);
        assert_eq!(cache.class_weight(Class::Cpu), 1);
    }
}
