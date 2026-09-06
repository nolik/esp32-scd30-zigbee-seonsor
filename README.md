0. specify which dev device esp connected
ls /dev/tty*
if it's /dev/ttyACM0
sudo chmod 666 /dev/ttyACM0
1. go nix-shell for esp32-c6:
nix --experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev#esp32c6-idf
2. idf.py build
3. flash esp32 with monitor for troubleshooting
idf.py -p /dev/ttyACM0 flash monitory
4. after all went good just flash
df.py -p /dev/ttyACM0 flash
