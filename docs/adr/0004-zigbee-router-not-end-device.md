# The device runs as a Zigbee router, not an end device

As an end device in its usual room, the sensor joined through the TS011F smart plug (a Tuya router).
The plug removed it from its child table about 64 minutes after every join. That 64 minutes is the
end-device aging timeout the firmware asks for. The firmware never noticed it had been dropped, so
it stayed silent until the next power cycle. This was measured on 2026-10-11: joined 19:23:35 as a
child of the plug at LQI 254, last report 20:25:40, gone from the plug's table at 20:30. The C++
firmware failed the same way (10-09). Next to the coordinator, the parent kept it and runs lasted
over 90 minutes.

The device is always on mains (USB) power, so it now runs as a router. A router does not age out
of its parent, and it adds a router in that room. The accepted costs:
- One re-pair. Switching role means clearing `zb_storage` once; `nvs`, and with it the Stored
  Reference, stays.
- `docs/z2m-baseline.json` now records `Router`.
- When the device is unplugged, end devices that joined through it have to find another parent.

Rejected alternative: stay an end device and rejoin on a leave/parent-loss signal. It would still
leave a gap every hour, and an end device that keeps its receiver on never polls its parent, so it
may never detect the loss at all.

Rollback to `cpp-final` (an end device) needs the same one-time `zb_storage` clearing.
