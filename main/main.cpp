#include <string.h>
#include <math.h>
#include <inttypes.h>
#include <functional>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"
#include "esp_zigbee_core.h"
#include "driver/i2c_master.h"
#include "hal/gpio_types.h"
#include "nvs_flash.h"
#include "nvs.h"

#define I2C_MASTER_SDA_IO           GPIO_NUM_21
#define I2C_MASTER_SCL_IO           GPIO_NUM_7

#define ESP_ZB_PRIMARY_CHANNEL_MASK ESP_ZB_TRANSCEIVER_ALL_CHANNELS_MASK

#define ESP_ZB_ZED_CONFIG()                                         \
    {                                                               \
        .esp_zb_role = ESP_ZB_DEVICE_TYPE_ED,                       \
        .install_code_policy = false,                               \
        .nwk_cfg = {                                                \
            .zed_cfg = {                                            \
                .ed_timeout = ESP_ZB_ED_AGING_TIMEOUT_64MIN,        \
                .keep_alive = 3000,                                 \
            },                                                      \
        },                                                          \
    }

#define ESP_ZB_DEFAULT_RADIO_CONFIG()                               \
    {                                                               \
        .radio_mode = ZB_RADIO_MODE_NATIVE,                         \
    }

#define ESP_ZB_DEFAULT_HOST_CONFIG()                                \
    {                                                               \
        .host_connection_mode = ZB_HOST_CONNECTION_MODE_NONE,       \
    }

// SCD30 I2C commands (see Sensirion SCD30 Interface Description)
#define SCD30_CMD_TRIGGER_CONTINUOUS_MEASUREMENT 0x0010
#define SCD30_CMD_SET_MEASUREMENT_INTERVAL        0x4600
#define SCD30_CMD_GET_DATA_READY                 0x0202
#define SCD30_CMD_READ_MEASUREMENT                0x0300
#define SCD30_CMD_AUTOMATIC_SELF_CALIBRATION      0x5306
#define SCD30_CMD_FORCED_RECALIBRATION            0x5204
#define SCD30_CMD_GET_FIRMWARE_VERSION            0xD100

#define SCD30_MEASUREMENT_INTERVAL_SECONDS       15

// Self-heal: the SCD30's own calibration doesn't survive power cycles on this unit,
// so we keep our own "last known good CO2" on the ESP32's flash (NVS) and re-assert
// it via FRC on every boot. This is a *self-heal*, not a real recalibration - it only
// re-establishes trust in whatever the sensor last reported, it can't detect or correct
// drift on its own. Pair with periodic real FRC (SCD30_PERFORM_FRC) against a genuine
// reference to keep long-term accuracy in check.
#define SCD30_CAL_NVS_NAMESPACE "scd30_cal"
#define SCD30_CAL_NVS_KEY "last_co2"
#define SCD30_SELF_HEAL_WAIT_MS (120 * 1000) // Sensirion requires >=2 min stable readings before FRC
#define SCD30_SAVE_INTERVAL_READINGS 40      // ~10 min at the 15s interval above

// FRC command's documented valid range - clamp any reference (manual or self-healed)
// to this before sending, since indoor CO2 can exceed it in poorly-ventilated rooms.
#define SCD30_FRC_MIN_PPM 400
#define SCD30_FRC_MAX_PPM 2000

// Guard against ever saving/reasserting an obviously-corrupted value (e.g. the ~1ppm
// the sensor reverts to after a failed power cycle) as a trusted self-heal reference.
#define SCD30_PLAUSIBLE_MIN_PPM 300
#define SCD30_PLAUSIBLE_MAX_PPM 10000

#define ZIGBEE_ENDPOINT 1

static const char *TAG = "ZIGBEE_SENSOR";

i2c_master_bus_handle_t bus_handle;
i2c_master_dev_handle_t scd_handle;

void scd30_init() {
    i2c_master_bus_config_t bus_config = {};
    bus_config.clk_source = I2C_CLK_SRC_DEFAULT;
    bus_config.i2c_port = -1;
    bus_config.scl_io_num = I2C_MASTER_SCL_IO;
    bus_config.sda_io_num = I2C_MASTER_SDA_IO;
    bus_config.glitch_ignore_cnt = 7;
    bus_config.flags.enable_internal_pullup = true;

    ESP_ERROR_CHECK(i2c_new_master_bus(&bus_config, &bus_handle));

    i2c_device_config_t dev_config = {};
    dev_config.dev_addr_length = I2C_ADDR_BIT_LEN_7;
    dev_config.device_address = 0x61;
    dev_config.scl_speed_hz = 100000;

    ESP_ERROR_CHECK(i2c_master_bus_add_device(bus_handle, &dev_config, &scd_handle));
    ESP_LOGI(TAG, "I2C initialized successfully");
}

