//! Input boundary types shared with every backend (preview WebSocket, touch
//! device, DRM), plus conversion to Blitz `UiEvent`s.

use blitz_traits::events::{BlitzMouseButtonEvent, MouseEventButton, MouseEventButtons, UiEvent};
use keyboard_types::Modifiers;

/// Pointer buttons, matching the browser/Blitz button numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Main = 0,
    Auxiliary = 1,
    Secondary = 2,
    Fourth = 3,
    Fifth = 4,
}

impl PointerButton {
    fn to_blitz(self) -> MouseEventButton {
        match self {
            PointerButton::Main => MouseEventButton::Main,
            PointerButton::Auxiliary => MouseEventButton::Auxiliary,
            PointerButton::Secondary => MouseEventButton::Secondary,
            PointerButton::Fourth => MouseEventButton::Fourth,
            PointerButton::Fifth => MouseEventButton::Fifth,
        }
    }

    fn to_buttons(self) -> MouseEventButtons {
        match self {
            PointerButton::Main => MouseEventButtons::Primary,
            PointerButton::Auxiliary => MouseEventButtons::Auxiliary,
            PointerButton::Secondary => MouseEventButtons::Secondary,
            PointerButton::Fourth => MouseEventButtons::Fourth,
            PointerButton::Fifth => MouseEventButtons::Fifth,
        }
    }
}

/// A pointer event with its position in logical (device-independent) pixels.
///
/// The Touch Bar and preview canvas operate in physical pixels; the runtime
/// converts physical coordinates to logical coordinates using the scale factor
/// before constructing these events.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub button: PointerButton,
    /// Bitmask of all currently-pressed buttons.
    pub buttons: u8,
    pub kind: PointerEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerEventKind {
    Down,
    Up,
    Move,
}

impl PointerEvent {
    fn blitz_buttons(&self) -> MouseEventButtons {
        MouseEventButtons::from_bits_retain(self.buttons)
    }

    fn to_blitz_mouse_event(
        &self,
        button: MouseEventButton,
        buttons: MouseEventButtons,
    ) -> BlitzMouseButtonEvent {
        BlitzMouseButtonEvent {
            x: self.x,
            y: self.y,
            button,
            buttons,
            mods: Modifiers::empty(),
        }
    }

    /// Convert this framework-level event into a Blitz `UiEvent`.
    pub fn to_ui_event(&self) -> UiEvent {
        match self.kind {
            PointerEventKind::Down => {
                let button = self.button.to_blitz();
                let buttons = self.blitz_buttons() | self.button.to_buttons();
                UiEvent::MouseDown(self.to_blitz_mouse_event(button, buttons))
            }
            PointerEventKind::Up => {
                let button = self.button.to_blitz();
                let buttons = self.blitz_buttons() & !self.button.to_buttons();
                UiEvent::MouseUp(self.to_blitz_mouse_event(button, buttons))
            }
            PointerEventKind::Move => {
                let button = MouseEventButton::Main;
                let buttons = self.blitz_buttons();
                UiEvent::MouseMove(self.to_blitz_mouse_event(button, buttons))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: PointerEventKind, x: f32, y: f32) -> PointerEvent {
        PointerEvent {
            x,
            y,
            button: PointerButton::Main,
            buttons: 0,
            kind,
        }
    }

    #[test]
    fn test_mouse_down_conversion() {
        let e = PointerEvent {
            x: 100.0,
            y: 20.0,
            button: PointerButton::Main,
            buttons: 0,
            kind: PointerEventKind::Down,
        };
        let ui = e.to_ui_event();
        match ui {
            UiEvent::MouseDown(me) => {
                assert_eq!(me.x, 100.0);
                assert_eq!(me.y, 20.0);
                assert_eq!(me.button, MouseEventButton::Main);
                assert!(me.buttons.contains(MouseEventButtons::Primary));
            }
            other => panic!("expected MouseDown, got {other:?}"),
        }
    }

    #[test]
    fn test_mouse_up_conversion_clears_button() {
        let e = PointerEvent {
            x: 0.0,
            y: 0.0,
            button: PointerButton::Main,
            buttons: 0b0000_0001,
            kind: PointerEventKind::Up,
        };
        let ui = e.to_ui_event();
        match ui {
            UiEvent::MouseUp(me) => {
                assert!(!me.buttons.contains(MouseEventButtons::Primary));
            }
            other => panic!("expected MouseUp, got {other:?}"),
        }
    }

    #[test]
    fn test_mouse_move_conversion() {
        let ui = event(PointerEventKind::Move, 42.0, 7.0).to_ui_event();
        match ui {
            UiEvent::MouseMove(me) => {
                assert_eq!(me.x, 42.0);
                assert_eq!(me.y, 7.0);
            }
            other => panic!("expected MouseMove, got {other:?}"),
        }
    }
}
