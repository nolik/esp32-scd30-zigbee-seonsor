use esp_idf_svc::sys::zb as ffi;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // Referencing a ZBOSS symbol forces the Zigbee libraries into the link (M0 check only).
    let init: unsafe extern "C" fn(*mut ffi::esp_zb_cfg_t) = ffi::esp_zb_init;
    log::info!("M0 skeleton up, esp_zb_init at {:p}", init as *const ());
    Ok(())
}

/// ZBOSS requires the application to define this symbol; Task 9 replaces it with the real one.
#[no_mangle]
pub extern "C" fn esp_zb_app_signal_handler(_signal: *mut ffi::esp_zb_app_signal_t) {}