// Fully tears down and recreates the I2C bus/device. A plain retry sometimes isn't
// enough if the bus has genuinely locked up (e.g. from concurrent access by more
// than one task) - this forces a clean electrical reset of the peripheral.
static esp_err_t scd30_i2c_bus_recover() {
    ESP_LOGW(TAG, "Attempting full I2C bus/device reset...");
    if (scd_handle) {
        i2c_master_bus_rm_device(scd_handle);
        scd_handle = NULL;
    }
    if (bus_handle) {
        i2c_del_master_bus(bus_handle);
        bus_handle = NULL;
    }
    vTaskDelay(pdMS_TO_TICKS(100));
    scd30_init();
    return (scd_handle != NULL) ? ESP_OK : ESP_FAIL;
}

// Retries `cmd` until it succeeds, with backoff and a periodic full bus reset if it
// keeps failing - instead of either aborting the device or spinning as fast as
// possible against a bus that may be genuinely stuck.
static void scd30_retry_until_ok(const std::function<esp_err_t()> &cmd, const char *cmd_name) {
    int fail_count = 0;
    while (cmd() != ESP_OK) {
        fail_count++;
        ESP_LOGW(TAG, "%s failed, retrying... (attempt %d)", cmd_name, fail_count);
        if (fail_count % 5 == 0) {
            scd30_i2c_bus_recover();
        }
        vTaskDelay(pdMS_TO_TICKS(fail_count < 10 ? 200 : 2000));
    }
}

// ---- SCD30 driver ----
// CRC-8: polynomial 0x31 (x^8 + x^5 + x^4 + 1), init 0xFF, no reflection, no final XOR.
static uint8_t scd30_crc8(const uint8_t *data, size_t len) {
    uint8_t crc = 0xFF;
    for (size_t i = 0; i < len; i++) {
        crc ^= data[i];
        for (int b = 0; b < 8; b++) {
            crc = (crc & 0x80) ? (uint8_t)((crc << 1) ^ 0x31) : (uint8_t)(crc << 1);
        }
    }
    return crc;
}

static esp_err_t scd30_write_command(uint16_t cmd) {
    uint8_t buf[2] = { (uint8_t)(cmd >> 8), (uint8_t)(cmd & 0xFF) };
    return i2c_master_transmit(scd_handle, buf, sizeof(buf), 1000);
}

static esp_err_t scd30_write_command_arg(uint16_t cmd, uint16_t arg) {
    uint8_t arg_bytes[2] = { (uint8_t)(arg >> 8), (uint8_t)(arg & 0xFF) };
    uint8_t buf[5] = {
        (uint8_t)(cmd >> 8),
        (uint8_t)(cmd & 0xFF),
        arg_bytes[0],
        arg_bytes[1],
        scd30_crc8(arg_bytes, 2),
    };
    return i2c_master_transmit(scd_handle, buf, sizeof(buf), 1000);
}

static esp_err_t scd30_trigger_continuous_measurement() {
    // Argument 0x0000 = use default ambient pressure compensation (1013.25 mBar)
    return scd30_write_command_arg(SCD30_CMD_TRIGGER_CONTINUOUS_MEASUREMENT, 0x0000);
}

static esp_err_t scd30_set_measurement_interval(uint16_t seconds) {
    // Stored in the SCD30's non-volatile memory; persists across power cycles.
    return scd30_write_command_arg(SCD30_CMD_SET_MEASUREMENT_INTERVAL, seconds);
}

static esp_err_t scd30_set_auto_self_calibration(bool enable) {
    return scd30_write_command_arg(SCD30_CMD_AUTOMATIC_SELF_CALIBRATION, enable ? 1 : 0);
}

// Forces the sensor to treat its CURRENT reading as `reference_ppm`. Only call this after
// the sensor has been running continuous measurement for >= 2 minutes exposed to a known,
// stable reference concentration (e.g. fresh outdoor air ~420ppm). Overwrites the baseline
// stored in the SCD30's own non-volatile memory. Valid range: 400-2000 ppm.
static esp_err_t scd30_force_recalibration(uint16_t reference_ppm) {
    return scd30_write_command_arg(SCD30_CMD_FORCED_RECALIBRATION, reference_ppm);
}

