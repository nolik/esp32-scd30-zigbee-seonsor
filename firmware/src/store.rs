//! Self-heal reference in ESP32 NVS. Namespace/key/type are byte-identical to the C++
//! firmware so the existing reference survives the migration (Q2).
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use logic::sensor::StoredReference;

const NAMESPACE: &str = "scd30_cal";
const KEY: &str = "last_co2";

pub struct NvsStoredReference {
    nvs: EspNvs<NvsDefault>,
}

impl NvsStoredReference {
    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        Ok(Self { nvs: EspNvs::new(partition, NAMESPACE, true)? })
    }
}

impl StoredReference for NvsStoredReference {
    fn load(&mut self) -> Option<i32> {
        self.nvs.get_i32(KEY).unwrap_or_else(|e| {
            log::warn!("Reading self-heal reference failed: {e}");
            None
        })
    }

    fn save(&mut self, ppm: i32) -> bool {
        self.nvs.set_i32(KEY, ppm).map_err(|e| log::warn!("NVS write failed: {e}")).is_ok()
    }
}
