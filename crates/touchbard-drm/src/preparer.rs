//! Opt-in hardware preparation, selected by hardware identity.

use crate::touchbar::{TouchBarDevicePreparer, TOUCHBAR_ID};

/// A hardware identity used to select a preparation workaround.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HardwareId {
    pub vendor_id: u16,
    pub product_id: u16,
}

/// Optional hardware preparation used before DRM-card discovery.
pub trait DevicePreparer {
    fn prepare(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Recover a prepared device whose DRM card is still absent.
    fn recover_no_card(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

/// Return the preparation workaround for `identity`, if one is known.
pub fn workaround_for(identity: HardwareId) -> Option<Box<dyn DevicePreparer>> {
    if identity == TOUCHBAR_ID {
        Some(Box::new(TouchBarDevicePreparer::new()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_bar_identity_has_a_known_workaround() {
        assert!(workaround_for(TOUCHBAR_ID).is_some());
    }

    #[test]
    fn unknown_identity_has_no_workaround() {
        let unknown = HardwareId {
            vendor_id: 0x1d6b,
            product_id: 0x0002,
        };
        assert!(workaround_for(unknown).is_none());
    }
}
