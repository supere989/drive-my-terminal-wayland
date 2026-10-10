#!/bin/bash
exec > /tmp/spike/inner.out 2>&1
export KWIN_SCREENSHOT_NO_PERMISSION_CHECKS=1
kwin_wayland --virtual --socket agent2 --width 1280 --height 800 --no-lockscreen --no-global-shortcuts > /tmp/spike/kwin.log 2>&1 &
KP=$!
for i in $(seq 1 20); do busctl --user list 2>/dev/null | grep -q "org.kde.KWin " && break; sleep 0.5; done
sleep 2
echo "--- EIS introspect:"; busctl --user introspect org.kde.KWin /org/kde/KWin/EIS/RemoteDesktop --no-pager 2>&1 | grep -v -E "^org.freedesktop|^\.(Get|Set|Ping|Introspect|Properties)"
WAYLAND_DISPLAY=agent2 QT_QPA_PLATFORM=wayland konsole -e bash -c 'echo hello-from-agent; sleep 40' > /tmp/spike/konsole.log 2>&1 &
sleep 4
echo "--- screenshot call:"
python3 - <<'PY'
import os, subprocess
r,w=os.pipe()
out=subprocess.run(["gdbus","call","--session","--dest","org.kde.KWin","--object-path","/org/kde/KWin/ScreenShot2","--method","org.kde.KWin.ScreenShot2.CaptureWorkspace","{}"],capture_output=True,text=True)
print(out.stdout[:300], out.stderr[:300])
PY
kill $KP
