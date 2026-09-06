#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"
#include "esp_zigbee_core.h"
#include "driver/i2c_master.h"
#include "hal/gpio_types.h"
#include "nvs_flash.h"

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

#define SCD30_MEASUREMENT_INTERVAL_SECONDS       15

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
#define SCD30_FRC_REFERENCE_PPM 420

// Re-enable ASC once you're satisfied with the FRC-corrected baseline. Sensirion
// recommends NOT running ASC and manual FRC at the same time long-term - pick one
// ongoing strategy. ASC needs several days of continuous operation, including some
// exposure to fresh/low CO2 air each day, to re-converge properly.
#define SCD30_ENABLE_ASC true

static void scd30_task(void *pvParameters) {
    // Retry setup commands instead of aborting the whole device on a transient I2C
    // glitch (these can happen right as the Zigbee radio keys up and transmits).
    while (scd30_set_measurement_interval(SCD30_MEASUREMENT_INTERVAL_SECONDS) != ESP_OK) {
        ESP_LOGW(TAG, "SCD30 set_measurement_interval failed, retrying...");
        vTaskDelay(pdMS_TO_TICKS(200));
    }
    vTaskDelay(pdMS_TO_TICKS(10));

    while (scd30_set_auto_self_calibration(SCD30_ENABLE_ASC) != ESP_OK) {
        ESP_LOGW(TAG, "SCD30 set_auto_self_calibration failed, retrying...");
        vTaskDelay(pdMS_TO_TICKS(200));
    }
    vTaskDelay(pdMS_TO_TICKS(10));

    while (scd30_trigger_continuous_measurement() != ESP_OK) {
        ESP_LOGW(TAG, "SCD30 trigger_continuous_measurement failed, retrying...");
        vTaskDelay(pdMS_TO_TICKS(200));
    }

    if (SCD30_PERFORM_FRC) {
        ESP_LOGW(TAG, "Waiting 2 minutes for stable readings before forced recalibration...");
        vTaskDelay(pdMS_TO_TICKS(120000));
        esp_err_t frc_err = scd30_force_recalibration(SCD30_FRC_REFERENCE_PPM);
        if (frc_err != ESP_OK) {
            ESP_LOGE(TAG, "Forced recalibration FAILED (err %d) - retry manually next boot", frc_err);
        } else {
            ESP_LOGW(TAG, "Forced recalibration to %d ppm: OK", SCD30_FRC_REFERENCE_PPM);
        }
    }

    while (true) {
        bool ready = false;
        esp_err_t err = scd30_get_data_ready(&ready);
        if (err == ESP_OK && ready) {
            float co2_ppm, temperature_c, humidity_pct;
            if (scd30_read_measurement(&co2_ppm, &temperature_c, &humidity_pct) == ESP_OK) {
                report_sensor_values(co2_ppm, temperature_c, humidity_pct);
            } else {
                ESP_LOGW(TAG, "Failed to read SCD30 measurement");
            }
        } else if (err != ESP_OK) {
            ESP_LOGW(TAG, "Failed to query SCD30 data-ready status");
        }
        vTaskDelay(pdMS_TO_TICKS(SCD30_MEASUREMENT_INTERVAL_SECONDS * 1000));
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
                xTaskCreate(scd30_task, "SCD30_Task", 4096, NULL, 4, NULL);
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

    esp_zb_identify_cluster_cfg_t identify_cfg = {
        .identify_time = ESP_ZB_ZCL_IDENTIFY_IDENTIFY_TIME_DEFAULT_VALUE,
    };

    esp_zb_carbon_dioxide_measurement_cluster_cfg_t co2_cfg = {
        .measured_value = 0.0004f, // ~400 ppm as a starting placeholder (fraction, per ZCL spec)
        .min_measured_value = 0.0f,
        .max_measured_value = 0.01f, // 10000 ppm
    };

    esp_zb_temperature_meas_cluster_cfg_t temp_cfg = {
        .measured_value = (int16_t)0x8000, // 0x8000 = "invalid/not yet measured" per ZCL spec
        .min_value = -4000,                // -40.00 C
        .max_value = 8500,                 // 85.00 C
    };

    esp_zb_humidity_meas_cluster_cfg_t humidity_cfg = {
        .measured_value = (uint16_t)0x8000, // 0x8000 = "invalid/not yet measured" per ZCL spec
        .min_value = 0,                    // 0%
        .max_value = 10000,                 // 100.00%
    };

    esp_zb_cluster_list_t *cluster_list = esp_zb_zcl_cluster_list_create();
    esp_zb_cluster_list_add_basic_cluster(cluster_list, esp_zb_basic_cluster_create(&basic_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_identify_cluster(cluster_list, esp_zb_identify_cluster_create(&identify_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_carbon_dioxide_measurement_cluster(cluster_list, esp_zb_carbon_dioxide_measurement_cluster_create(&co2_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_temperature_meas_cluster(cluster_list, esp_zb_temperature_meas_cluster_create(&temp_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);
    esp_zb_cluster_list_add_humidity_meas_cluster(cluster_list, esp_zb_humidity_meas_cluster_create(&humidity_cfg), ESP_ZB_ZCL_CLUSTER_SERVER_ROLE);

    esp_zb_ep_list_t *ep_list = esp_zb_ep_list_create();
    esp_zb_ep_list_add_ep(ep_list, cluster_list, ep_config);

    esp_zb_device_register(ep_list);

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
