pub const EVENT_SIZE: usize = 24;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_SW: u16 = 0x05;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const KEY_VOICECOMMAND: u16 = 582;
const SW_MUTE_DEVICE: u16 = 0x0e;

const LONG_PRESS_MS: i64 = 800;
const MAX_HOLD_MS: i64 = 10_000;
// maind's buttonsClickLimiter for "activate" (quasar.cfg).
const MIN_ACTION_PRESS_MS: i64 = 56;
const MIN_ACTION_GAP_MS: i64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// Skips the timestamp: evdev stamps CLOCK_REALTIME unless EVIOCSCLOCKID.
pub fn decode(raw: &[u8; EVENT_SIZE]) -> RawEvent {
    let [.., k0, k1, c0, c1, v0, v1, v2, v3] = *raw;
    RawEvent {
        kind: u16::from_le_bytes([k0, k1]),
        code: u16::from_le_bytes([c0, c1]),
        value: i32::from_le_bytes([v0, v1, v2, v3]),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Short,
    Long,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputAction {
    Action(Press),
    MicMuted(bool),
}

pub struct Keypad {
    down_ms: Option<i64>,
    long_sent: bool,
    last_click_ms: Option<i64>,
    skip_to_report: bool,
}

impl Keypad {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            down_ms: None,
            long_sent: false,
            last_click_ms: None,
            skip_to_report: false,
        }
    }

    /// Feed queued events before a due `on_tick`: a release read at the deadline is short.
    pub fn on_event(&mut self, ev: RawEvent, now_ms: i64) -> Option<InputAction> {
        if ev.kind == EV_SYN {
            match ev.code {
                // evdev may leave half a packet after it; event-codes.rst: skip to SYN_REPORT.
                SYN_DROPPED => {
                    self.down_ms = None;
                    self.skip_to_report = true;
                }
                SYN_REPORT => self.skip_to_report = false,
                _ => {}
            }
            return None;
        }
        if self.skip_to_report {
            return None;
        }
        match (ev.kind, ev.code) {
            (EV_KEY, KEY_VOICECOMMAND) => self.on_action_key(ev.value, now_ms),
            (EV_SW, SW_MUTE_DEVICE) => Some(InputAction::MicMuted(ev.value != 0)),
            _ => None,
        }
    }

    fn on_action_key(&mut self, value: i32, now_ms: i64) -> Option<InputAction> {
        match (value, self.down_ms) {
            (1, None) => {
                self.down_ms = Some(now_ms);
                self.long_sent = false;
                None
            }
            (0, Some(down)) => {
                self.down_ms = None;
                if self.long_sent
                    || now_ms - down < MIN_ACTION_PRESS_MS
                    || self
                        .last_click_ms
                        .is_some_and(|last| now_ms - last < MIN_ACTION_GAP_MS)
                {
                    return None;
                }
                self.last_click_ms = Some(now_ms);
                Some(InputAction::Action(Press::Short))
            }
            _ => None,
        }
    }

    pub fn deadline(&self) -> Option<i64> {
        let hold = if self.long_sent {
            MAX_HOLD_MS
        } else {
            LONG_PRESS_MS
        };
        self.down_ms.map(|down| down + hold)
    }

    pub fn on_tick(&mut self, now_ms: i64) -> Option<InputAction> {
        if self.deadline().is_none_or(|d| now_ms < d) {
            return None;
        }
        if self.long_sent {
            self.down_ms = None;
            return None;
        }
        self.long_sent = true;
        Some(InputAction::Action(Press::Long))
    }

    pub fn reset(&mut self) {
        self.down_ms = None;
        self.skip_to_report = false;
    }
}

const KNOB_UP: u16 = 104;
const KNOB_DOWN: u16 = 109;

