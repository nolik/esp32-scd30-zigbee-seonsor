#!/usr/bin/env bash
# Print the Z2M contract projection of "CO2 sensor" (same shape as docs/z2m-baseline.json,
# minus capture metadata). Usage: scripts/z2m-contract.sh [--reinterview]
set -euo pipefail
if [ "${1:-}" = "--reinterview" ]; then
  # Without a re-interview Z2M keeps showing endpoints/clusters cached from the C++ firmware.
  mosquitto_pub -h localhost -t zigbee2mqtt/bridge/request/device/interview -m '{"id":"CO2 sensor"}'
  sleep 30
fi
mosquitto_sub -h localhost -t zigbee2mqtt/bridge/devices -C 1 -W 20 | jq -S '
  [.[] | select(.model_id == "ESP32C6-SCD30-Zigbee")][0]
  | {friendly_name, ieee_address, type, manufacturer, model_id, power_source, software_build_id,
     date_code, interview_completed, interview_state, supported,
     endpoints: (.endpoints | map_values({
       bindings: [.bindings[] | {cluster, target: .target.type}],
       clusters,
       configured_reportings: [.configured_reportings[] | {cluster, attribute,
         minimum_report_interval, maximum_report_interval, reportable_change}]}))}'
