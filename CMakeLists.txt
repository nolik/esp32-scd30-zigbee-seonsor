cmake_minimum_required(VERSION 3.16)

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(esp32-scd30-zigbee-sensor)

# Tell ESP-IDF to use your custom partition table
set(PARTITION_TABLE_FILE partitions.csv)
