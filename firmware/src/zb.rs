//! The only module that touches the ZBOSS C API (all `unsafe` lives here). Builds the frozen
//! Zigbee identity from docs/z2m-baseline.json, runs the stack on its own thread and offers
//! a safe `Reporter` for the sensor thread.
use core::ffi::c_void;
use core::ptr::addr_of_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use esp_idf_svc::sys::{esp, zb as ffi};
use logic::sensor::Reporter;
use logic::zcl::{
    Report, CO2_MAX_FRACTION, CO2_PLACEHOLDER, HUMIDITY_MAX_CENTI, HUMIDITY_PLACEHOLDER, TEMP_MAX_CENTI,
    TEMP_MIN_CENTI, TEMP_PLACEHOLDER,
};

// Numeric values from the ZCL / HA specs (stable across SDK versions).
const ENDPOINT: u8 = 1;
const HA_PROFILE_ID: u16 = 0x0104;
const TEMPERATURE_SENSOR_DEVICE_ID: u16 = 0x0302;
const CLUSTER_ON_OFF: u16 = 0x0006;
const CLUSTER_TEMPERATURE: u16 = 0x0402;
const CLUSTER_HUMIDITY: u16 = 0x0405;
const CLUSTER_CO2: u16 = 0x040D;
const ATTR_MEASURED_VALUE: u16 = 0x0000;
const ATTR_ON_OFF: u16 = 0x0000;
const ATTR_BASIC_MANUFACTURER_NAME: u16 = 0x0004;
const ATTR_BASIC_MODEL_IDENTIFIER: u16 = 0x0005;
const SERVER_ROLE: u8 = 0x01;
const ZCL_TYPE_BOOL: u32 = 0x10;
const ZCL_STATUS_SUCCESS: u32 = 0x00;
const ALL_CHANNELS_MASK: u32 = 0x07FF_F800;
const ED_KEEP_ALIVE_MS: u32 = 3000;
// Task 9 Step 1: values of ESP_ZB_ZCL_BASIC_{ZCL_VERSION,POWER_SOURCE}_DEFAULT_VALUE.
const BASIC_ZCL_VERSION: u8 = 0x08;
const BASIC_POWER_SOURCE: u8 = 0x00; // SDK default, same as the C++ firmware (Z2M shows mains via node descriptor)

// ZCL strings are length-prefixed (Pascal style); ZBOSS keeps pointers to them.
static mut MANUFACTURER_NAME: [u8; 6] = *b"\x05Nolik";
static mut MODEL_IDENTIFIER: [u8; 21] = *b"\x14ESP32C6-SCD30-Zigbee";

/// Set once the stack reports SKIP_STARTUP; attribute writes before that are dropped.
static STACK_READY: AtomicBool = AtomicBool::new(false);
/// Where the "Trigger Recalibration" switch sends requests (the sensor thread).
static RECALIBRATION_REQUESTS: OnceLock<Sender<()>> = OnceLock::new();

/// Configure the radio and start the Zigbee stack on its own thread. Call once, after NVS
/// has been initialised.
pub fn start(recalibration_requests: Sender<()>) -> anyhow::Result<()> {
    RECALIBRATION_REQUESTS.set(recalibration_requests).map_err(|_| anyhow::anyhow!("zb::start called twice"))?;
    // SAFETY: plain C struct; all-zero = native radio, no host connection (the defaults the
    // C++ firmware set explicitly).
    let mut platform: ffi::esp_zb_platform_config_t = unsafe { core::mem::zeroed() };
    esp!(unsafe { ffi::esp_zb_platform_config(&mut platform) })?;
    std::thread::Builder::new()
        .name("zigbee".into())
        .stack_size(16 * 1024)
        // SAFETY: run_stack is entered exactly once, on this dedicated thread.
        .spawn(|| unsafe { run_stack() })?;
    Ok(())
}

unsafe fn run_stack() {
    let mut cfg: ffi::esp_zb_cfg_t = core::mem::zeroed();
    cfg.esp_zb_role = ffi::esp_zb_nwk_device_type_t_ESP_ZB_DEVICE_TYPE_ED as _;
    cfg.install_code_policy = false;
    cfg.nwk_cfg.zed_cfg.ed_timeout = ffi::esp_zb_aging_timeout_t_ESP_ZB_ED_AGING_TIMEOUT_64MIN as _;
    cfg.nwk_cfg.zed_cfg.keep_alive = ED_KEEP_ALIVE_MS as _;
    ffi::esp_zb_init(&mut cfg);

    esp!(ffi::esp_zb_device_register(build_endpoint())).expect("esp_zb_device_register");
    ffi::esp_zb_core_action_handler_register(Some(action_handler));
    ffi::esp_zb_set_primary_network_channel_set(ALL_CHANNELS_MASK);

    log::info!("Starting Zigbee Stack...");
    esp!(ffi::esp_zb_start(false)).expect("esp_zb_start");
    ffi::esp_zb_stack_main_loop();
}

