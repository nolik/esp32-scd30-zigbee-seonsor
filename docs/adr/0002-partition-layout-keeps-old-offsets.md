# Partition layout keeps the C++ offsets and leaves 1 MB unused

The Rust `factory` app sits at `0x120000` (2 MB). `nvs`, `phy_init`, `zb_storage` and `zb_fct` stay at
exactly the offsets the C++ firmware used. The old 1 MB app region at `0x10000–0x110000` is
deliberately left empty. Moving those data partitions would lose the Zigbee pairing and the Stored
Reference: the defective SCD30 then needs a Reference Recalibration in fresh air before its readings
mean anything. Keeping the offsets also means the `cpp-final` rollback can be flashed with its own
partition table without touching either. Do not "reclaim" the gap unless you accept a re-pair and a
new Reference Recalibration.
