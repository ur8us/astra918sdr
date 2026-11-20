//! Lionel has no LDIV input, RF outputs, PTT, or TX indicator connections.
use astra918_firmware::Error;

pub(crate) fn set_output_config(_ma: u8, _fast: bool) -> Result<(), Error> {
    Ok(())
}
pub(crate) fn set_polarity(_differential: bool) -> Result<(), Error> {
    Ok(())
}
pub(crate) fn rf_enabled() -> bool {
    false
}
pub(crate) unsafe fn tx_divider_enable(_divider: u16, _differential: bool) {}
pub(crate) unsafe fn tx_divider_disable() {}
