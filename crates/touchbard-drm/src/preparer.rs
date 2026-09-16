//! Opt-in hardware preparation, selected by hardware identity.

use crate::touchbar::{TouchBarDevicePreparer, TOUCHBAR_ID};

/// A hardware identity used to select a known preparation workaround.
///
/// Generic DRM code treats this as an opaque key: only the workaround lookup
/// consults it, and only Touch Bar-specific code ([`crate::touchbar`]) knows
/// which identity means the Touch Bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HardwareId {
    /// USB vendor id. Vendor ids are assigned by the USB-IF; Touch Bar-specific
    /// knowledge of which vendor/product pair is the Touch Bar lives in
    /// [`crate::touchbar`].
    pub vendor_id: u16,
    /// USB product id.
    pub product_id: u16,
}

/// Opt-in hardware preparation applied only when a device needs one.
///
/// DRM does not assume every device needs preparation: most hardware is driven
/// purely through normal discovery, and a [`DevicePreparer`] exists only for
/// hardware identities with a *known* workaround, selected by [`HardwareId`].
///
/// A preparer is not a DRM driver: it prepares the hardware (for the Touch
/// Bar, the USB workaround in [`crate::touchbar`]) and owns no framebuffer,
/// modeset, or rendering logic. Finding the DRM card stays with the generic
/// discovery code.
pub trait DevicePreparer {
    /// Prepare the hardware so its DRM device becomes discoverable.
    fn prepare(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// The known preparation workaround for `identity`, if any.
///
/// Unknown identities yield `None`: a workaround is never guessed for
/// unrecognized hardware. The only known workaround is the Touch Bar's
/// hardware preparation.
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

    /// Unknown hardware must never get a guessed workaround: only the listed
    /// identities map to a preparer.
    #[test]
    fn unknown_identity_has_no_workaround() {
        let unknown = HardwareId {
            vendor_id: 0x1d6b, // Linux Foundation root hub
            product_id: 0x0002,
        };
        assert!(workaround_for(unknown).is_none());
    }
}