static uint16_t scd30_clamp_frc_reference(float ppm) {
    if (ppm < SCD30_FRC_MIN_PPM) return SCD30_FRC_MIN_PPM;
    if (ppm > SCD30_FRC_MAX_PPM) return SCD30_FRC_MAX_PPM;
    return (uint16_t)lroundf(ppm);
}

static bool scd30_ppm_is_plausible(float ppm) {
    return ppm >= SCD30_PLAUSIBLE_MIN_PPM && ppm <= SCD30_PLAUSIBLE_MAX_PPM;
}

// ---- Self-heal storage (ESP32 NVS - separate from, and more reliable than, the
// SCD30's own non-volatile calibration memory) ----
static esp_err_t scd30_cal_save_last_co2(int32_t ppm) {
    nvs_handle_t handle;
    esp_err_t err = nvs_open(SCD30_CAL_NVS_NAMESPACE, NVS_READWRITE, &handle);
    if (err != ESP_OK) return err;
    err = nvs_set_i32(handle, SCD30_CAL_NVS_KEY, ppm);
    if (err == ESP_OK) err = nvs_commit(handle);
    nvs_close(handle);
    return err;
}

// Returns ESP_ERR_NVS_NOT_FOUND (or similar) if nothing has been saved yet, e.g. on
// a brand-new device or before the first successful calibration of this firmware.
static esp_err_t scd30_cal_load_last_co2(int32_t *ppm) {
    nvs_handle_t handle;
    esp_err_t err = nvs_open(SCD30_CAL_NVS_NAMESPACE, NVS_READONLY, &handle);
    if (err != ESP_OK) return err;
    err = nvs_get_i32(handle, SCD30_CAL_NVS_KEY, ppm);
    nvs_close(handle);
    return err;
}

static esp_err_t scd30_read_firmware_version(uint8_t *major, uint8_t *minor) {
    esp_err_t err = scd30_write_command(SCD30_CMD_GET_FIRMWARE_VERSION);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(3));

    uint8_t buf[3];
    err = i2c_master_receive(scd_handle, buf, sizeof(buf), 1000);
    if (err != ESP_OK) return err;

    if (scd30_crc8(buf, 2) != buf[2]) {
        ESP_LOGW(TAG, "SCD30 firmware version CRC mismatch");
        return ESP_ERR_INVALID_CRC;
    }
    *major = buf[0];
    *minor = buf[1];
    return ESP_OK;
}

static esp_err_t scd30_get_data_ready(bool *ready) {
    esp_err_t err = scd30_write_command(SCD30_CMD_GET_DATA_READY);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(3));

    uint8_t buf[3];
    err = i2c_master_receive(scd_handle, buf, sizeof(buf), 1000);
    if (err != ESP_OK) return err;

    if (scd30_crc8(buf, 2) != buf[2]) {
        ESP_LOGW(TAG, "SCD30 data-ready CRC mismatch");
        return ESP_ERR_INVALID_CRC;
    }
    *ready = (buf[0] == 0x00 && buf[1] == 0x01);
    return ESP_OK;
}

// Reconstructs one IEEE-754 float from 2 CRC-checked 16-bit words (6 raw bytes: MSB,LSB,CRC x2)
static float scd30_words_to_float(const uint8_t *w) {
    uint32_t raw = ((uint32_t)w[0] << 24) | ((uint32_t)w[1] << 16) |
                   ((uint32_t)w[3] << 8)  | ((uint32_t)w[4]);
    float val;
    memcpy(&val, &raw, sizeof(val));
    return val;
}

static esp_err_t scd30_read_measurement(float *co2_ppm, float *temperature_c, float *humidity_pct) {
    esp_err_t err = scd30_write_command(SCD30_CMD_READ_MEASUREMENT);
    if (err != ESP_OK) return err;
    vTaskDelay(pdMS_TO_TICKS(3));

    uint8_t buf[18];
    err = i2c_master_receive(scd_handle, buf, sizeof(buf), 1000);
    if (err != ESP_OK) return err;

    for (int i = 0; i < 6; i++) {
        const uint8_t *word = &buf[i * 3];
        if (scd30_crc8(word, 2) != word[2]) {
            ESP_LOGW(TAG, "SCD30 measurement CRC mismatch at word %d", i);
            return ESP_ERR_INVALID_CRC;
        }
    }

    *co2_ppm = scd30_words_to_float(&buf[0]);
    *temperature_c = scd30_words_to_float(&buf[6]);
    *humidity_pct = scd30_words_to_float(&buf[12]);
    return ESP_OK;
}

