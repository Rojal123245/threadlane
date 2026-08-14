//! Chat service boundary.
//!
//! Backend execution currently remains in `AppState::send_prompt`; this facade
//! is the migration seam for moving async agent work out of UI state without
//! changing session behavior in one large step.
