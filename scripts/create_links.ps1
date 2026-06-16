$binDir = "gdextension_template/bin"

if (Test-Path $binDir) {
    Remove-Item -Recurse -Force $binDir
}

New-Item -Path $binDir -ItemType Directory -Force | Out-Null

# Windows links
New-Item -Path "$binDir/win-x64" -ItemType Directory -Force | Out-Null

New-Item -Path "$binDir/win-x64/godot_vlc.dll" -Value "../../../target/release/godot_vlc.dll" -ItemType SymbolicLink -Force
New-Item -Path "$binDir/win-x64/godot_vlc_debug.dll" -Value "../../../target/debug/godot_vlc.dll" -ItemType SymbolicLink -Force
New-Item -Path "$binDir/win-x64/godot_vlc_debug.pdb" -Value "../../../target/debug/godot_vlc.pdb" -ItemType SymbolicLink -Force

New-Item -Path "$binDir/win-x64/libvlc.dll" -Value "../../../thirdparty/vlc/win-x64/lib/libvlc.dll" -ItemType SymbolicLink -Force
New-Item -Path "$binDir/win-x64/libvlccore.dll" -Value "../../../thirdparty/vlc/win-x64/lib/libvlccore.dll" -ItemType SymbolicLink -Force
New-Item -Path "$binDir/win-x64/plugins" -Value "../../../thirdparty/vlc/win-x64/lib/plugins" -ItemType SymbolicLink -Force

# Linux links
New-Item -Path "$binDir/linux-x64" -ItemType Directory -Force | Out-Null

New-Item -Path "$demoBin/linux-x64/libgodot_vlc.so" -Value "../../../target/release/libgodot_vlc.so" -ItemType SymbolicLink -Force
New-Item -Path "$demoBin/linux-x64/libgodot_vlc_debug.so" -Value "../../../target/debug/libgodot_vlc.so" -ItemType SymbolicLink -Force

New-Item -Path "$binDir/linux-x64/libvlc.so.12" -Value "../../../thirdparty/vlc/linux-x64/lib/libvlc.so.12" -ItemType SymbolicLink -Force
New-Item -Path "$binDir/linux-x64/libvlccore.so.9" -Value "../../../thirdparty/vlc/linux-x64/lib/libvlccore.so.9" -ItemType SymbolicLink -Force
New-Item -Path "$binDir/linux-x64/libidn.so.12" -Value "../../../thirdparty/vlc/linux-x64/lib/libidn.so.12" -ItemType SymbolicLink -Force
New-Item -Path "$binDir/linux-x64/vlc" -Value "../../../thirdparty/vlc/linux-x64/lib/vlc" -ItemType SymbolicLink -Force


New-Item -Path "demo/addons/godot-vlc" -Value "../../gdextension_template" -ItemType SymbolicLink -Force