//! Language-neutral ownership lives in the SDK; conversion stays in this binding.
pub use omnibci_sdk::session::{Stm32Device as Device, discover_stm32_ble as discover};
