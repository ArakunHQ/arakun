//! Running a session for an issue delegated to Arakun in Linear, as the relay
//! passes it on.

use gpui::{Context, Window};

use super::Shell;
use crate::phone::linear::Delivery;

impl Shell {
    /// A delivery from the relay. A later change takes it and starts, prompts
    /// or stops the session; until then it is left for the relay to send again.
    pub(super) fn linear_delivery(
        &mut self,
        _delivery: Delivery,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}
