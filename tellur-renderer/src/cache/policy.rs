use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroUsize;

use lru::LruCache;

/// Policy-visible metadata for one resident value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryMeta<C> {
    pub(crate) class: C,
    pub(crate) weight: usize,
    pub(crate) observed_cost: u64,
}

impl<C> EntryMeta<C> {
    #[cfg(test)]
    pub(crate) const fn new(class: C, weight: usize) -> Self {
        Self::with_cost(class, weight, 1)
    }

    pub(crate) const fn with_cost(class: C, weight: usize, observed_cost: u64) -> Self {
        Self {
            class,
            weight: if weight == 0 { 1 } else { weight },
            observed_cost: if observed_cost == 0 { 1 } else { observed_cost },
        }
    }
}

/// An owned proof that a lookup missed.
///
/// Keeping the key in a ticket prevents admission from borrowing the cache
/// across the work needed to create a value.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MissTicket<K> {
    key: K,
    frequency: u64,
}

impl<K> MissTicket<K> {
    pub(super) fn new(key: K, frequency: u64) -> Self {
        Self { key, frequency }
    }

    pub(crate) fn key(&self) -> &K {
        &self.key
    }
}

/// Frequency and smoothed render cost attached to a key's lookup history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observation {
    frequency: u64,
    smoothed_cost: Option<u64>,
}

impl Observation {
    const fn first_lookup() -> Self {
        Self {
            frequency: 1,
            smoothed_cost: None,
        }
    }

    fn record_lookup(&mut self) {
        self.frequency = self.frequency.saturating_add(1);
    }

    fn record_cost(&mut self, observed_cost: u64) {
        let observed_cost = observed_cost.max(1);
        // A simple 1/2 EWMA responds to changed render cost without retaining
        // another unbounded sample counter.
        self.smoothed_cost = Some(match self.smoothed_cost {
            None => observed_cost,
            Some(old) => ((u128::from(old) + u128::from(observed_cost)) / 2) as u64,
        });
    }

    fn cost(self) -> u64 {
        self.smoothed_cost.unwrap_or(1)
    }

    fn benefit(self) -> u128 {
        u128::from(self.frequency) * u128::from(self.cost())
    }
}

/// A candidate that may be committed after its value has been created.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AdmissionCandidate<K, C> {
    key: K,
    meta: EntryMeta<C>,
    observation: Option<Observation>,
    policy_generation: u64,
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

/// An admission decision that leaves the resident set unchanged.
///
/// Planning may update bounded non-resident observations, but dropping a plan
/// never removes residents. Its victims and candidate only become resident-set
/// changes when the adapter explicitly prepares and commits it.
#[derive(Debug, PartialEq, Eq)]
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
    /// Frequency-aware admission requires a key to be seen at least twice.
    BelowFrequency { frequency: u64, required: u64 },
    /// The candidate would displace more accumulated benefit than it brings.
    NotWorthReplacing,
    /// Cache state changed after this opaque plan was produced.
    StalePlan,
}

#[derive(Debug, PartialEq, Eq)]
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
    /// The policy was cleared after this candidate was planned.
    StaleCandidate,
}

