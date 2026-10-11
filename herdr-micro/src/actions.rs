//! The Micro's Herdr layer. What its controls do lives in `daemon::dispatch`:
//! a jump through Herdr, or keyboard events through `herdr_device`.

pub use codex_micro::service::layer_identity;

pub const HERDR_LAYER: usize = 2;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PLUGIN_ID;

    #[test]
    fn layer_identity_matches_the_plugin_id() {
        assert_eq!(layer_identity(2).process, format!("{PLUGIN_ID}.layer-2"));
    }
}