/// A knob click from `rotary-volume`: `Some(true)` louder, `Some(false)` quieter (docs/max.md).
pub fn knob_turn(ev: RawEvent) -> Option<bool> {
    if ev.kind != EV_KEY || ev.value != 1 {
        return None;
    }
    match ev.code {
        KNOB_UP => Some(true),
        KNOB_DOWN => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHORT: Option<InputAction> = Some(InputAction::Action(Press::Short));
    const LONG: Option<InputAction> = Some(InputAction::Action(Press::Long));
    const KEY_PAGEUP: u16 = 104;
    const SW_HEADPHONE_INSERT: u16 = 0x02;
    const EV_MSC: u16 = 0x04;

    fn ev(kind: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { kind, code, value }
    }

    fn action(value: i32) -> RawEvent {
        ev(EV_KEY, KEY_VOICECOMMAND, value)
    }

    fn mute(value: i32) -> RawEvent {
        ev(EV_SW, SW_MUTE_DEVICE, value)
    }

    fn syn(code: u16) -> RawEvent {
        ev(EV_SYN, code, 0)
    }

    fn click(k: &mut Keypad, at: i64, held: i64) -> Option<InputAction> {
        assert_eq!(k.on_event(action(1), at), None);
        k.on_event(action(0), at + held)
    }

    #[test]
    fn decodes_a_key_press_as_read_from_event0() {
        let raw: [u8; EVENT_SIZE] = [
            0x5c, 0x7a, 0x33, 0x6a, 0x00, 0x00, 0x00, 0x00, // tv_sec
            0x3f, 0x42, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, // tv_usec 999999
            0x01, 0x00, // EV_KEY
            0x46, 0x02, // KEY_VOICECOMMAND
            0x01, 0x00, 0x00, 0x00, // press
        ];
        assert_eq!(decode(&raw), action(1));
    }

    #[test]
    fn decodes_the_value_as_signed_little_endian() {
        let mut raw = [0xffu8; EVENT_SIZE];
        raw[16..18].copy_from_slice(&0x0002u16.to_le_bytes());
        raw[18..20].copy_from_slice(&0x0108u16.to_le_bytes());
        raw[20..24].copy_from_slice(&(-2i32).to_le_bytes());
        assert_eq!(decode(&raw), ev(2, 0x0108, -2));
    }

    #[test]
    fn a_short_press_is_reported_on_release() {
        let mut k = Keypad::new();
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_event(action(1), 1000), None);
        assert_eq!(k.deadline(), Some(1800));
        assert_eq!(k.on_event(action(0), 1200), SHORT);
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_tick(1800), None);
    }

    #[test]
    fn touches_shorter_than_56_ms_are_dropped() {
        let mut k = Keypad::new();
        assert_eq!(click(&mut k, 0, 55), None);
        assert_eq!(k.deadline(), None);
        assert_eq!(click(&mut k, 1000, 56), SHORT);
    }

    #[test]
    fn clicks_released_closer_than_300_ms_to_the_accepted_one_are_dropped() {
        let mut k = Keypad::new();
        assert_eq!(click(&mut k, 0, 100), SHORT);
        assert_eq!(click(&mut k, 150, 249), None);
        assert_eq!(click(&mut k, 420, 100), SHORT);
    }

    #[test]
    fn a_click_released_exactly_300_ms_after_the_accepted_one_passes() {
        let mut k = Keypad::new();
        assert_eq!(click(&mut k, 0, 100), SHORT);
        assert_eq!(click(&mut k, 150, 250), SHORT);
    }

    #[test]
    fn a_dropped_short_touch_does_not_move_the_gap_either() {
        let mut k = Keypad::new();
        assert_eq!(click(&mut k, 0, 100), SHORT);
        assert_eq!(click(&mut k, 350, 10), None);
        assert_eq!(click(&mut k, 380, 100), SHORT);
    }

    #[test]
    fn long_fires_at_800_ms_while_held_and_the_release_adds_nothing() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_tick(799), None);
        assert_eq!(k.deadline(), Some(800));
        assert_eq!(k.on_tick(800), LONG);
        assert_eq!(k.deadline(), Some(MAX_HOLD_MS));
        assert_eq!(k.on_event(action(0), 3000), None);
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_tick(MAX_HOLD_MS), None);
    }

    #[test]
    fn a_long_press_does_not_count_for_the_click_gap() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_tick(800), LONG);
        assert_eq!(k.on_event(action(0), 900), None);
        assert_eq!(click(&mut k, 950, 100), SHORT);
    }

    #[test]
    fn a_release_read_before_a_due_tick_is_short() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_event(action(0), 800), SHORT);
        assert_eq!(k.on_tick(800), None);
    }

    #[test]
    fn a_late_tick_fires_long_once_then_the_stuck_guard() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_tick(20_000), LONG);
        assert_eq!(k.on_tick(20_000), None);
        assert_eq!(k.deadline(), None);
    }

    #[test]
    fn a_stuck_key_is_assumed_released_10_s_after_the_press() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_tick(800), LONG);
        assert_eq!(k.on_tick(9999), None);
        assert_eq!(k.deadline(), Some(10_000));
        assert_eq!(k.on_tick(10_000), None);
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_event(action(0), 15_000), None);

        assert_eq!(k.on_event(action(1), 20_000), None);
        assert_eq!(k.deadline(), Some(20_800));
        assert_eq!(k.on_tick(20_800), LONG);
    }

    #[test]
    fn the_next_press_after_a_stuck_key_can_be_short() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_tick(800), LONG);
        assert_eq!(k.on_tick(10_000), None);
        assert_eq!(click(&mut k, 12_000, 100), SHORT);
    }

    #[test]
    fn a_second_press_while_held_is_ignored() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_event(action(1), 500), None);
        assert_eq!(k.deadline(), Some(800));
        assert_eq!(k.on_tick(800), LONG);
        assert_eq!(k.on_event(action(1), 900), None);
        assert_eq!(k.deadline(), Some(MAX_HOLD_MS));
    }

    #[test]
    fn a_release_without_a_press_and_autorepeat_are_ignored() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(0), 0), None);
        assert_eq!(k.on_event(action(2), 10), None);
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_event(action(1), 100), None);
        assert_eq!(k.on_event(action(2), 200), None);
        assert_eq!(k.on_event(action(0), 300), SHORT);
    }

    #[test]
    fn the_mute_switch_reports_microphones_off_on_1() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(mute(1), 0), Some(InputAction::MicMuted(true)));
        assert_eq!(k.on_event(mute(0), 0), Some(InputAction::MicMuted(false)));
    }

    #[test]
    fn other_events_are_ignored() {
        let mut k = Keypad::new();
        for e in [
            ev(EV_KEY, KEY_PAGEUP, 1),
            ev(EV_SW, SW_HEADPHONE_INSERT, 1),
            ev(EV_MSC, KEY_VOICECOMMAND, 1),
            syn(SYN_REPORT),
            syn(1),
        ] {
            assert_eq!(k.on_event(e, 0), None, "{e:?}");
        }
        assert_eq!(k.deadline(), None);
    }

    #[test]
    fn syn_dropped_mid_press_forgets_the_key() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_event(syn(SYN_DROPPED), 100), None);
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_tick(800), None);
        assert_eq!(k.on_event(syn(SYN_REPORT), 150), None);
        assert_eq!(k.on_event(action(0), 200), None);
        assert_eq!(click(&mut k, 1000, 100), SHORT);
    }

    #[test]
    fn syn_dropped_after_a_long_press_disarms_the_stuck_guard() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.on_tick(800), LONG);
        assert_eq!(k.on_event(syn(SYN_DROPPED), 900), None);
        assert_eq!(k.deadline(), None);
    }

    #[test]
    fn events_after_syn_dropped_are_skipped_up_to_syn_report() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(syn(SYN_DROPPED), 0), None);
        assert_eq!(k.on_event(mute(1), 0), None);
        assert_eq!(k.on_event(action(1), 0), None);
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_event(syn(SYN_REPORT), 0), None);
        assert_eq!(k.on_event(mute(1), 10), Some(InputAction::MicMuted(true)));
        assert_eq!(click(&mut k, 20, 100), SHORT);
    }

    #[test]
    fn reset_forgets_a_held_key() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(action(1), 0), None);
        k.reset();
        assert_eq!(k.deadline(), None);
        assert_eq!(k.on_tick(800), None);
        assert_eq!(k.on_event(action(0), 900), None);
        assert_eq!(click(&mut k, 1000, 100), SHORT);
    }

    #[test]
    fn reset_ends_the_skip_after_syn_dropped() {
        let mut k = Keypad::new();
        assert_eq!(k.on_event(syn(SYN_DROPPED), 0), None);
        k.reset();
        assert_eq!(k.on_event(mute(1), 0), Some(InputAction::MicMuted(true)));
    }

    #[test]
    fn knob_clicks_are_read_on_the_press() {
        assert_eq!(knob_turn(ev(EV_KEY, KEY_PAGEUP, 1)), Some(true));
        assert_eq!(knob_turn(ev(EV_KEY, 109, 1)), Some(false));
        assert_eq!(knob_turn(ev(EV_KEY, KEY_PAGEUP, 0)), None);
        assert_eq!(knob_turn(ev(EV_KEY, KEY_VOICECOMMAND, 1)), None);
        assert_eq!(knob_turn(ev(EV_MSC, KEY_PAGEUP, 1)), None);
    }
}