// ---- Zigbee reporting ----
static void report_sensor_values(float co2_ppm, float temperature_c, float humidity_pct) {
    // Carbon Dioxide Measurement cluster: MeasuredValue is a float, expressed as
    // mol/mol fraction (e.g. 400 ppm == 0.0004), per ZCL spec.
    float co2_fraction = co2_ppm / 1000000.0f;

    // Temperature Measurement cluster: MeasuredValue is int16, hundredths of a degree C.
    int16_t temp_hundredths = (int16_t)(temperature_c * 100.0f);

    // Relative Humidity Measurement cluster: MeasuredValue is uint16, hundredths of a percent.
    uint16_t humidity_hundredths = (uint16_t)(humidity_pct * 100.0f);

    esp_zb_lock_acquire(portMAX_DELAY);
    esp_zb_zcl_set_attribute_val(ZIGBEE_ENDPOINT, ESP_ZB_ZCL_CLUSTER_ID_CARBON_DIOXIDE_MEASUREMENT,
                                  ESP_ZB_ZCL_CLUSTER_SERVER_ROLE,
                                  ESP_ZB_ZCL_ATTR_CARBON_DIOXIDE_MEASUREMENT_MEASURED_VALUE_ID,
                                  &co2_fraction, false);
    esp_zb_zcl_set_attribute_val(ZIGBEE_ENDPOINT, ESP_ZB_ZCL_CLUSTER_ID_TEMP_MEASUREMENT,
                                  ESP_ZB_ZCL_CLUSTER_SERVER_ROLE,
                                  ESP_ZB_ZCL_ATTR_TEMP_MEASUREMENT_VALUE_ID,
                                  &temp_hundredths, false);
    esp_zb_zcl_set_attribute_val(ZIGBEE_ENDPOINT, ESP_ZB_ZCL_CLUSTER_ID_REL_HUMIDITY_MEASUREMENT,
                                  ESP_ZB_ZCL_CLUSTER_SERVER_ROLE,
                                  ESP_ZB_ZCL_ATTR_REL_HUMIDITY_MEASUREMENT_VALUE_ID,
                                  &humidity_hundredths, false);
    esp_zb_lock_release();

    ESP_LOGI(TAG, "CO2: %.1f ppm, Temp: %.2f C, Humidity: %.2f %%", co2_ppm, temperature_c, humidity_pct);
}

// Set to true, rebuild and reflash ONCE while the sensor sits in fresh outdoor air
// (or another known-reference environment) for the recalibration to take effect,
// then set back to false and reflash again so it doesn't refire every boot.
#define SCD30_PERFORM_FRC false
#define SCD30_FRC_REFERENCE_PPM 425

// ASC and FRC should never both be active at once (Sensirion: whichever command
// fires most recently "wins" and overrides the other). So: disable ASC only while
// a manual FRC run is in progress this boot; leave it enabled for all normal/
// self-heal boots, where it runs as a background safety net against long-term
// drift alongside the self-heal mechanism.
#define SCD30_ENABLE_ASC (!SCD30_PERFORM_FRC)

