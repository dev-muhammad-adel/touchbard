//! Deterministic reaction-challenge model for the home showcase (`/`).
//!
//! The challenge is a hit-test on a horizontal strip of target slots: the
//! target advances one slot (wrapping) on every hit, and tapping any other slot
//! is a miss. The model is deliberately pure - no DOM, no timing - so the rules
//! are unit-testable away from the routed page that renders them.

/// Number of slots the challenge cycles through. Fixed at seven so the target
/// positions are deterministic and the strip stays comfortably tappable.
pub const SLOTS: usize = 7;

/// Where the challenge currently is: waiting for input, or the result of the
/// most recent tap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Before the first tap: the target is waiting.
    Ready,
    /// The most recent tap landed on the target.
    Hit,
    /// The most recent tap missed the target.
    Miss,
}

impl Phase {
    /// Short state label rendered on the status chip.
    pub const fn label(self) -> &'static str {
        match self {
            Phase::Ready => "READY",
            Phase::Hit => "HIT",
            Phase::Miss => "MISS",
        }
    }

    /// One-shot CSS flash class that replays after the last input (the page
    /// re-keys the stage on every tap), or `""` while idle.
    pub const fn flash_class(self) -> &'static str {
        match self {
            Phase::Ready => "",
            Phase::Hit => "flash-hit",
            Phase::Miss => "flash-miss",
        }
    }
}

/// Deterministic next target slot after a hit: advance one slot, wrapping.
pub fn next_target(current: usize) -> usize {
    (current + 1) % SLOTS
}

/// The result of tapping slot `i` while the target lives in slot `target`.
pub fn scored(i: usize, target: usize) -> Phase {
    if i == target {
        Phase::Hit
    } else {
        Phase::Miss
    }
}

/// Status-chip text colour per phase (the only presentation mapping worth
/// sharing with the tests: it encodes the success/failure visual state).
pub fn chip_color(phase: Phase) -> &'static str {
    match phase {
        Phase::Ready => "#c0caf5",
        Phase::Hit => "#9ece6a",
        Phase::Miss => "#f7768e",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_target_cycles_through_all_slots() {
        assert_eq!(SLOTS, 7, "the challenge is seven slots wide");
        for i in 0..SLOTS {
            assert_eq!(next_target(i), (i + 1) % SLOTS, "target advances by one");
        }
    }

    #[test]
    fn scored_maps_slot_to_phase() {
        assert_eq!(
            scored(3, 3),
            Phase::Hit,
            "the slot under the target is a hit"
        );
        assert_ne!(scored(4, 3), Phase::Hit, "any other slot is not a hit");
        assert_eq!(scored(4, 3), Phase::Miss);
        assert_eq!(scored(0, 6), Phase::Miss);
        assert_eq!(scored(6, 0), Phase::Miss);
    }

    #[test]
    fn phase_labels_are_short_and_distinct() {
        let labels: Vec<&str> = [Phase::Ready, Phase::Hit, Phase::Miss]
            .iter()
            .map(|p| p.label())
            .collect();
        assert_eq!(labels, vec!["READY", "HIT", "MISS"]);
    }

    #[test]
    fn ready_phase_has_no_flash_animation() {
        assert_eq!(Phase::Ready.flash_class(), "");
        assert_eq!(Phase::Hit.flash_class(), "flash-hit");
        assert_eq!(Phase::Miss.flash_class(), "flash-miss");
    }

    #[test]
    fn chip_color_follows_phase() {
        assert_eq!(chip_color(Phase::Ready), "#c0caf5");
        assert_eq!(chip_color(Phase::Hit), "#9ece6a");
        assert_eq!(chip_color(Phase::Miss), "#f7768e");
    }

    #[test]
    fn a_hit_sequence_walks_the_target_all_the_way_around() {
        let mut target = 0usize;
        let mut seen = [false; SLOTS];
        for _ in 0..SLOTS {
            seen[target] = true;
            assert_eq!(scored(target, target), Phase::Hit);
            target = next_target(target);
        }
        assert!(
            seen.iter().all(|&s| s),
            "a full hit-cycle visits every slot once"
        );
        assert_eq!(target, 0, "seven hits walk the target all the way around");
    }
}
