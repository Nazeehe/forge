//! Modal dialogs: every keyboard-activatable control is clickable.
//!
//! Creation, group management, quit, theming, Telegram setup, card
//! editing, and first-run onboarding. An open modal swallows all mouse
//! input; clicks behind it move nothing.

pub mod card_edit;
pub mod create;
pub mod groups;
pub mod oobe;
pub mod quit;
pub mod telegram;
pub mod theme;