static void scd30_task(void *pvParameters) {
    uint8_t fw_major = 0, fw_minor = 0;
    if (scd30_read_firmware_version(&fw_major, &fw_minor) == ESP_OK) {
        ESP_LOGI(TAG, "SCD30 firmware version: %d.%d", fw_major, fw_minor);
    } else {
        ESP_LOGW(TAG, "Failed to read SCD30 firmware version");
    }

    // Retry setup commands with backoff and bus recovery instead of aborting the
    // whole device or spinning tight against a potentially stuck bus.
    scd30_retry_until_ok([]() { return scd30_set_measurement_interval(SCD30_MEASUREMENT_INTERVAL_SECONDS); },
                         "SCD30 set_measurement_interval");
    vTaskDelay(pdMS_TO_TICKS(10));

    scd30_retry_until_ok([]() { return scd30_set_auto_self_calibration(SCD30_ENABLE_ASC); },
                         "SCD30 set_auto_self_calibration");
    vTaskDelay(pdMS_TO_TICKS(10));

    scd30_retry_until_ok([]() { return scd30_trigger_continuous_measurement(); },
                         "SCD30 trigger_continuous_measurement");

    bool calibration_trusted = false;

    if (SCD30_PERFORM_FRC) {
        // Manual path: a real recalibration against a genuine known reference
        // (e.g. fresh outdoor air). This is the only way to correct actual drift -
        // self-heal below can only ever re-assert what we already trusted.
        ESP_LOGW(TAG, "Waiting 2 minutes for stable readings before forced recalibration...");
        vTaskDelay(pdMS_TO_TICKS(SCD30_SELF_HEAL_WAIT_MS));
        uint16_t reference = scd30_clamp_frc_reference(SCD30_FRC_REFERENCE_PPM);
        esp_err_t frc_err = scd30_force_recalibration(reference);
        if (frc_err != ESP_OK) {
            ESP_LOGE(TAG, "Forced recalibration FAILED (err %d) - retry manually next boot", frc_err);
        } else {
            ESP_LOGW(TAG, "Forced recalibration to %d ppm: OK", reference);
            calibration_trusted = true;
            if (scd30_cal_save_last_co2(reference) == ESP_OK) {
                ESP_LOGI(TAG, "Saved %d ppm as new self-heal reference", reference);
            } else {
                ESP_LOGW(TAG, "Failed to save self-heal reference to NVS");
            }
        }
    } else {
        // Normal boot: self-heal using whatever we last trusted, since this specific
        // unit's own calibration doesn't survive a power cycle. This only re-asserts
        // old trust, it can't detect or correct real-world drift on its own.
        int32_t last_known_ppm = 0;
        esp_err_t load_err = scd30_cal_load_last_co2(&last_known_ppm);
        if (load_err == ESP_OK && scd30_ppm_is_plausible((float)last_known_ppm)) {
            ESP_LOGW(TAG, "Self-heal: waiting 2 minutes before reasserting last known CO2 (%" PRId32 " ppm)...", last_known_ppm);
            vTaskDelay(pdMS_TO_TICKS(SCD30_SELF_HEAL_WAIT_MS));
            uint16_t reference = scd30_clamp_frc_reference((float)last_known_ppm);
            esp_err_t frc_err = scd30_force_recalibration(reference);
            if (frc_err == ESP_OK) {
                ESP_LOGI(TAG, "Self-heal recalibration to %d ppm: OK", reference);
                calibration_trusted = true;
            } else {
                ESP_LOGW(TAG, "Self-heal recalibration FAILED (err %d)", frc_err);
            }
        } else {
            ESP_LOGI(TAG, "No usable self-heal reference saved yet - skipping auto-recalibration this boot");
        }
    }

    int trusted_reading_count = 0;
    int consecutive_failures = 0;

    while (true) {
        bool ready = false;
        esp_err_t err = scd30_get_data_ready(&ready);
        if (err == ESP_OK && ready) {
            float co2_ppm, temperature_c, humidity_pct;
            if (scd30_read_measurement(&co2_ppm, &temperature_c, &humidity_pct) == ESP_OK) {
                consecutive_failures = 0;
                report_sensor_values(co2_ppm, temperature_c, humidity_pct);

                // Only persist readings from a session we know started from a trusted
                // calibration - otherwise we'd overwrite a good reference with garbage
                // the moment this sensor reverts after its next power cycle.
                if (calibration_trusted && scd30_ppm_is_plausible(co2_ppm)) {
                    trusted_reading_count++;
                    if (trusted_reading_count % SCD30_SAVE_INTERVAL_READINGS == 0) {
                        int32_t rounded = (int32_t)lroundf(co2_ppm);
                        if (scd30_cal_save_last_co2(rounded) == ESP_OK) {
                            ESP_LOGI(TAG, "Self-heal reference updated to %" PRId32 " ppm", rounded);
                        } else {
                            ESP_LOGW(TAG, "Failed to update self-heal reference in NVS");
                        }
                    }
                }
            } else {
                consecutive_failures++;
                ESP_LOGW(TAG, "Failed to read SCD30 measurement (%d consecutive)", consecutive_failures);
            }
        } else if (err != ESP_OK) {
            consecutive_failures++;
            ESP_LOGW(TAG, "Failed to query SCD30 data-ready status (%d consecutive)", consecutive_failures);
        }

        if (consecutive_failures >= 5) {
            scd30_i2c_bus_recover();
            consecutive_failures = 0;
        }

        vTaskDelay(pdMS_TO_TICKS(SCD30_MEASUREMENT_INTERVAL_SECONDS * 1000));
    }
}

// ---- Zigbee-controlled manual FRC trigger ----
// Exposes an On/Off cluster acting as a momentary "trigger recalibration" switch,
// so a real FRC (against whatever reference concentration you're currently exposing
// the sensor to) can be kicked off from Home Assistant/Zigbee2MQTT without reflashing.
static volatile bool frc_trigger_in_progress = false;

