#!/usr/bin/env python3
"""Bundle an already-built native executable; does not sign or notarize it."""
import plistlib
import shutil
import re
from pathlib import Path
root=Path(__file__).resolve().parents[1]
app=root/'target/release/bundle/macos/CongSpeak Native.app'
(app/'Contents/MacOS').mkdir(parents=True,exist_ok=True)
(app/'Contents/Resources').mkdir(exist_ok=True)
shutil.copy2(root/'target/release/congspeak-native',app/'Contents/MacOS/congspeak-native')
icon=root/'target/debug/bundle/macos/CongSpeak.app/Contents/Resources/icon.icns'
if icon.exists():shutil.copy2(icon,app/'Contents/Resources/icon.icns')
version=re.search(r'^version = "([^"]+)"', (root/'apps/native/Cargo.toml').read_text(), re.M)[1]
info={'CFBundleExecutable':'congspeak-native','CFBundleIdentifier':'app.congspeak.native','CFBundleName':'CongSpeak Native','CFBundleDisplayName':'CongSpeak Native','CFBundlePackageType':'APPL','CFBundleShortVersionString':version,'CFBundleVersion':version,'CFBundleIconFile':'icon.icns','LSMinimumSystemVersion':'14.2','NSHighResolutionCapable':True,'NSMicrophoneUsageDescription':'用于 TeamSpeak 语音通话和本地麦克风试听。'}
(app/'Contents/Info.plist').write_bytes(plistlib.dumps(info))
print(app)
