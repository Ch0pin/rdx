//! Shared accounting for optional project caches. Admission never truncates a query.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Debug)]
pub(crate) struct RetainedBudget {
    used: AtomicUsize,
    limit: usize,
}
impl Default for RetainedBudget {
    fn default() -> Self {
        Self::new(384 * 1024 * 1024)
    }
}
impl RetainedBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit,
        }
    }
    pub(crate) fn reserve(self: &Arc<Self>, bytes: usize) -> Option<Arc<Reservation>> {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let next = used.checked_add(bytes).filter(|next| *next <= self.limit)?;
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => used = actual,
            }
        }
        Some(Arc::new(Reservation {
            owner: self.clone(),
            bytes,
        }))
    }
    #[cfg(test)]
    fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }
}
#[derive(Debug)]
pub(crate) struct Reservation {
    owner: Arc<RetainedBudget>,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.owner.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservations_share_one_limit_and_live_readers_keep_their_charge() {
        let budget = Arc::new(RetainedBudget::new(100));
        let cache = budget.reserve(60).unwrap();
        let reader = cache.clone();
        assert!(budget.reserve(41).is_none());
        drop(cache);
        assert_eq!(budget.used(), 60);
        drop(reader);
        assert_eq!(budget.used(), 0);
        assert!(budget.reserve(100).is_some());
        assert!(budget.reserve(usize::MAX).is_none());
    }
}