/// Pure residency strategy used by the value-owning cache layer.
///
/// Implementations only observe keys, classes, weights, and accesses. They do
/// not know what a cached value is or how its resources are allocated.
pub(crate) trait ResidencyPolicy<K, C> {
    fn contains_key(&self, key: &K) -> bool;
    fn record_hit(&mut self, key: &K);
    fn record_miss(&mut self, key: &K) -> u64;
    fn plan_admission(
        &mut self,
        ticket: MissTicket<K>,
        meta: EntryMeta<C>,
    ) -> AdmissionDecision<K, C>;
    fn validate_plan(
        &self,
        candidate: &AdmissionCandidate<K, C>,
        victims: &[K],
    ) -> Result<(), AdmissionRejectReason>;
    fn validate_candidate(
        &self,
        candidate: &AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason>;
    /// Ensures a prepared-but-uncommitted candidate remains in bounded
    /// non-resident history after inserting all of its victims.
    fn retain_candidate(&mut self, candidate: &AdmissionCandidate<K, C>);
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
#[cfg(test)]
pub(crate) struct ImmediateLruPolicy<K, C> {
    residents: LruCache<K, EntryMeta<C>>,
    capacities: HashMap<C, usize>,
    class_weights: HashMap<C, usize>,
    total_weight: usize,
}

#[cfg(test)]
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

#[cfg(test)]
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

    fn record_miss(&mut self, _key: &K) -> u64 {
        1
    }

    fn plan_admission(
        &mut self,
        ticket: MissTicket<K>,
        meta: EntryMeta<C>,
    ) -> AdmissionDecision<K, C> {
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
                observation: None,
                policy_generation: 0,
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

    fn validate_plan(
        &self,
        candidate: &AdmissionCandidate<K, C>,
        victims: &[K],
    ) -> Result<(), AdmissionRejectReason> {
        if self.contains_key(candidate.key()) {
            return Err(AdmissionRejectReason::AlreadyResident);
        }
        let meta = candidate.meta();
        let capacity = self.class_capacity(meta.class);
        if meta.weight > capacity {
            return Err(AdmissionRejectReason::Overweight {
                weight: meta.weight,
                capacity,
            });
        }
        let mut projected_weight = self.class_weight(meta.class);
        for victim in victims {
            let Some(resident) = self.residents.peek(victim) else {
                return Err(AdmissionRejectReason::StalePlan);
            };
            if resident.class != meta.class {
                return Err(AdmissionRejectReason::StalePlan);
            }
            projected_weight = projected_weight.saturating_sub(resident.weight);
        }
        if meta.weight > capacity.saturating_sub(projected_weight) {
            return Err(AdmissionRejectReason::StalePlan);
        }
        Ok(())
    }

    fn retain_candidate(&mut self, _candidate: &AdmissionCandidate<K, C>) {}

    fn commit_candidate(
        &mut self,
        candidate: AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason> {
        self.validate_candidate(&candidate)?;
        let AdmissionCandidate { key, meta, .. } = candidate;
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

#[cfg(test)]
impl<K, C> Default for ImmediateLruPolicy<K, C>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Bounds metadata for non-resident keys independently of image capacity.
const GHOST_CAPACITY: usize = 4_096;
/// Ten ghost-cache turns keep old popularity from becoming permanent.
const AGING_INTERVAL: usize = GHOST_CAPACITY * 10;
/// The second recent observation may cache its freshly-rendered value.
const ADMISSION_FREQUENCY: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrequencyResident<C> {
    meta: EntryMeta<C>,
    observation: Observation,
}

fn compare_density(
    left: Observation,
    left_weight: usize,
    right: Observation,
    right_weight: usize,
) -> Ordering {
    let left_benefit = left.benefit();
    let right_benefit = right.benefit();
    let left_weight = left_weight.max(1) as u128;
    let right_weight = right_weight.max(1) as u128;

    let quotient = (left_benefit / left_weight).cmp(&(right_benefit / right_weight));
    if quotient != Ordering::Equal {
        return quotient;
    }

    // Once the integral quotients match, both remainders are smaller than a
    // usize-sized denominator. Their cross products therefore fit in u128.
    let left_remainder = left_benefit % left_weight;
    let right_remainder = right_benefit % right_weight;
    (left_remainder * right_weight).cmp(&(right_remainder * left_weight))
}

/// Second-use admission with cost-aware frequency-density replacement.
///
/// Resident values are bounded by effective per-class byte capacities (a
/// zero-byte image still costs one policy byte). Non-resident observations are
/// retained in a small LRU so a one-off key costs metadata rather than value
/// memory.
pub(crate) struct FrequencyLruPolicy<K, C> {
    residents: LruCache<K, FrequencyResident<C>>,
    ghosts: LruCache<K, Observation>,
    capacities: HashMap<C, usize>,
    class_weights: HashMap<C, usize>,
    total_weight: usize,
    lookups_until_aging: usize,
    generation: u64,
}

impl<K, C> FrequencyLruPolicy<K, C>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    pub(super) fn new() -> Self {
        Self::with_ghost_capacity(GHOST_CAPACITY)
    }

    fn with_ghost_capacity(ghost_capacity: usize) -> Self {
        Self {
            residents: LruCache::unbounded(),
            ghosts: LruCache::new(NonZeroUsize::new(ghost_capacity).unwrap()),
            capacities: HashMap::new(),
            class_weights: HashMap::new(),
            total_weight: 0,
            lookups_until_aging: AGING_INTERVAL,
            generation: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn with_test_ghost_capacity(ghost_capacity: usize) -> Self {
        Self::with_ghost_capacity(ghost_capacity)
    }

    fn before_lookup(&mut self) {
        if self.lookups_until_aging == 1 {
            self.age_frequencies();
            self.lookups_until_aging = AGING_INTERVAL;
        } else {
            self.lookups_until_aging -= 1;
        }
    }

    fn age_frequencies(&mut self) {
        for (_, resident) in self.residents.iter_mut() {
            resident.observation.frequency /= 2;
        }

        let mut expired = Vec::new();
        for (key, observation) in self.ghosts.iter_mut() {
            observation.frequency /= 2;
            if observation.frequency == 0 {
                expired.push(key.clone());
            }
        }
        for key in expired {
            self.ghosts.pop(&key);
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

    fn candidate(
        &self,
        key: K,
        meta: EntryMeta<C>,
        observation: Observation,
        victims: Vec<K>,
    ) -> AdmissionDecision<K, C> {
        AdmissionDecision::Admit(AdmissionPlan {
            candidate: AdmissionCandidate {
                key,
                meta,
                observation: Some(observation),
                policy_generation: self.generation,
            },
            victims,
        })
    }

    fn replacement_victims(
        &self,
        class: C,
        required_weight: usize,
    ) -> Vec<(K, FrequencyResident<C>)> {
        let mut candidates = self
            .residents
            .iter()
            .rev()
            .filter(|(_, resident)| resident.meta.class == class && resident.meta.weight > 0)
            .map(|(key, resident)| (key.clone(), *resident))
            .collect::<Vec<_>>();

        // Stable sorting preserves the LRU-to-MRU iteration order for equal
        // densities, making recency the deterministic tie breaker.
        candidates.sort_by(|(_, left), (_, right)| {
            compare_density(
                left.observation,
                left.meta.weight,
                right.observation,
                right.meta.weight,
            )
        });

        let mut selected = Vec::new();
        let mut reclaimed = 0usize;
        for candidate in candidates {
            reclaimed = reclaimed.saturating_add(candidate.1.meta.weight);
            selected.push(candidate);
            if reclaimed >= required_weight {
                break;
            }
        }
        selected
    }

    #[cfg(test)]
    pub(super) fn observation(&self, key: &K) -> Option<(u64, u64)> {
        self.residents
            .peek(key)
            .map(|resident| resident.observation)
            .or_else(|| self.ghosts.peek(key).copied())
            .map(|observation| (observation.frequency, observation.cost()))
    }

    #[cfg(test)]
    pub(super) fn ghost_len(&self) -> usize {
        self.ghosts.len()
    }
}

impl<K, C> ResidencyPolicy<K, C> for FrequencyLruPolicy<K, C>
where
    K: Clone + Eq + Hash,
    C: Copy + Eq + Hash,
{
    fn contains_key(&self, key: &K) -> bool {
        self.residents.contains(key)
    }

    fn record_hit(&mut self, key: &K) {
        self.before_lookup();
        let resident = self.residents.get_mut(key);
        debug_assert!(resident.is_some(), "a cache hit must be resident");
        if let Some(resident) = resident {
            resident.observation.record_lookup();
        }
    }

    fn record_miss(&mut self, key: &K) -> u64 {
        self.before_lookup();
        if let Some(observation) = self.ghosts.get_mut(key) {
            observation.record_lookup();
            observation.frequency
        } else {
            let observation = Observation::first_lookup();
            self.ghosts.put(key.clone(), observation);
            observation.frequency
        }
    }

    fn plan_admission(
        &mut self,
        ticket: MissTicket<K>,
        meta: EntryMeta<C>,
    ) -> AdmissionDecision<K, C> {
        if let Some(resident) = self.residents.peek_mut(ticket.key()) {
            resident.observation.record_cost(meta.observed_cost);
            return AdmissionDecision::Reject(AdmissionRejectReason::AlreadyResident);
        }

        // Planning consumes exactly one miss ticket and attaches that render's
        // measured cost to non-resident history. If bounded ghost churn removed
        // the key meanwhile, the ticket still carries its lookup frequency.
        let observation = if let Some(observation) = self.ghosts.get_mut(ticket.key()) {
            observation.record_cost(meta.observed_cost);
            let mut snapshot = *observation;
            snapshot.frequency = snapshot.frequency.min(ticket.frequency);
            snapshot
        } else {
            let mut observation = Observation {
                frequency: ticket.frequency.max(1),
                smoothed_cost: None,
            };
            observation.record_cost(meta.observed_cost);
            self.ghosts.put(ticket.key.clone(), observation);
            observation
        };

        let capacity = self.class_capacity(meta.class);
        if meta.weight > capacity {
            return AdmissionDecision::Reject(AdmissionRejectReason::Overweight {
                weight: meta.weight,
                capacity,
            });
        }
        if observation.frequency < ADMISSION_FREQUENCY {
            return AdmissionDecision::Reject(AdmissionRejectReason::BelowFrequency {
                frequency: observation.frequency,
                required: ADMISSION_FREQUENCY,
            });
        }

        let available = capacity.saturating_sub(self.class_weight(meta.class));
        if meta.weight <= available {
            return self.candidate(ticket.key, meta, observation, Vec::new());
        }

        let required_weight = meta.weight - available;
        let victims = self.replacement_victims(meta.class, required_weight);
        let victim_weight = victims.iter().fold(0usize, |total, (_, resident)| {
            total.saturating_add(resident.meta.weight)
        });
        debug_assert!(
            victim_weight >= required_weight,
            "resident class weight must equal the sum of resident metadata"
        );
        let mut victim_benefit = 0u128;
        let mut victim_benefit_overflowed = false;
        for (_, resident) in &victims {
            match victim_benefit.checked_add(resident.observation.benefit()) {
                Some(total) => victim_benefit = total,
                None => {
                    victim_benefit = u128::MAX;
                    victim_benefit_overflowed = true;
                }
            }
        }
        let candidate_benefit = observation.benefit();
        let worth_replacing = !victim_benefit_overflowed
            && (candidate_benefit > victim_benefit
                || (candidate_benefit == victim_benefit && meta.weight < victim_weight));
        if !worth_replacing {
            return AdmissionDecision::Reject(AdmissionRejectReason::NotWorthReplacing);
        }

        self.candidate(
            ticket.key,
            meta,
            observation,
            victims.into_iter().map(|(key, _)| key).collect(),
        )
    }

    fn validate_candidate(
        &self,
        candidate: &AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason> {
        if candidate.policy_generation != self.generation || candidate.observation.is_none() {
            return Err(CommitRejectReason::StaleCandidate);
        }
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

    fn validate_plan(
        &self,
        candidate: &AdmissionCandidate<K, C>,
        victims: &[K],
    ) -> Result<(), AdmissionRejectReason> {
        if candidate.policy_generation != self.generation || candidate.observation.is_none() {
            return Err(AdmissionRejectReason::StalePlan);
        }
        if self.contains_key(candidate.key()) {
            return Err(AdmissionRejectReason::AlreadyResident);
        }
        let meta = candidate.meta();
        let capacity = self.class_capacity(meta.class);
        if meta.weight > capacity {
            return Err(AdmissionRejectReason::Overweight {
                weight: meta.weight,
                capacity,
            });
        }
        let mut projected_weight = self.class_weight(meta.class);
        for victim in victims {
            let Some(resident) = self.residents.peek(victim) else {
                return Err(AdmissionRejectReason::StalePlan);
            };
            if resident.meta.class != meta.class {
                return Err(AdmissionRejectReason::StalePlan);
            }
            projected_weight = projected_weight.saturating_sub(resident.meta.weight);
        }
        if meta.weight > capacity.saturating_sub(projected_weight) {
            return Err(AdmissionRejectReason::StalePlan);
        }
        Ok(())
    }

    fn retain_candidate(&mut self, candidate: &AdmissionCandidate<K, C>) {
        if candidate.policy_generation != self.generation {
            return;
        }
        if let Some(observation) = self.ghosts.get_mut(candidate.key()) {
            // `get_mut` deliberately refreshes the candidate's ghost recency.
            // Preserve any lookups/cost updates newer than the plan snapshot.
            let _ = observation;
        } else if let Some(observation) = candidate.observation {
            self.ghosts.put(candidate.key().clone(), observation);
        }
    }

    fn commit_candidate(
        &mut self,
        candidate: AdmissionCandidate<K, C>,
    ) -> Result<(), CommitRejectReason> {
        self.validate_candidate(&candidate)?;
        let AdmissionCandidate {
            key,
            meta,
            observation,
            ..
        } = candidate;
        let observation = self
            .ghosts
            .pop(&key)
            .or(observation)
            .expect("a frequency candidate must carry an observation");
        let replaced = self
            .residents
            .put(key, FrequencyResident { meta, observation });
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
        let resident = self.residents.pop(key)?;
        self.subtract_weight(resident.meta);
        self.ghosts.put(key.clone(), resident.observation);
        Some((key.clone(), resident.meta))
    }

    fn victim_key(&self, class: C) -> Option<K> {
        let mut victim: Option<(&K, &FrequencyResident<C>)> = None;
        for (key, resident) in self.residents.iter().rev() {
            if resident.meta.class != class {
                continue;
            }
            let replace = match victim {
                None => true,
                Some((_, current)) => {
                    compare_density(
                        resident.observation,
                        resident.meta.weight,
                        current.observation,
                        current.meta.weight,
                    ) == Ordering::Less
                }
            };
            if replace {
                victim = Some((key, resident));
            }
        }
        victim.map(|(key, _)| key.clone())
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
        self.ghosts.clear();
        self.class_weights.clear();
        self.total_weight = 0;
        self.lookups_until_aging = AGING_INTERVAL;
        self.generation = self.generation.wrapping_add(1);
    }

    fn total_weight(&self) -> usize {
        self.total_weight
    }

    fn class_weight(&self, class: C) -> usize {
        self.class_weights.get(&class).copied().unwrap_or(0)
    }

    fn class_capacity(&self, class: C) -> usize {
        self.capacities.get(&class).copied().unwrap_or(0)
    }

    #[cfg(test)]
    fn total_capacity(&self) -> usize {
        self.capacities
            .values()
            .copied()
            .fold(0, usize::saturating_add)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.residents.len()
    }
}

impl<K, C> Default for FrequencyLruPolicy<K, C>
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
    }

    #[test]
    fn ghost_history_is_bounded_lru() {
        let mut policy = FrequencyLruPolicy::<usize, Class>::new();
        for key in 0..=GHOST_CAPACITY {
            policy.record_miss(&key);
        }

        assert_eq!(policy.ghost_len(), GHOST_CAPACITY);
        assert_eq!(policy.observation(&0), None);
        assert_eq!(policy.observation(&GHOST_CAPACITY), Some((1, 1)));
    }

    #[test]
    fn aging_deletes_zero_ghosts_but_keeps_zero_residents() {
        let mut policy = FrequencyLruPolicy::<&str, Class>::new();
        policy.set_capacity(Class::Cpu, 1);
        policy.record_miss(&"resident");
        let frequency = policy.record_miss(&"resident");
        let ticket = MissTicket::new("resident", frequency);
        let AdmissionDecision::Admit(plan) =
            policy.plan_admission(ticket, EntryMeta::with_cost(Class::Cpu, 1, 10))
        else {
            panic!("second observation should plan admission");
        };
        policy.commit_candidate(plan.into_candidate()).unwrap();
        policy.record_miss(&"ghost");

        policy.lookups_until_aging = 1;
        policy.record_miss(&"current");

        policy.lookups_until_aging = 1;
        policy.record_miss(&"current");

        assert_eq!(policy.observation(&"ghost"), None);
        assert_eq!(policy.observation(&"resident"), Some((0, 10)));
        assert_eq!(policy.observation(&"current"), Some((1, 1)));
    }

    #[test]
    fn aging_runs_before_the_40960th_lookup() {
        let mut policy = FrequencyLruPolicy::<&str, Class>::new();
        policy.record_miss(&"aged");
        for _ in 1..AGING_INTERVAL {
            policy.record_miss(&"clock");
        }

        assert_eq!(policy.observation(&"aged"), None);
        assert_eq!(policy.lookups_until_aging, AGING_INTERVAL);
    }

    #[test]
    fn density_comparison_uses_exact_fraction_and_handles_extremes() {
        let left = Observation {
            frequency: 1,
            smoothed_cost: Some(2),
        };
        let right = Observation {
            frequency: 1,
            smoothed_cost: Some(3),
        };
        assert_eq!(compare_density(left, 3, right, 5), Ordering::Greater);

        let mut extreme = Observation {
            frequency: u64::MAX,
            smoothed_cost: Some(u64::MAX),
        };
        assert_eq!(
            extreme.benefit(),
            u128::from(u64::MAX) * u128::from(u64::MAX)
        );
        extreme.record_cost(u64::MAX);
        assert_eq!(extreme.cost(), u64::MAX);
        assert_eq!(
            compare_density(extreme, usize::MAX, extreme, usize::MAX),
            Ordering::Equal
        );
    }

    #[test]
    fn overflowing_victim_benefit_sum_rejects_candidate() {
        let mut policy = FrequencyLruPolicy::<&str, Class>::new();
        policy.set_capacity(Class::Cpu, 2);
        let resident = FrequencyResident {
            meta: EntryMeta::with_cost(Class::Cpu, 1, u64::MAX),
            observation: Observation {
                frequency: u64::MAX,
                smoothed_cost: Some(u64::MAX),
            },
        };
        policy.residents.put("a", resident);
        policy.residents.put("b", resident);
        policy.class_weights.insert(Class::Cpu, 2);
        policy.total_weight = 2;

        policy.record_miss(&"candidate");
        let frequency = policy.record_miss(&"candidate");
        let ticket = MissTicket::new("candidate", frequency);
        assert_eq!(
            policy.plan_admission(ticket, EntryMeta::with_cost(Class::Cpu, 2, u64::MAX),),
            AdmissionDecision::Reject(AdmissionRejectReason::NotWorthReplacing)
        );
    }
}
