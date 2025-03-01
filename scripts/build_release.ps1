cargo build -r
Copy-Item `
    -Path "target/release/godot_vlc.dll" `
    -Destination "demo/addons/godot-vlc/bin/win64"