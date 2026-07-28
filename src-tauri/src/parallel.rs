//! A small scoped work queue for the filesystem-bound loops.
//!
//! Skill Manager spends nearly all of its time walking skill directories, and
//! on Windows every file it opens is also an antivirus scan. That makes the
//! work latency-bound rather than CPU-bound, so running several directories at
//! once is close to a linear speed-up even on a virtual machine with two
//! cores. A work queue over `std::thread::scope` borrows the caller's data
//! directly, needs no dependency, and keeps the installer small — which
//! matters, because the installed binary is itself scanned on every launch.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Enough workers to keep several antivirus-mediated reads outstanding on a
/// two-core virtual machine, and few enough that a large catalog cannot swamp
/// a shared host.
const MIN_WORKERS: usize = 4;
const MAX_WORKERS: usize = 8;

fn worker_count(items: usize) -> usize {
    let available = std::thread::available_parallelism().map_or(MIN_WORKERS, NonZeroUsize::get);
    items.min(available.clamp(MIN_WORKERS, MAX_WORKERS))
}

/// Applies `operation` to every item and returns the results in the original
/// order. A panic inside `operation` propagates once the workers have joined,
/// exactly as it would from a sequential loop.
pub(crate) fn map<T, R, F>(items: &[T], operation: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Send + Sync,
{
    let workers = worker_count(items.len());
    if workers < 2 {
        return items.iter().map(operation).collect();
    }

    let next = AtomicUsize::new(0);
    let results = Mutex::new((0..items.len()).map(|_| None).collect::<Vec<Option<R>>>());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else {
                    return;
                };
                let value = operation(item);
                let mut results = results.lock().unwrap_or_else(|error| error.into_inner());
                results[index] = Some(value);
            });
        }
    });

    results
        .into_inner()
        .unwrap_or_else(|error| error.into_inner())
        .into_iter()
        .map(|value| value.expect("every index is filled by exactly one worker"))
        .collect()
}

/// Applies a fallible `operation` to every item and reports the first failure
/// in item order, so the message a caller sees never depends on scheduling.
pub(crate) fn try_map<T, R, F>(items: &[T], operation: F) -> Result<Vec<R>, String>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Result<R, String> + Send + Sync,
{
    map(items, operation).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_item_in_order() {
        let items = (0..1_000_u32).collect::<Vec<_>>();
        assert_eq!(
            map(&items, |item| item * 2),
            items.iter().map(|item| item * 2).collect::<Vec<_>>()
        );
        assert!(map::<u32, u32, _>(&[], |item| *item).is_empty());
    }

    #[test]
    fn reports_the_first_failure_in_item_order() {
        let items = (0..500_u32).collect::<Vec<_>>();
        let outcome = try_map(&items, |item| {
            if item % 100 == 7 {
                Err(format!("item {item}"))
            } else {
                Ok(*item)
            }
        });
        assert_eq!(outcome, Err("item 7".to_string()));
        assert_eq!(try_map(&items, |item| Ok(*item)), Ok(items.clone()));
    }
}