static void frc_manual_trigger_task(void *pvParameters) {
    ESP_LOGW(TAG, "Zigbee-triggered FRC: disabling ASC and waiting 2 minutes for stable readings...");
    scd30_set_auto_self_calibration(false); // avoid running ASC and FRC at the same time
    vTaskDelay(pdMS_TO_TICKS(SCD30_SELF_HEAL_WAIT_MS));

    uint16_t reference = scd30_clamp_frc_reference(SCD30_FRC_REFERENCE_PPM);
    esp_err_t frc_err = scd30_force_recalibration(reference);
    if (frc_err == ESP_OK) {
        ESP_LOGW(TAG, "Zigbee-triggered FRC to %d ppm: OK", reference);
        if (scd30_cal_save_last_co2(reference) == ESP_OK) {
            ESP_LOGI(TAG, "Saved %d ppm as new self-heal reference", reference);
        } else {
            ESP_LOGW(TAG, "Failed to save self-heal reference to NVS");
        }
    } else {
        ESP_LOGE(TAG, "Zigbee-triggered FRC FAILED (err %d)", frc_err);
    }

    scd30_set_auto_self_calibration(SCD30_ENABLE_ASC); // restore normal ASC state

    // Flip the switch back off so it behaves like a momentary trigger rather than a
    // persistent toggle - Home Assistant/Z2M will see it return to "off" on its own.
    bool off = false;
    esp_zb_lock_acquire(portMAX_DELAY);
    esp_zb_zcl_set_attribute_val(ZIGBEE_ENDPOINT, ESP_ZB_ZCL_CLUSTER_ID_ON_OFF,
                                  ESP_ZB_ZCL_CLUSTER_SERVER_ROLE,
                                  ESP_ZB_ZCL_ATTR_ON_OFF_ON_OFF_ID,
                                  &off, false);
    esp_zb_lock_release();

    frc_trigger_in_progress = false;
    vTaskDelete(NULL);
}

static esp_err_t zb_attribute_handler(const esp_zb_zcl_set_attr_value_message_t *message) {
    if (!message) return ESP_FAIL;
    if (message->info.status != ESP_ZB_ZCL_STATUS_SUCCESS) return ESP_ERR_INVALID_ARG;

    if (message->info.dst_endpoint == ZIGBEE_ENDPOINT &&
        message->info.cluster == ESP_ZB_ZCL_CLUSTER_ID_ON_OFF &&
        message->attribute.id == ESP_ZB_ZCL_ATTR_ON_OFF_ON_OFF_ID &&
        message->attribute.data.type == ESP_ZB_ZCL_ATTR_TYPE_BOOL) {
        bool requested_on = message->attribute.data.value ? *(bool *)message->attribute.data.value : false;
        if (requested_on) {
            if (!frc_trigger_in_progress) {
                ESP_LOGI(TAG, "Recalibration triggered via Zigbee");
                frc_trigger_in_progress = true;
                xTaskCreate(frc_manual_trigger_task, "FRC_Trigger_Task", 4096, NULL, 4, NULL);
            } else {
                ESP_LOGW(TAG, "Recalibration already in progress - ignoring trigger");
            }
        }
    }
    return ESP_OK;
}

static esp_err_t zb_action_handler(esp_zb_core_action_callback_id_t callback_id, const void *message) {
    esp_err_t ret = ESP_OK;
    switch (callback_id) {
        case ESP_ZB_CORE_SET_ATTR_VALUE_CB_ID:
            ret = zb_attribute_handler((const esp_zb_zcl_set_attr_value_message_t *)message);
            break;
        default:
            ESP_LOGW(TAG, "Receive Zigbee action(0x%x) callback", callback_id);
            break;
    }
    return ret;
}

// One-time diagnostic, per Espressif's own attribute-reporting debugging checklist:
// verify each attribute actually carries the REPORTING access flag. If one is
// missing it, the device will never honor a Configure Reporting request for it,
// no matter how many times Z2M retries - this would explain a deterministic,
// repeatable failure rather than random per-boot bad luck.
static void log_attribute_reporting_flags() {
    struct { uint16_t cluster_id; uint16_t attr_id; const char *name; } checks[] = {
        {ESP_ZB_ZCL_CLUSTER_ID_CARBON_DIOXIDE_MEASUREMENT, ESP_ZB_ZCL_ATTR_CARBON_DIOXIDE_MEASUREMENT_MEASURED_VALUE_ID, "CO2"},
        {ESP_ZB_ZCL_CLUSTER_ID_TEMP_MEASUREMENT, ESP_ZB_ZCL_ATTR_TEMP_MEASUREMENT_VALUE_ID, "Temperature"},
        {ESP_ZB_ZCL_CLUSTER_ID_REL_HUMIDITY_MEASUREMENT, ESP_ZB_ZCL_ATTR_REL_HUMIDITY_MEASUREMENT_VALUE_ID, "Humidity"},
        {ESP_ZB_ZCL_CLUSTER_ID_ON_OFF, ESP_ZB_ZCL_ATTR_ON_OFF_ON_OFF_ID, "OnOff"},
    };

    for (auto &c : checks) {
        esp_zb_zcl_attr_t *attr = esp_zb_zcl_get_attribute(ZIGBEE_ENDPOINT, c.cluster_id,
                                                            ESP_ZB_ZCL_CLUSTER_SERVER_ROLE, c.attr_id);
        if (!attr) {
            ESP_LOGE(TAG, "Attr check [%s]: not found!", c.name);
            continue;
        }
        bool reportable = (attr->access & ESP_ZB_ZCL_ATTR_ACCESS_REPORTING) != 0;
        ESP_LOGW(TAG, "Attr check [%s]: access=0x%02x, REPORTING flag %s",
                 c.name, attr->access, reportable ? "SET" : "MISSING");
    }
}

