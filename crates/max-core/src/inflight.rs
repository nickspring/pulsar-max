use std::collections::BTreeSet;

use crate::dialog::PlayId;
use crate::player_proto::{PlayerEvent, play_id_of};

/// Plays that have not reported `done`. Each id leaves the set once, and only that removal
/// yields its `Done`: whether the helper answers, the command never reaches it, or it dies.
#[derive(Debug, Default)]
pub struct InFlight {
    ids: BTreeSet<u64>,
}

impl InFlight {
    pub fn start(&mut self, id: PlayId) {
        self.ids.insert(id.0);
    }

    pub fn finish(&mut self, id: PlayId, ok: bool) -> Option<PlayerEvent> {
        self.ids
            .remove(&id.0)
            .then_some(PlayerEvent::Done { id, ok })
    }

    pub fn fail_unsent(&mut self, line: &str) -> Option<PlayerEvent> {
        self.finish(play_id_of(line)?, false)
    }

    pub fn fail_all(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.ids)
            .into_iter()
            .map(|id| PlayerEvent::Done {
                id: PlayId(id),
                ok: false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(id: u64, ok: bool) -> PlayerEvent {
        PlayerEvent::Done { id: PlayId(id), ok }
    }

    #[test]
    fn a_play_finishes_once() {
        let mut f = InFlight::default();
        f.start(PlayId(1));
        assert_eq!(f.finish(PlayId(1), true), Some(done(1, true)));
        assert_eq!(f.finish(PlayId(1), false), None);
    }

    #[test]
    fn unknown_ids_yield_nothing() {
        let mut f = InFlight::default();
        assert_eq!(f.finish(PlayId(9), true), None);
    }

    #[test]
    fn helper_death_fails_every_open_play_and_late_answers_are_dropped() {
        let mut f = InFlight::default();
        for id in [3, 1, 2] {
            f.start(PlayId(id));
        }
        f.finish(PlayId(2), true);
        assert_eq!(f.fail_all(), [done(1, false), done(3, false)]);
        assert_eq!(f.finish(PlayId(1), true), None);
        assert!(f.fail_all().is_empty());
    }

    #[test]
    fn an_unsent_play_command_fails_its_play_only() {
        let mut f = InFlight::default();
        f.start(PlayId(4));
        assert_eq!(f.fail_unsent("stop\tmusic"), None);
        assert_eq!(
            f.fail_unsent("play\t4\tmusic\thttp://a"),
            Some(done(4, false))
        );
        assert_eq!(f.fail_unsent("play\t4\tmusic\thttp://a"), None);
    }
}