unsafe fn build_endpoint() -> *mut ffi::esp_zb_ep_list_t {
    let mut basic_cfg: ffi::esp_zb_basic_cluster_cfg_t = core::mem::zeroed();
    basic_cfg.zcl_version = BASIC_ZCL_VERSION;
    basic_cfg.power_source = BASIC_POWER_SOURCE;
    let basic = ffi::esp_zb_basic_cluster_create(&mut basic_cfg);
    ffi::esp_zb_basic_cluster_add_attr(basic, ATTR_BASIC_MANUFACTURER_NAME, addr_of_mut!(MANUFACTURER_NAME).cast());
    ffi::esp_zb_basic_cluster_add_attr(basic, ATTR_BASIC_MODEL_IDENTIFIER, addr_of_mut!(MODEL_IDENTIFIER).cast());

    let mut identify_cfg: ffi::esp_zb_identify_cluster_cfg_t = core::mem::zeroed(); // identify_time 0

    let mut co2_cfg: ffi::esp_zb_carbon_dioxide_measurement_cluster_cfg_t = core::mem::zeroed();
    co2_cfg.measured_value = CO2_PLACEHOLDER.0;
    co2_cfg.min_measured_value = 0.0;
    co2_cfg.max_measured_value = CO2_MAX_FRACTION;

    let mut temp_cfg: ffi::esp_zb_temperature_meas_cluster_cfg_t = core::mem::zeroed();
    temp_cfg.measured_value = TEMP_PLACEHOLDER.0; // 0, never 0x8000 (README Step 6)
    temp_cfg.min_value = TEMP_MIN_CENTI;
    temp_cfg.max_value = TEMP_MAX_CENTI;

    let mut humidity_cfg: ffi::esp_zb_humidity_meas_cluster_cfg_t = core::mem::zeroed();
    humidity_cfg.measured_value = HUMIDITY_PLACEHOLDER.0;
    humidity_cfg.min_value = 0;
    humidity_cfg.max_value = HUMIDITY_MAX_CENTI;

    let mut on_off_cfg: ffi::esp_zb_on_off_cluster_cfg_t = core::mem::zeroed(); // off

    let list = ffi::esp_zb_zcl_cluster_list_create();
    ffi::esp_zb_cluster_list_add_basic_cluster(list, basic, SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_identify_cluster(list, ffi::esp_zb_identify_cluster_create(&mut identify_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_carbon_dioxide_measurement_cluster(
        list, ffi::esp_zb_carbon_dioxide_measurement_cluster_create(&mut co2_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_temperature_meas_cluster(list, ffi::esp_zb_temperature_meas_cluster_create(&mut temp_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_humidity_meas_cluster(list, ffi::esp_zb_humidity_meas_cluster_create(&mut humidity_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_on_off_cluster(list, ffi::esp_zb_on_off_cluster_create(&mut on_off_cfg), SERVER_ROLE);

    let mut ep_cfg: ffi::esp_zb_endpoint_config_t = core::mem::zeroed(); // app_device_version 0
    ep_cfg.endpoint = ENDPOINT;
    ep_cfg.app_profile_id = HA_PROFILE_ID;
    ep_cfg.app_device_id = TEMPERATURE_SENSOR_DEVICE_ID;
    let ep_list = ffi::esp_zb_ep_list_create();
    ffi::esp_zb_ep_list_add_ep(ep_list, list, ep_cfg);
    ep_list
}

fn commission(mode: u32) {
    // SAFETY: called from the stack's own signal handler (Zigbee thread).
    unsafe { ffi::esp_zb_bdb_start_top_level_commissioning(mode as _) };
}

/// Called by ZBOSS on the Zigbee thread. Must never start threads: steering re-fires on every
/// rejoin (the C++ firmware's duplicate-SCD30_Task bug). The sensor thread is started once
/// from `main`.
#[no_mangle]
pub unsafe extern "C" fn esp_zb_app_signal_handler(signal: *mut ffi::esp_zb_app_signal_t) {
    let Some(signal) = signal.as_ref() else { return };
    let sig_type = *signal.p_app_signal;
    let status = signal.esp_err_status;
    match sig_type {
        t if t == ffi::esp_zb_app_signal_type_t_ESP_ZB_ZDO_SIGNAL_SKIP_STARTUP as u32 => {
            log::info!("Zigbee stack initialized");
            STACK_READY.store(true, Ordering::Release);
            commission(ffi::esp_zb_bdb_commissioning_mode_t_ESP_ZB_BDB_MODE_INITIALIZATION as u32);
        }
        t if t == ffi::esp_zb_app_signal_type_t_ESP_ZB_BDB_SIGNAL_DEVICE_FIRST_START as u32
            || t == ffi::esp_zb_app_signal_type_t_ESP_ZB_BDB_SIGNAL_DEVICE_REBOOT as u32 =>
        {
            log::info!("Start network steering");
            commission(ffi::esp_zb_bdb_commissioning_mode_t_ESP_ZB_BDB_MODE_NETWORK_STEERING as u32);
        }
        t if t == ffi::esp_zb_app_signal_type_t_ESP_ZB_BDB_SIGNAL_STEERING as u32 => {
            if status == 0 {
                log::info!("Successfully joined Zigbee network!");
            } else {
                log::warn!("Failed to join network (status: {status}), retrying...");
                commission(ffi::esp_zb_bdb_commissioning_mode_t_ESP_ZB_BDB_MODE_NETWORK_STEERING as u32);
            }
        }
        _ => log::info!("ZDO signal: {sig_type}, status: {status}"),
    }
}

/// Attribute writes from Zigbee2MQTT. Only the On/Off "Trigger Recalibration" switch matters:
/// turning it on sends a request to the sensor thread (which ignores duplicates).
unsafe extern "C" fn action_handler(
    callback_id: ffi::esp_zb_core_action_callback_id_t,
    message: *const c_void,
) -> ffi::esp_err_t {
    if callback_id != ffi::esp_zb_core_action_callback_id_s_ESP_ZB_CORE_SET_ATTR_VALUE_CB_ID {
        log::warn!("Receive Zigbee action(0x{:x}) callback", callback_id as u32);
        return 0;
    }
    let Some(msg) = message.cast::<ffi::esp_zb_zcl_set_attr_value_message_t>().as_ref() else {
        return esp_idf_svc::sys::ESP_FAIL as _;
    };
    if msg.info.status as u32 != ZCL_STATUS_SUCCESS {
        return esp_idf_svc::sys::ESP_ERR_INVALID_ARG as _;
    }
    let is_switch = msg.info.dst_endpoint == ENDPOINT
        && msg.info.cluster == CLUSTER_ON_OFF
        && msg.attribute.id == ATTR_ON_OFF
        && msg.attribute.data.type_ as u32 == ZCL_TYPE_BOOL;
    if is_switch {
        let value = msg.attribute.data.value.cast::<bool>();
        if !value.is_null() && *value {
            if let Some(tx) = RECALIBRATION_REQUESTS.get() {
                let _ = tx.send(());
            }
        }
    }
    0
}

/// Writes ZCL attributes from the sensor thread under the Zigbee lock.
pub struct ZigbeeReporter;

impl ZigbeeReporter {
    fn with_lock(f: impl FnOnce()) {
        if !STACK_READY.load(Ordering::Acquire) {
            log::debug!("Zigbee stack not ready - attribute write dropped");
            return;
        }
        // SAFETY: the ZBOSS API requires holding the stack lock for calls from other tasks.
        unsafe { ffi::esp_zb_lock_acquire(u32::MAX) };
        f();
        unsafe { ffi::esp_zb_lock_release() };
    }
}

/// SAFETY: caller holds the Zigbee lock; `T` matches the attribute's ZCL type.
unsafe fn set_attr<T>(cluster: u16, attr: u16, value: &mut T) {
    let status = ffi::esp_zb_zcl_set_attribute_val(
        ENDPOINT, cluster, SERVER_ROLE, attr, (value as *mut T).cast(), false);
    if status as u32 != ZCL_STATUS_SUCCESS {
        log::warn!("set_attribute_val cluster 0x{cluster:04x} failed: status {}", status as u32);
    }
}

impl Reporter for ZigbeeReporter {
    fn report(&mut self, report: &Report) {
        let (mut co2, mut temp, mut humidity) = (report.co2.0, report.temperature.0, report.humidity.0);
        Self::with_lock(|| unsafe {
            set_attr(CLUSTER_CO2, ATTR_MEASURED_VALUE, &mut co2);
            set_attr(CLUSTER_TEMPERATURE, ATTR_MEASURED_VALUE, &mut temp);
            set_attr(CLUSTER_HUMIDITY, ATTR_MEASURED_VALUE, &mut humidity);
        });
    }

    fn reset_recalibration_switch(&mut self) {
        let mut off = false;
        Self::with_lock(|| unsafe { set_attr(CLUSTER_ON_OFF, ATTR_ON_OFF, &mut off) });
    }
}