void esp_zb_app_signal_handler(esp_zb_app_signal_t *signal_struct) {
    uint32_t *p_sg_p = signal_struct->p_app_signal;
    esp_err_t err_status = signal_struct->esp_err_status;
    esp_zb_app_signal_type_t sig_type = (esp_zb_app_signal_type_t)*p_sg_p;

    switch (sig_type) {
        case ESP_ZB_ZDO_SIGNAL_SKIP_STARTUP:
            ESP_LOGI(TAG, "Zigbee stack initialized");
            esp_zb_bdb_start_top_level_commissioning(ESP_ZB_BDB_MODE_INITIALIZATION);
            break;
        case ESP_ZB_BDB_SIGNAL_DEVICE_FIRST_START:
        case ESP_ZB_BDB_SIGNAL_DEVICE_REBOOT:
            ESP_LOGI(TAG, "Start network steering");
            esp_zb_bdb_start_top_level_commissioning(ESP_ZB_BDB_MODE_NETWORK_STEERING);
            break;
        case ESP_ZB_BDB_SIGNAL_STEERING:
            if (err_status == ESP_OK) {
                ESP_LOGI(TAG, "Successfully joined Zigbee network!");
                // This signal fires on every rejoin, not just the first boot - guard
                // against spawning duplicate SCD30_Task instances that would fight
                // over the same I2C bus and lock it up.
                static bool scd30_task_started = false;
                if (!scd30_task_started) {
                    scd30_task_started = true;
                    xTaskCreate(scd30_task, "SCD30_Task", 4096, NULL, 4, NULL);
                } else {
                    ESP_LOGI(TAG, "SCD30_Task already running - not starting another");
                }
            } else {
                ESP_LOGW(TAG, "Failed to join network (status: %d), retrying...", err_status);
                esp_zb_bdb_start_top_level_commissioning(ESP_ZB_BDB_MODE_NETWORK_STEERING);
            }
            break;
        default:
            ESP_LOGI(TAG, "ZDO signal: %d, status: %d", sig_type, err_status);
            break;
    }
}

