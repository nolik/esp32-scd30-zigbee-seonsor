//! Hardware-free core of the SCD30 Zigbee sensor: calibration rules, ZCL value encoding and
//! the sensor service state machine. Host-tested with `cargo test`; the firmware crate only
//! supplies trait implementations.
pub mod calib;
pub mod zcl;
pub mod sensor;
