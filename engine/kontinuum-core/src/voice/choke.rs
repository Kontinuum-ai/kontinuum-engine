//! Lock-free choke-group state for sampler voices (issue #19 hat logic).
//! Voices sharing an `Arc<ChokeState>` in the same group choke each other:
//! a voice's `note_on` stamps a fresh epoch, and any same-group voice still
//! holding an older epoch fast-fades within 10 ms. Counter-based, so the
//! outcome is a pure function of the trigger order — no wall clock, no
//! allocation, RT-safe (relaxed atomics only).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Group ids 1..=CHOKE_GROUPS; slot 0 is unused so id 0 stays "no choke".
pub const CHOKE_GROUPS: usize = 16;

/// The hat track's choke group: open and closed hats share one group, so any
/// hat retrigger chokes the previous one regardless of pool slot (#14).
pub const CHOKE_GROUP_HATS: u8 = 1;

#[derive(Default)]
pub struct ChokeState {
    epochs: [AtomicU64; CHOKE_GROUPS + 1],
}

impl ChokeState {
    pub fn shared() -> Arc<Self> {
        Arc::new(ChokeState::default())
    }

    /// Stamp a new trigger epoch for `group` (1..=CHOKE_GROUPS) and return
    /// it. Invalid ids are ignored (returns 0, which never owns a voice).
    pub fn trigger(&self, group: u8) -> u64 {
        if group == 0 || group as usize > CHOKE_GROUPS {
            return 0;
        }
        self.epochs[group as usize].fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The epoch a voice in `group` must hold to keep sounding.
    pub fn current(&self, group: u8) -> u64 {
        if group == 0 || group as usize > CHOKE_GROUPS {
            return 0;
        }
        self.epochs[group as usize].load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_advance_and_outdate_older_holders() {
        let state = ChokeState::shared();
        assert_eq!(state.trigger(1), 1);
        assert_eq!(state.trigger(1), 2);
        assert_eq!(state.current(1), 2);
        assert_eq!(state.trigger(2), 1, "groups are independent");
        assert_eq!(state.current(2), 1);
        // A voice holding epoch 1 in group 1 is stale now.
        assert_ne!(state.current(1), 1);
    }

    #[test]
    fn invalid_group_ids_never_own_a_voice() {
        let state = ChokeState::shared();
        assert_eq!(state.trigger(0), 0);
        assert_eq!(state.trigger((CHOKE_GROUPS + 1) as u8), 0);
        assert_eq!(state.current(0), 0);
    }
}