void zigbee_task(void *pvParameters) {
    esp_zb_cfg_t zb_nwk_cfg = ESP_ZB_ZED_CONFIG();
    esp_zb_init(&zb_nwk_cfg);

    esp_zb_endpoint_config_t ep_config = {
        .endpoint = ZIGBEE_ENDPOINT,
        .app_profile_id = ESP_ZB_AF_HA_PROFILE_ID,
        .app_device_id = ESP_ZB_HA_TEMPERATURE_SENSOR_DEVICE_ID,
        .app_device_version = 0
    };

    esp_zb_basic_cluster_cfg_t basic_cfg = {
        .zcl_version = ESP_ZB_ZCL_BASIC_ZCL_VERSION_DEFAULT_VALUE,
        .power_source = ESP_ZB_ZCL_BASIC_POWER_SOURCE_DEFAULT_VALUE,
    };

    // Give this device an identifiable manufacturer/model so a Zigbee2MQTT external
    // converter can reliably match it (otherwise it shows as blank/generic strings).
    // ZCL strings are Pascal-style: first byte = length, followed by raw characters
    // (no null terminator). Built explicitly here rather than via DEFINE_PSTRING,
    // which isn't pulled in by esp_zigbee_core.h in this SDK version.
    static uint8_t manufacturer_pstring[] = {5, 'N', 'o', 'l', 'i', 'k'};
    static uint8_t model_pstring[] = {20, 'E', 'S', 'P', '3', '2', 'C', '6', '-', 'S', 'C', 'D',
                                       '3', '0', '-', 'Z', 'i', 'g', 'b', 'e', 'e'};

    esp_zb_attribute_list_t *basic_cluster = esp_zb_basic_cluster_create(&basic_cfg);
    esp_zb_basic_cluster_add_attr(basic_cluster, ESP_ZB_ZCL_ATTR_BASIC_MANUFACTURER_NAME_ID, manufacturer_pstring);
    esp_zb_basic_cluster_add_attr(basic_cluster, ESP_ZB_ZCL_ATTR_BASIC_MODEL_IDENTIFIER_ID, model_pstring);

    esp_zb_identify_cluster_cfg_t identify_cfg = {
        .identify_time = ESP_ZB_ZCL_IDENTIFY_IDENTIFY_TIME_DEFAULT_VALUE,
    };

    esp_zb_carbon_dioxide_measurement_cluster_cfg_t co2_cfg = {
        .measured_value = 0.0004f, // ~400 ppm as a starting placeholder (fraction, per ZCL spec)
        .min_measured_value = 0.0f,
        .max_measured_value = 0.01f, // 10000 ppm
    };

    esp_zb_temperature_meas_cluster_cfg_t temp_cfg = {
        // Was (int16_t)0x8000 (-32768, the ZCL "invalid" sentinel) - but that sits at
        // the extreme edge of int16 range. The jump from -32768 to a real reading
        // (e.g. 2073 = 20.73C) computes as a delta of ~34841, which itself overflows
        // signed 16-bit arithmetic. If the reporting engine's "has this changed
        // enough" check uses 16-bit signed math, that overflow can make it wrongly
        // conclude "not changed enough" - silently preventing any report ever again
        // after the first one. Using 0 avoids the overflow entirely and stays safely
        // within our declared min/max (-4000 to 8500) below, unlike -32768 which was
        // actually outside that declared range.
        .measured_value = 0, // 0.00 C placeholder until the first real reading lands
        .min_value = -4000,                // -40.00 C
        .max_value = 8500,                 // 85.00 C
    };

    esp_zb_humidity_meas_cluster_cfg_t humidity_cfg = {
        .measured_value = (uint16_t)0x8000, // 0x8000 = "invalid/not yet measured" per ZCL spec
        .min_value = 0,                    // 0%
        .max_value = 10000,                 // 100.00%
    };

    // "Trigger Recalibration" momentary switch - flipping this On (from Home Assistant/
    // Zigbee2MQTT) kicks off a real FRC without needing to reflash. See frc_manual_trigger_task.
    esp_zb_on_off_cluster_cfg_t on_off_cfg = {
        .on_off = 0,
    };

    esp_zb_cluster_list_t *cluster_list = esp_zb_zcl_cluster_list_create();
    esp_zb_cluster_list_add_basic_cluster(cluster_list, basic_cluster, ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_identify_cluster(cluster_list, esp_zb_identify_cluster_create(&identify_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_carbon_dioxide_measurement_cluster(cluster_list, esp_zb_carbon_dioxide_measurement_cluster_create(&co2_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_temperature_meas_cluster(cluster_list, esp_zb_temperature_meas_cluster_create(&temp_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_humidity_meas_cluster(cluster_list, esp_zb_humidity_meas_cluster_create(&humidity_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_on_off_cluster(cluster_list, esp_zb_on_off_cluster_create(&on_off_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);

    esp_zb_ep_list_t *ep_list = esp_zb_ep_list_create();
    esp_zb_ep_list_add_ep(ep_list, cluster_list, ep_config);

    esp_zb_device_register(ep_list);
    esp_zb_core_action_handler_register(zb_action_handler);
    log_attribute_reporting_flags();

    esp_zb_set_primary_network_channel_set(ESP_ZB_PRIMARY_CHANNEL_MASK);

    ESP_LOGI(TAG, "Starting Zigbee Stack...");
    ESP_ERROR_CHECK(esp_zb_start(false));

    esp_zb_stack_main_loop();

    vTaskDelete(NULL);
}

extern "C" void app_main() {
    scd30_init();

    esp_zb_platform_config_t config = {
        .radio_config = ESP_ZB_DEFAULT_RADIO_CONFIG(),
        .host_config = ESP_ZB_DEFAULT_HOST_CONFIG(),
    };
    ESP_ERROR_CHECK(nvs_flash_init());
    ESP_ERROR_CHECK(esp_zb_platform_config(&config));

    xTaskCreate(zigbee_task, "Zigbee_Task", 8192, NULL, 5, NULL);
}
