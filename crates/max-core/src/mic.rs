/// pulsar's rules (controller/logic.rs): HA cannot lift the hardware mute, and the switch alone
/// always brings the microphones back.
#[derive(Debug, Default)]
pub struct Mics {
    sw_muted: bool,
    hw_muted: bool,
}

impl Mics {
    pub fn muted(&self) -> bool {
        self.sw_muted || self.hw_muted
    }

    pub fn hw_muted(&self) -> bool {
        self.hw_muted
    }

    pub fn set_switch(&mut self, on: bool) {
        if !(on && self.hw_muted) {
            self.sw_muted = !on;
        }
    }

    pub fn set_hw(&mut self, muted: bool) {
        self.hw_muted = muted;
        self.sw_muted &= muted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ha_switch_mutes_and_unmutes_when_hardware_allows() {
        let mut m = Mics::default();
        assert!(!m.muted());
        m.set_switch(false);
        assert!(m.muted());
        m.set_switch(true);
        assert!(!m.muted());
    }

    #[test]
    fn ha_cannot_lift_the_hardware_mute() {
        let mut m = Mics::default();
        m.set_hw(true);
        m.set_switch(true);
        assert!(m.muted());
        assert!(m.hw_muted());
        m.set_hw(false);
        assert!(!m.muted());
    }

    #[test]
    fn ha_mute_under_hardware_mute_is_kept_and_lifted_with_the_switch() {
        let mut m = Mics::default();
        m.set_hw(true);
        m.set_switch(false);
        m.set_switch(true);
        assert!(m.muted());
        m.set_hw(false);
        assert!(!m.muted());
    }

    #[test]
    fn lifting_the_switch_also_lifts_an_earlier_ha_mute() {
        let mut m = Mics::default();
        m.set_switch(false);
        m.set_hw(true);
        assert!(m.muted());
        m.set_hw(false);
        assert!(!m.muted());
    }
}
