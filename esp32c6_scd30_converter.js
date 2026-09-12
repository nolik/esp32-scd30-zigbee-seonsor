const fz = require('zigbee-herdsman-converters/converters/fromZigbee');
const tz = require('zigbee-herdsman-converters/converters/toZigbee');
const exposes = require('zigbee-herdsman-converters/lib/exposes');
const reporting = require('zigbee-herdsman-converters/lib/reporting');
const e = exposes.presets;

// Custom CO2 converter: our firmware reports the msCO2 cluster's measuredValue as a
// mol/mol fraction per the ZCL spec (e.g. 0.000425 = 425ppm), so convert to plain ppm.
const fzLocal = {
    co2: {
        cluster: 'msCO2',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg, publish, options, meta) => {
            if (msg.data.hasOwnProperty('measuredValue')) {
                return {co2: Math.round(msg.data.measuredValue * 1000000)};
            }
        },
    },
};

const definition = {
    zigbeeModel: ['ESP32C6-SCD30-Zigbee'],
    model: 'ESP32C6-SCD30-Zigbee',
    vendor: 'Nolik',
    description: 'DIY ESP32-C6 Zigbee CO2/temperature/humidity sensor (SCD30)',
    fromZigbee: [fzLocal.co2, fz.temperature, fz.humidity, fz.on_off],
    toZigbee: [tz.on_off],
    exposes: [
        e.co2(),
        e.temperature(),
        e.humidity(),
        e.switch()
            .withLabel('Trigger Recalibration')
            .withDescription(
                'Flip ON to trigger a Forced Recalibration (FRC) against whatever CO2 concentration the ' +
                'sensor is currently exposed to. Only use this while genuinely exposing the sensor to a ' +
                'known reference (e.g. fresh outdoor air, ~420-430ppm) for at least 2 minutes beforehand. ' +
                'Automatically resets back to OFF once the recalibration completes.'
            ),
    ],
    configure: async (device, coordinatorEndpoint, logger) => {
        const endpoint = device.getEndpoint(1);
        await reporting.bind(endpoint, coordinatorEndpoint, [
            'msCO2', 'msTemperatureMeasurement', 'msRelativeHumidity', 'genOnOff',
        ]);
        await reporting.temperature(endpoint);
        await reporting.humidity(endpoint);
        await reporting.onOff(endpoint);
        await endpoint.configureReporting('msCO2', [{
            attribute: 'measuredValue',
            minimumReportInterval: 0,
            maximumReportInterval: 3600,
            reportableChange: 0.00001, // roughly a 10ppm change
        }]);
    },
};

module.exports = definition;
